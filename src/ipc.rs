use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

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

pub struct Server {
    path: PathBuf,
    thread: Option<thread::JoinHandle<()>>,
}

#[derive(Clone)]
pub struct EventPublisher {
    subscribers: Arc<Mutex<Vec<SyncSender<NamedEvent>>>>,
}

pub struct ControlCommand {
    pub request: ControlRequest,
    pub reply: SyncSender<Result<ControlReply, String>>,
}

pub enum ControlRequest {
    Status,
    Mode,
    SetMode(String),
    NextMode,
    Reload,
}

pub enum ControlReply {
    Status(Status),
    Mode(String),
    Done,
}

pub struct Client {
    path: PathBuf,
}

pub struct EventStream {
    lines: std::io::Lines<BufReader<UnixStream>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("control socket error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid control response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon rejected request: {0}")]
    Daemon(String),
    #[error("unexpected daemon response")]
    UnexpectedResponse,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
enum Request {
    Status,
    Mode,
    ModeSet { name: String },
    ModeNext,
    Reload,
    Events,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Response {
    Status { status: Status },
    Mode { name: String },
    Done,
    Event { event: NamedEvent },
    Error { message: String },
}

impl Server {
    pub fn bind(
        path: impl AsRef<Path>,
        commands: SyncSender<ControlCommand>,
    ) -> std::io::Result<(Self, EventPublisher)> {
        let path = path.as_ref().to_path_buf();
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660))?;
        let subscribers = Arc::new(Mutex::new(Vec::new()));
        let publisher = EventPublisher {
            subscribers: subscribers.clone(),
        };
        let thread = thread::Builder::new()
            .name("scd-ipc".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        break;
                    };
                    let commands = commands.clone();
                    let subscribers = subscribers.clone();
                    if let Err(error) = thread::Builder::new()
                        .name("scd-client".into())
                        .spawn(move || handle_client(stream, commands, subscribers))
                    {
                        log::warn!("could not serve control client: {error}");
                    }
                }
            })?;

        Ok((
            Self {
                path,
                thread: Some(thread),
            },
            publisher,
        ))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.thread.take();
        let _ = fs::remove_file(&self.path);
    }
}

impl EventPublisher {
    pub fn publish(&self, event: NamedEvent) {
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|subscriber| subscriber.try_send(event.clone()).is_ok());
    }
}

impl Client {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn status(&self) -> Result<Status, ClientError> {
        match self.request(Request::Status)? {
            Response::Status { status } => Ok(status),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn mode(&self) -> Result<String, ClientError> {
        match self.request(Request::Mode)? {
            Response::Mode { name } => Ok(name),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn set_mode(&self, name: String) -> Result<(), ClientError> {
        match self.request(Request::ModeSet { name })? {
            Response::Done => Ok(()),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn next_mode(&self) -> Result<(), ClientError> {
        match self.request(Request::ModeNext)? {
            Response::Done => Ok(()),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn reload(&self) -> Result<(), ClientError> {
        match self.request(Request::Reload)? {
            Response::Done => Ok(()),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn events(&self) -> Result<EventStream, ClientError> {
        let stream = UnixStream::connect(&self.path)?;
        serde_json::to_writer(&stream, &Request::Events)?;
        (&stream).write_all(b"\n")?;
        Ok(EventStream {
            lines: BufReader::new(stream).lines(),
        })
    }

    fn request(&self, request: Request) -> Result<Response, ClientError> {
        let stream = UnixStream::connect(&self.path)?;
        serde_json::to_writer(&stream, &request)?;
        (&stream).write_all(b"\n")?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response)?;
        match serde_json::from_str(&response)? {
            Response::Error { message } => Err(ClientError::Daemon(message)),
            response => Ok(response),
        }
    }
}

impl Iterator for EventStream {
    type Item = Result<NamedEvent, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.lines.next().map(|line| {
            let response: Response = serde_json::from_str(&line?)?;
            match response {
                Response::Event { event } => Ok(event),
                Response::Error { message } => Err(ClientError::Daemon(message)),
                _ => Err(ClientError::UnexpectedResponse),
            }
        })
    }
}

fn handle_client(
    stream: UnixStream,
    commands: SyncSender<ControlCommand>,
    subscribers: Arc<Mutex<Vec<SyncSender<NamedEvent>>>>,
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
        let (sender, receiver) = mpsc::sync_channel(32);
        subscribers.lock().unwrap().push(sender);
        let mut writer = BufWriter::new(stream);
        for event in receiver {
            if serde_json::to_writer(&mut writer, &Response::Event { event }).is_err()
                || writer.write_all(b"\n").is_err()
                || writer.flush().is_err()
            {
                break;
            }
        }
        return;
    }

    let control_request = match request {
        Request::Status => ControlRequest::Status,
        Request::Mode => ControlRequest::Mode,
        Request::ModeSet { name } => ControlRequest::SetMode(name),
        Request::ModeNext => ControlRequest::NextMode,
        Request::Reload => ControlRequest::Reload,
        Request::Events => unreachable!(),
    };
    let (reply, receiver) = mpsc::sync_channel(1);
    if commands
        .send(ControlCommand {
            request: control_request,
            reply,
        })
        .is_err()
    {
        return;
    }
    let response = match receiver.recv() {
        Ok(Ok(ControlReply::Status(status))) => Response::Status { status },
        Ok(Ok(ControlReply::Mode(name))) => Response::Mode { name },
        Ok(Ok(ControlReply::Done)) => Response::Done,
        Ok(Err(message)) => Response::Error { message },
        Err(_) => return,
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
    fn wire_format_is_stable() {
        assert_eq!(
            serde_json::to_string(&Request::ModeSet {
                name: "couch".into()
            })
            .unwrap(),
            r#"{"command":"mode-set","name":"couch"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Event {
                event: NamedEvent {
                    name: "keyboard.toggle".into()
                }
            })
            .unwrap(),
            r#"{"type":"event","event":{"name":"keyboard.toggle"}}"#
        );
    }
}
