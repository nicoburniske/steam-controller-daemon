use calloop::{
    EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
    channel::{self, Event as ChannelEvent},
    generic::Generic,
    ping::{Ping, make_ping},
};
use scd::ipc::{OskState, Request, Response};
use scd::{Error, Result, ResultExt};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, TrySendError};
use std::thread;

pub struct Server {
    path: PathBuf,
    osk: watch::WatchSender<OskState>,
    osk_current: OskState,
    osk_ping: Ping,
    events: channel::Sender<IpcEvent>,
    thread: Option<thread::JoinHandle<()>>,
}

pub struct ControlCommand {
    pub request: Request,
    client: u64,
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

enum IpcEvent {
    Response { client: u64, response: Response },
    Shutdown,
}

struct ClientConnection {
    stream: Rc<UnixStream>,
    registration: RegistrationToken,
    osk: bool,
}

struct IpcState {
    clients: HashMap<u64, ClientConnection>,
    next_client: u64,
    osk: Vec<u8>,
}

struct ClientReader {
    client: u64,
    request: String,
    commands: mpsc::SyncSender<ControlCommand>,
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
                state.osk = output;
                let osk = &state.osk;
                state.clients.retain(|_, client| {
                    if !client.osk || write_message(&client.stream, osk).is_ok() {
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
        let client = state.next_client;
        state.next_client = state
            .next_client
            .checked_add(1)
            .expect("IPC client ID overflow");
        let stream = Rc::new(stream);
        let mut reader = ClientReader {
            client,
            request: String::new(),
            commands: commands.clone(),
        };
        let registration = match handle.insert_source(
            Generic::new(stream.clone(), Interest::READ, Mode::Level),
            move |_, stream, state| Ok(reader.ready(stream, state)),
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
    fn ready(&mut self, stream: &UnixStream, state: &mut IpcState) -> PostAction {
        let remaining = 64 * 1024 + 1 - self.request.len();
        match BufReader::new(stream)
            .take(remaining as u64)
            .read_line(&mut self.request)
        {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => PostAction::Continue,
            Err(_) => self.reject(stream, state, "invalid request"),
            Ok(0) => self.close(state),
            Ok(_) if self.request.len() > 64 * 1024 => {
                self.reject(stream, state, "request is too large")
            }
            Ok(_) if !self.request.ends_with('\n') => self.close(state),
            Ok(_) => match serde_json::from_str::<Request>(&self.request) {
                Ok(Request::Osk) => {
                    state
                        .clients
                        .get_mut(&self.client)
                        .expect("registered IPC client exists")
                        .osk = true;
                    if write_message(stream, &state.osk).is_ok() {
                        PostAction::Continue
                    } else {
                        self.close(state)
                    }
                }
                Ok(request) => {
                    let command = ControlCommand {
                        request,
                        client: self.client,
                    };
                    match self.commands.try_send(command) {
                        Ok(()) => PostAction::Disable,
                        Err(TrySendError::Full(_)) => {
                            self.reject(stream, state, "daemon command queue is full")
                        }
                        Err(TrySendError::Disconnected(_)) => self.close(state),
                    }
                }
                Err(_) => self.reject(stream, state, "invalid request"),
            },
        }
    }

    fn reject(&self, stream: &UnixStream, state: &mut IpcState, message: &str) -> PostAction {
        if let Ok(output) = json_line(&Response::Error {
            message: message.into(),
        }) {
            let _ = write_message(stream, &output);
        }
        self.close(state)
    }

    fn close(&self, state: &mut IpcState) -> PostAction {
        state.clients.remove(&self.client);
        PostAction::Remove
    }
}

fn json_line(value: &impl Serialize) -> std::io::Result<Vec<u8>> {
    let mut output = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    output.push(b'\n');
    Ok(output)
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
    use scd::ipc::{Client, Status};
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
}
