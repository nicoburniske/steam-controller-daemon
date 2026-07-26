use crate::{Error, Result, ResultExt};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::thread;
use tokio::sync::broadcast;

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
}

pub type EventPublisher = broadcast::Sender<NamedEvent>;

pub struct ControlCommand {
    pub request: Request,
    pub reply: SyncSender<Response>,
}

pub struct Client {
    path: PathBuf,
}

pub struct EventStream {
    lines: std::io::Lines<BufReader<UnixStream>>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Request {
    Status,
    Mode,
    ModeSet { name: String },
    ModeNext,
    Reload,
    Events,
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

impl Server {
    pub fn bind(
        path: impl AsRef<Path>,
        commands: SyncSender<ControlCommand>,
    ) -> Result<(Self, EventPublisher)> {
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
        thread::Builder::new()
            .name("scd-ipc".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        break;
                    };
                    let commands = commands.clone();
                    let events = events.clone();
                    if let Err(error) = thread::Builder::new()
                        .name("scd-client".into())
                        .spawn(move || handle_client(stream, commands, events))
                    {
                        log::warn!("could not serve control client: {error}");
                    }
                }
            })
            .whence()?;

        Ok((Self { path }, publisher))
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

fn handle_client(stream: UnixStream, commands: SyncSender<ControlCommand>, events: EventPublisher) {
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
