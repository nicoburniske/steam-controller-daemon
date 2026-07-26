use crate::protocol::Button;
use crate::{Error, Result, ResultExt};
use evdev::KeyCode;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::thread;
use tokio::sync::{broadcast, watch};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub connected: bool,
    pub mode: String,
    pub battery_percent: Option<u8>,
    pub charging: Option<bool>,
    pub device: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamedEvent {
    pub name: String,
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
const OSK_PAD_VISIBLE_LIMIT: f32 = 5.0 / 6.0;

pub struct Server {
    path: PathBuf,
}

pub type EventPublisher = broadcast::Sender<NamedEvent>;
pub type OskPublisher = watch::Sender<OskState>;

pub struct ControlCommand {
    pub request: Request,
    pub reply: SyncSender<Response>,
}

#[derive(Clone)]
pub struct Client {
    path: PathBuf,
}

pub struct EventStream {
    lines: std::io::Lines<BufReader<UnixStream>>,
}

pub struct OskStream {
    lines: std::io::Lines<BufReader<UnixStream>>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Request {
    Status,
    Mode,
    ModeSet {
        name: String,
    },
    ModeNext,
    Reload,
    Events,
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
    Mode { name: String },
    Done,
    Event { event: NamedEvent },
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
        pad.position[0] = match side {
            OskPadSide::Left => pad.position[0].clamp(-OSK_PAD_VISIBLE_LIMIT, 1.0),
            OskPadSide::Right => pad.position[0].clamp(-1.0, OSK_PAD_VISIBLE_LIMIT),
        };
        pad.position[1] = pad.position[1].clamp(-OSK_PAD_VISIBLE_LIMIT, OSK_PAD_VISIBLE_LIMIT);
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
    pub fn bind(
        path: impl AsRef<Path>,
        commands: SyncSender<ControlCommand>,
    ) -> Result<(Self, EventPublisher, OskPublisher)> {
        let path = path.as_ref().to_path_buf();
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::message(error)),
        }

        let listener = UnixListener::bind(&path).whence()?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).whence()?;
        let (events, _) = broadcast::channel(32);
        let publisher = events.clone();
        let (osk, _) = watch::channel(OskState::default());
        let osk_publisher = osk.clone();
        thread::Builder::new()
            .name("scd-ipc".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        break;
                    };
                    let commands = commands.clone();
                    let events = events.clone();
                    let osk = osk.clone();
                    if let Err(error) = thread::Builder::new()
                        .name("scd-client".into())
                        .spawn(move || handle_client(stream, commands, events, osk))
                    {
                        log::warn!("could not serve control client: {error}");
                    }
                }
            })
            .whence()?;

        Ok((Self { path }, publisher, osk_publisher))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
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

    pub fn mode(&self) -> Result<String> {
        match self.request(Request::Mode)? {
            Response::Mode { name } => Ok(name),
            _ => Err(Error::message("unexpected daemon response")),
        }
    }

    pub fn set_mode(&self, name: String) -> Result<()> {
        match self.request(Request::ModeSet { name })? {
            Response::Done => Ok(()),
            _ => Err(Error::message("unexpected daemon response")),
        }
    }

    pub fn next_mode(&self) -> Result<()> {
        match self.request(Request::ModeNext)? {
            Response::Done => Ok(()),
            _ => Err(Error::message("unexpected daemon response")),
        }
    }

    pub fn reload(&self) -> Result<()> {
        match self.request(Request::Reload)? {
            Response::Done => Ok(()),
            _ => Err(Error::message("unexpected daemon response")),
        }
    }

    pub fn events(&self) -> Result<EventStream> {
        let stream = UnixStream::connect(&self.path).whence()?;
        serde_json::to_writer(&stream, &Request::Events).whence()?;
        (&stream).write_all(b"\n").whence()?;
        Ok(EventStream {
            lines: BufReader::new(stream).lines(),
        })
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
        match self.request(Request::Key {
            code,
            shift,
            session,
        })? {
            Response::Done => Ok(()),
            _ => Err(Error::message("unexpected daemon response")),
        }
    }

    pub fn hide_osk(&self, session: u64) -> Result<()> {
        match self.request(Request::OskHide { session })? {
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

impl Iterator for EventStream {
    type Item = Result<NamedEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        self.lines.next().map(|line| {
            let line = line.whence()?;
            let response: Response = serde_json::from_str(&line).whence()?;
            match response {
                Response::Event { event } => Ok(event),
                Response::Error { message } => Err(Error::message(message)),
                _ => Err(Error::message("unexpected daemon response")),
            }
        })
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

fn handle_client(
    stream: UnixStream,
    commands: SyncSender<ControlCommand>,
    events: EventPublisher,
    osk: OskPublisher,
) {
    let mut request = String::new();
    if BufReader::new(&stream).read_line(&mut request).is_err() {
        return;
    }
    let Ok(request) = serde_json::from_str::<Request>(&request) else {
        let _ = serde_json::to_writer(
            &stream,
            &Response::Error {
                message: "invalid request".into(),
            },
        );
        return;
    };

    if matches!(request, Request::Events) {
        let mut receiver = events.subscribe();
        drop(events);
        let mut writer = BufWriter::new(stream);
        loop {
            let event = match receiver.blocking_recv() {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    log::warn!("control event client missed {count} events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if serde_json::to_writer(&mut writer, &Response::Event { event }).is_err()
                || writer.write_all(b"\n").is_err()
                || writer.flush().is_err()
            {
                break;
            }
        }
        return;
    }

    if matches!(request, Request::Osk) {
        let mut receiver = osk.subscribe();
        drop(osk);
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
            return;
        };
        let mut writer = BufWriter::new(stream);
        loop {
            let state = *receiver.borrow_and_update();
            if serde_json::to_writer(&mut writer, &state).is_err()
                || writer.write_all(b"\n").is_err()
                || writer.flush().is_err()
            {
                break;
            }
            if runtime.block_on(receiver.changed()).is_err() {
                break;
            }
        }
        return;
    }

    let (reply, receiver) = mpsc::sync_channel(1);
    if commands.send(ControlCommand { request, reply }).is_err() {
        return;
    }
    let Ok(response) = receiver.recv() else {
        return;
    };
    let mut writer = BufWriter::new(stream);
    let _ = serde_json::to_writer(&mut writer, &response);
    let _ = writer.write_all(b"\n");
    let _ = writer.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_history_wraps_in_order() {
        let mut state = OskState {
            click_sequence: u64::MAX - 1,
            ..Default::default()
        };
        state.record_click(OskPadSide::Left, [1.0, 0.0]);
        state.record_click(OskPadSide::Right, [2.0, 0.0]);
        state.record_click(OskPadSide::Left, [3.0, 0.0]);

        let mut clicks = state.clicks_since(u64::MAX - 1);
        assert_eq!(clicks.missed(), 0);
        assert_eq!(
            clicks.next().map(|click| click.sequence),
            Some(u64::MAX - 1)
        );
        assert_eq!(clicks.next().map(|click| click.sequence), Some(u64::MAX));
        assert_eq!(clicks.next().map(|click| click.sequence), Some(0));
        assert_eq!(clicks.next(), None);
        assert_eq!(state.click_cursor(), 1);
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
        assert_eq!(
            state.left.position,
            [-OSK_PAD_VISIBLE_LIMIT, OSK_PAD_VISIBLE_LIMIT]
        );

        pad.position = [1.0, -1.0];
        state.update_pad(OskPadSide::Right, pad, false);
        assert_eq!(
            state.right.position,
            [OSK_PAD_VISIBLE_LIMIT, -OSK_PAD_VISIBLE_LIMIT]
        );
    }
}
