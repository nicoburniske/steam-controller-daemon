use crate::protocol::Button;
use crate::{Error, Result, ResultExt};
use calloop::{
    EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
    channel::{self, Event as ChannelEvent},
    generic::Generic,
    ping::{Ping, make_ping},
};
use evdev::KeyCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, TrySendError};
use std::thread;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub connected: bool,
    pub mode: String,
    pub battery_percent: Option<u8>,
    pub charging: Option<bool>,
    pub device: Option<String>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum HapticSound {
    ControllerOn,
    ControllerOff,
    UpFive,
    DownFive,
    UpSix,
    DownSix,
    WhoopUpThree,
    WhoopDown,
    Pulse,
    ToneLow,
    ToneHigh,
    SweepUp,
    SweepDown,
    TrillUp,
    TrillDown,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct OskState {
    pub visible: bool,
    #[serde(default)]
    pub shift_held: bool,
    #[serde(default)]
    bindings: OskBindings,
    #[serde(default)]
    active_bindings: u32,
    pub left: OskPad,
    pub right: OskPad,
    session: u64,
    click_sequence: u64,
    click_history_len: u8,
    click_history: [OskClick; OSK_CLICK_HISTORY_LEN],
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OskBindings([u16; Button::ALL.len()]);

impl OskBindings {
    pub fn get(&self, input: Button) -> Option<KeyCode> {
        let code = self.0[input.index()];
        (code != 0).then(|| KeyCode::new(code))
    }

    pub fn iter(&self) -> impl Iterator<Item = (Button, KeyCode)> + '_ {
        Button::ALL
            .into_iter()
            .filter_map(|input| self.get(input).map(|key| (input, key)))
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct OskPad {
    pub touched: bool,
    pub pressed: bool,
    pub position: [f32; 2],
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct OskClick {
    pub sequence: u64,
    pub pad: OskPadSide,
    pub position: [f32; 2],
    #[serde(default)]
    pub shift_held: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OskPadSide {
    #[default]
    Left,
    Right,
}

pub struct OskClicks<'a> {
    state: &'a OskState,
    sequence: u64,
    remaining: u8,
    missed: u64,
}

const OSK_CLICK_HISTORY_LEN: usize = 16;
const OSK_PAD_MIN_RESPONSE: f32 = 0.15;
const OSK_PAD_FULL_RESPONSE_DISTANCE: f32 = 0.02;
pub const OSK_PAD_LIMIT: f32 = 5.0 / 6.0;

pub struct Server {
    path: PathBuf,
    osk: OskPublisher,
    osk_current: OskState,
    osk_ping: Ping,
    events: channel::Sender<IpcEvent>,
    thread: Option<thread::JoinHandle<()>>,
}

type OskPublisher = watch::WatchSender<OskState>;

pub struct ControlCommand {
    pub request: Request,
    client: u64,
}

enum IpcEvent {
    Response { client: u64, response: Response },
    Shutdown,
}

struct ClientConnection {
    stream: Arc<UnixStream>,
    registration: RegistrationToken,
    osk: bool,
}

struct IpcState {
    clients: HashMap<u64, ClientConnection>,
    next_client: u64,
    osk: Arc<[u8]>,
}

struct ClientReader {
    client: u64,
    reader: BufReader<UnixStream>,
    request: String,
    received: bool,
    commands: mpsc::SyncSender<ControlCommand>,
}

#[derive(Clone)]
pub struct Client {
    path: PathBuf,
}

pub struct OskStream {
    lines: std::io::Lines<BufReader<UnixStream>>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Request {
    Status,
    ModeSet {
        name: String,
    },
    ModeNext,
    Sound {
        sound: HapticSound,
    },
    Reload,
    Osk,
    OskHide {
        session: u64,
    },
    Key {
        code: u16,
        shift: bool,
        session: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Response {
    Status { status: Status },
    Done,
    Error { message: String },
}

impl OskState {
    pub fn set_visible(&mut self, visible: bool) {
        if visible && !self.visible {
            self.session = self.session.wrapping_add(1);
            if self.session == 0 {
                self.session = 1;
            }
            self.click_history_len = 0;
        }
        self.visible = visible;
        self.shift_held = false;
        self.active_bindings = 0;
        self.left.touched = false;
        self.left.pressed = false;
        self.right.touched = false;
        self.right.pressed = false;
    }

    pub fn session(&self) -> u64 {
        self.session
    }

    pub fn bindings(&self) -> OskBindings {
        self.bindings
    }

    pub fn active_bindings(&self) -> u32 {
        self.active_bindings
    }

    pub fn set_bindings(&mut self, bindings: impl IntoIterator<Item = (Button, KeyCode)>) {
        self.bindings.0.fill(0);
        let mut configured = 0;
        for (input, key) in bindings {
            self.bindings.0[input.index()] = key.code();
            configured |= input.mask();
        }
        self.active_bindings &= configured;
    }

    pub fn set_active_bindings(&mut self, bindings: impl IntoIterator<Item = Button>) {
        self.active_bindings = bindings
            .into_iter()
            .fold(0, |active, input| active | input.mask());
    }

    pub fn update_pad(&mut self, side: OskPadSide, mut pad: OskPad, record_rising_edge: bool) {
        pad.position[0] = pad.position[0].clamp(-OSK_PAD_LIMIT, OSK_PAD_LIMIT);
        pad.position[1] = pad.position[1].clamp(-OSK_PAD_LIMIT, OSK_PAD_LIMIT);
        let clicked = {
            let previous = match side {
                OskPadSide::Left => &mut self.left,
                OskPadSide::Right => &mut self.right,
            };
            if pad.touched && previous.touched {
                for (current, previous) in pad.position.iter_mut().zip(previous.position) {
                    let response = ((*current - previous).abs() / OSK_PAD_FULL_RESPONSE_DISTANCE)
                        .clamp(OSK_PAD_MIN_RESPONSE, 1.0);
                    *current = previous + response * (*current - previous);
                }
            }
            let clicked = record_rising_edge && pad.pressed && !previous.pressed;
            *previous = pad;
            clicked
        };
        if clicked {
            self.record_click(side, pad.position);
        }
    }

    pub fn record_click(&mut self, pad: OskPadSide, position: [f32; 2]) {
        let sequence = self.click_sequence;
        self.click_history[sequence as usize % OSK_CLICK_HISTORY_LEN] = OskClick {
            sequence,
            pad,
            position,
            shift_held: self.shift_held,
        };
        self.click_sequence = sequence.wrapping_add(1);
        self.click_history_len = self
            .click_history_len
            .saturating_add(1)
            .min(OSK_CLICK_HISTORY_LEN as u8);
    }

    pub fn click_cursor(&self) -> u64 {
        self.click_sequence
    }

    pub fn clicks_since(&self, cursor: u64) -> OskClicks<'_> {
        let available = u64::from(self.click_history_len.min(OSK_CLICK_HISTORY_LEN as u8));
        let requested = self.click_sequence.wrapping_sub(cursor);
        let missed = requested.saturating_sub(available);
        OskClicks {
            state: self,
            sequence: cursor.wrapping_add(missed),
            remaining: requested.min(available) as u8,
            missed,
        }
    }
}

impl OskClicks<'_> {
    pub fn missed(&self) -> u64 {
        self.missed
    }
}

impl Iterator for OskClicks<'_> {
    type Item = OskClick;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let click = self.state.click_history[self.sequence as usize % OSK_CLICK_HISTORY_LEN];
        debug_assert_eq!(click.sequence, self.sequence);
        self.sequence = self.sequence.wrapping_add(1);
        self.remaining -= 1;
        Some(click)
    }
}

impl Server {
    pub fn bind(path: impl AsRef<Path>) -> Result<(Self, mpsc::Receiver<ControlCommand>)> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).whence()?;
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::message(error)),
        }

        let listener = UnixListener::bind(&path).whence()?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).whence()?;
        listener.set_nonblocking(true).whence()?;

        let (osk, osk_receiver) = watch::channel(OskState::default());
        let (osk_ping, osk_source) = make_ping().whence()?;
        let (events, event_source) = channel::channel();
        let (commands, receiver) = mpsc::sync_channel(32);
        let (started, startup) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("scd-ipc".into())
            .spawn(move || {
                if let Err(error) = run_ipc(
                    listener,
                    commands,
                    event_source,
                    osk_receiver,
                    osk_source,
                    started.clone(),
                ) {
                    log::warn!("IPC event loop failed: {error}");
                    let _ = started.try_send(Err(error));
                }
            })
            .whence()?;

        if let Err(error) = startup.recv().whence().and_then(|result| result) {
            let _ = thread.join();
            let _ = fs::remove_file(&path);
            return Err(error);
        }

        Ok((
            Self {
                path,
                osk,
                osk_current: OskState::default(),
                osk_ping,
                events,
                thread: Some(thread),
            },
            receiver,
        ))
    }

    pub fn respond(&self, command: ControlCommand, response: Response) {
        let _ = self.events.send(IpcEvent::Response {
            client: command.client,
            response,
        });
    }

    pub fn publish_osk(&mut self, next: OskState) {
        if self.osk_current != next {
            self.osk_current = next;
            self.osk.send(next);
            self.osk_ping.ping();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.events.send(IpcEvent::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

impl Client {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn status(&self) -> Result<Status> {
        match self.request(Request::Status)? {
            Response::Status { status } => Ok(status),
            _ => Err(Error::message("unexpected daemon response")),
        }
    }

    pub fn set_mode(&self, name: String) -> Result<()> {
        self.request_done(Request::ModeSet { name })
    }

    pub fn next_mode(&self) -> Result<()> {
        self.request_done(Request::ModeNext)
    }

    pub fn play_sound(&self, sound: HapticSound) -> Result<()> {
        self.request_done(Request::Sound { sound })
    }

    pub fn reload(&self) -> Result<()> {
        self.request_done(Request::Reload)
    }

    pub fn osk(&self) -> Result<OskStream> {
        let stream = UnixStream::connect(&self.path).whence()?;
        serde_json::to_writer(&stream, &Request::Osk).whence()?;
        (&stream).write_all(b"\n").whence()?;
        Ok(OskStream {
            lines: BufReader::new(stream).lines(),
        })
    }

    pub fn key(&self, code: u16, shift: bool, session: u64) -> Result<()> {
        self.request_done(Request::Key {
            code,
            shift,
            session,
        })
    }

    pub fn hide_osk(&self, session: u64) -> Result<()> {
        self.request_done(Request::OskHide { session })
    }

    fn request_done(&self, request: Request) -> Result<()> {
        match self.request(request)? {
            Response::Done => Ok(()),
            _ => Err(Error::message("unexpected daemon response")),
        }
    }

    fn request(&self, request: Request) -> Result<Response> {
        let stream = UnixStream::connect(&self.path).whence()?;
        serde_json::to_writer(&stream, &request).whence()?;
        (&stream).write_all(b"\n").whence()?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).whence()?;
        match serde_json::from_str(&response).whence()? {
            Response::Error { message } => Err(Error::message(message)),
            response => Ok(response),
        }
    }
}

impl Iterator for OskStream {
    type Item = Result<OskState>;

    fn next(&mut self) -> Option<Self::Item> {
        self.lines.next().map(|line| {
            let line = line.whence()?;
            serde_json::from_str(&line).whence()
        })
    }
}

fn run_ipc(
    listener: UnixListener,
    commands: mpsc::SyncSender<ControlCommand>,
    event_source: channel::Channel<IpcEvent>,
    mut osk_receiver: watch::WatchReceiver<OskState>,
    osk_source: calloop::ping::PingSource,
    started: mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let mut event_loop = EventLoop::<IpcState>::try_new().whence()?;
    let handle = event_loop.handle();
    let signal = event_loop.get_signal();

    handle
        .insert_source(event_source, {
            let handle = handle.clone();
            move |event, _, state| match event {
                ChannelEvent::Msg(IpcEvent::Response { client, response }) => {
                    if let Some(client) = state.clients.remove(&client) {
                        if let Ok(output) = json_line(&response) {
                            let _ = write_message(&client.stream, &output);
                        }
                        handle.remove(client.registration);
                    }
                }
                ChannelEvent::Msg(IpcEvent::Shutdown) | ChannelEvent::Closed => signal.stop(),
            }
        })
        .whence()?;

    handle
        .insert_source(osk_source, {
            let handle = handle.clone();
            move |(), _, state| {
                let Some(next) = osk_receiver.get_if_new() else {
                    return;
                };
                let Ok(output) = json_line(&next) else {
                    log::warn!("could not encode OSK state");
                    return;
                };
                state.osk = output.clone();
                state.clients.retain(|_, client| {
                    if !client.osk || write_message(&client.stream, &output).is_ok() {
                        true
                    } else {
                        handle.remove(client.registration);
                        false
                    }
                });
            }
        })
        .whence()?;

    handle
        .insert_source(Generic::new(listener, Interest::READ, Mode::Level), {
            let handle = handle.clone();
            move |_, listener, state| {
                accept_clients(listener, &handle, &commands, state);
                Ok(PostAction::Continue)
            }
        })
        .whence()?;

    let mut state = IpcState {
        clients: HashMap::new(),
        next_client: 1,
        osk: json_line(&OskState::default()).whence()?,
    };
    started.send(Ok(())).whence()?;
    event_loop.run(None, &mut state, |_| {}).whence()
}

fn accept_clients<'a>(
    listener: &UnixListener,
    handle: &LoopHandle<'a, IpcState>,
    commands: &mpsc::SyncSender<ControlCommand>,
    state: &mut IpcState,
) {
    loop {
        let (stream, _) = match listener.accept() {
            Ok(client) => client,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => {
                log::warn!("could not accept IPC client: {error}");
                break;
            }
        };
        if let Err(error) = stream.set_nonblocking(true) {
            log::warn!("could not configure IPC client: {error}");
            continue;
        }
        let reader = match stream.try_clone() {
            Ok(reader) => reader,
            Err(error) => {
                log::warn!("could not clone IPC client: {error}");
                continue;
            }
        };

        let client = loop {
            let client = state.next_client;
            state.next_client = state.next_client.wrapping_add(1).max(1);
            if !state.clients.contains_key(&client) {
                break client;
            }
        };
        let stream = Arc::new(stream);
        let mut reader = ClientReader {
            client,
            reader: BufReader::new(reader),
            request: String::new(),
            received: false,
            commands: commands.clone(),
        };
        let registration = match handle.insert_source(
            Generic::new(stream.clone(), Interest::READ, Mode::Level),
            move |_, _, state| reader.ready(state),
        ) {
            Ok(registration) => registration,
            Err(error) => {
                log::warn!("could not register IPC client: {}", error.error);
                continue;
            }
        };
        state.clients.insert(
            client,
            ClientConnection {
                stream,
                registration,
                osk: false,
            },
        );
    }
}

impl ClientReader {
    fn ready(&mut self, state: &mut IpcState) -> std::io::Result<PostAction> {
        if self.received {
            let mut buffer = [0; 4096];
            return Ok(match self.reader.read(&mut buffer) {
                Ok(0) => self.close(state),
                Ok(_) => PostAction::Continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    PostAction::Continue
                }
                Err(_) => self.close(state),
            });
        }

        let remaining = 64 * 1024 + 1 - self.request.len();
        let read = self
            .reader
            .by_ref()
            .take(remaining as u64)
            .read_line(&mut self.request);
        if self.request.len() > 64 * 1024 {
            self.error("request is too large");
            return Ok(self.close(state));
        }
        match read {
            Ok(0) => return Ok(self.close(state)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(PostAction::Continue);
            }
            Err(_) => {
                self.error("invalid request");
                return Ok(self.close(state));
            }
            Ok(_) if !self.request.ends_with('\n') => return Ok(self.close(state)),
            Ok(_) => {}
        }

        self.received = true;
        match serde_json::from_str::<Request>(&self.request) {
            Ok(Request::Osk) => {
                let Some(client) = state.clients.get_mut(&self.client) else {
                    return Ok(PostAction::Remove);
                };
                client.osk = true;
                if write_message(self.reader.get_ref(), &state.osk).is_ok() {
                    return Ok(PostAction::Continue);
                }
            }
            Ok(request) => {
                let command = ControlCommand {
                    request,
                    client: self.client,
                };
                match self.commands.try_send(command) {
                    Ok(()) => return Ok(PostAction::Continue),
                    Err(TrySendError::Full(_)) => {
                        self.error("daemon command queue is full");
                    }
                    Err(TrySendError::Disconnected(_)) => {}
                }
            }
            Err(_) => self.error("invalid request"),
        }
        Ok(self.close(state))
    }

    fn error(&self, message: &str) {
        if let Ok(output) = json_line(&Response::Error {
            message: message.into(),
        }) {
            let _ = write_message(self.reader.get_ref(), &output);
        }
    }

    fn close(&self, state: &mut IpcState) -> PostAction {
        state.clients.remove(&self.client);
        PostAction::Remove
    }
}
fn json_line(value: &impl Serialize) -> std::io::Result<Arc<[u8]>> {
    let mut output = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    output.push(b'\n');
    Ok(output.into())
}

fn write_message(stream: &UnixStream, message: &[u8]) -> std::io::Result<()> {
    let mut stream = stream;
    if stream.write(message)? == message.len() {
        Ok(())
    } else {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    #[test]
    fn stalled_connection_does_not_block_other_clients() {
        static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

        let path = std::env::temp_dir().join(format!(
            "scd-ipc-{}-{}.sock",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        let (mut server, commands) = Server::bind(&path).unwrap();
        let stalled = UnixStream::connect(&path).unwrap();

        let osk = UnixStream::connect(&path).unwrap();
        osk.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        serde_json::to_writer(&osk, &Request::Osk).unwrap();
        (&osk).write_all(b"\n").unwrap();
        let mut osk = BufReader::new(osk);
        let mut line = String::new();
        osk.read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<OskState>(&line).unwrap(),
            OskState::default()
        );

        let mut state = OskState::default();
        state.set_visible(true);
        server.publish_osk(state);
        line.clear();
        osk.read_line(&mut line).unwrap();
        assert_eq!(serde_json::from_str::<OskState>(&line).unwrap(), state);

        let client = Client::new(&path);
        let status_client = thread::spawn(move || client.status());
        let command = commands.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(command.request, Request::Status));
        let status = Status {
            connected: true,
            mode: "desktop".into(),
            battery_percent: Some(75),
            charging: Some(false),
            device: Some("test controller".into()),
        };
        server.respond(
            command,
            Response::Status {
                status: status.clone(),
            },
        );
        assert_eq!(status_client.join().unwrap().unwrap(), status);

        drop(stalled);
    }

    #[test]
    fn osk_bindings_replace_previous_keys() {
        let mut state = OskState::default();
        state.set_bindings([
            (Button::L4, KeyCode::KEY_LEFTMETA),
            (Button::X, KeyCode::KEY_BACKSPACE),
        ]);
        assert_eq!(
            state.bindings().get(Button::L4),
            Some(KeyCode::KEY_LEFTMETA)
        );
        assert_eq!(
            state.bindings().get(Button::X),
            Some(KeyCode::KEY_BACKSPACE)
        );
        state.set_active_bindings([Button::L4, Button::X]);
        assert_ne!(state.active_bindings() & Button::L4.mask(), 0);
        assert_ne!(state.active_bindings() & Button::X.mask(), 0);

        state.set_bindings([(Button::L4, KeyCode::KEY_LEFTCTRL)]);
        assert_eq!(
            state.bindings().get(Button::L4),
            Some(KeyCode::KEY_LEFTCTRL)
        );
        assert_eq!(state.bindings().get(Button::X), None);
        assert_ne!(state.active_bindings() & Button::L4.mask(), 0);
        assert_eq!(state.active_bindings() & Button::X.mask(), 0);
    }

    #[test]
    fn first_touch_is_exact_and_clicks_use_the_smoothed_position() {
        let mut state = OskState::default();
        state.update_pad(
            OskPadSide::Left,
            OskPad {
                touched: true,
                pressed: true,
                position: [0.0, 0.0],
            },
            false,
        );
        assert_eq!(state.left.position, [0.0, 0.0]);
        assert_eq!(state.click_cursor(), 0);

        state.update_pad(
            OskPadSide::Left,
            OskPad {
                touched: true,
                pressed: false,
                position: [0.0, 0.0],
            },
            true,
        );
        state.update_pad(
            OskPadSide::Left,
            OskPad {
                touched: true,
                pressed: true,
                position: [0.005, 0.0],
            },
            true,
        );

        let click = state.clicks_since(0).next().unwrap();
        assert_eq!(click.pad, OskPadSide::Left);
        assert_eq!(click.position, [0.00125, 0.0]);
        assert_eq!(state.left.position, click.position);
        assert_eq!(state.click_cursor(), 1);

        state.update_pad(
            OskPadSide::Left,
            OskPad {
                touched: true,
                pressed: false,
                position: [0.5, 0.5],
            },
            true,
        );
        assert_eq!(state.left.position, [0.5, 0.5]);

        state.update_pad(
            OskPadSide::Left,
            OskPad {
                touched: false,
                pressed: false,
                position: [0.0, 0.0],
            },
            true,
        );
        state.update_pad(
            OskPadSide::Left,
            OskPad {
                touched: true,
                pressed: false,
                position: [0.75, -0.75],
            },
            true,
        );
        assert_eq!(state.left.position, [0.75, -0.75]);
    }

    #[test]
    fn pad_edges_saturate_before_per_axis_filtering() {
        let mut state = OskState::default();
        let mut pad = OskPad {
            touched: true,
            ..Default::default()
        };
        state.update_pad(OskPadSide::Left, pad, false);
        pad.position = [0.005, 0.02];
        state.update_pad(OskPadSide::Left, pad, false);
        assert_eq!(state.left.position, [0.00125, 0.02]);

        pad.position = [-1.0, 1.0];
        state.update_pad(OskPadSide::Left, pad, false);
        assert_eq!(state.left.position, [-OSK_PAD_LIMIT, OSK_PAD_LIMIT]);

        pad.position = [1.0, -1.0];
        state.update_pad(OskPadSide::Right, pad, false);
        assert_eq!(state.right.position, [OSK_PAD_LIMIT, -OSK_PAD_LIMIT]);

        pad.position = [1.0, 0.0];
        state.update_pad(OskPadSide::Left, pad, false);
        assert_eq!(state.left.position[0], OSK_PAD_LIMIT);

        pad.position = [-1.0, 0.0];
        state.update_pad(OskPadSide::Right, pad, false);
        assert_eq!(state.right.position[0], -OSK_PAD_LIMIT);
    }
}
