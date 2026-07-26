use crate::config::{Config, ConfigError};
use crate::device::{DeviceError, DeviceEvent, DeviceManager};
use crate::ipc::{
    ControlCommand, ControlReply, ControlRequest, EventPublisher, NamedEvent, Server, Status,
};
use crate::mapper::{Mapper, MapperError, Output};
use crate::output::{OutputError, Outputs};
use crate::protocol::Report;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub struct Daemon {
    config_path: PathBuf,
    socket_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Mapper(#[from] MapperError),
    #[error(transparent)]
    Device(#[from] DeviceError),
    #[error(transparent)]
    Output(#[from] OutputError),
    #[error("could not start control socket: {0}")]
    Socket(#[source] std::io::Error),
}

impl Daemon {
    pub fn new(config_path: impl AsRef<Path>, socket_path: impl AsRef<Path>) -> Self {
        Self {
            config_path: config_path.as_ref().to_path_buf(),
            socket_path: socket_path.as_ref().to_path_buf(),
        }
    }

    pub fn run(self) -> Result<(), DaemonError> {
        let config = Config::load(&self.config_path)?;
        let mut mapper = Mapper::new(config)?;
        let mut outputs = Outputs::new()?;
        let mut device = DeviceManager::new()?;
        let (command_sender, commands) = mpsc::sync_channel(32);
        let (_server, events) =
            Server::bind(&self.socket_path, command_sender).map_err(DaemonError::Socket)?;
        let mut battery = None;
        let mut charging = None;
        let mut last_lizard_refresh = Instant::now() - Duration::from_secs(4);
        let mut rumble = (0, 0);
        let mut last_rumble = Instant::now() - Duration::from_millis(50);

        log::info!("active mode: {}", mapper.active_mode());
        loop {
            while let Ok(command) = commands.try_recv() {
                self.handle_control(
                    command,
                    &mut mapper,
                    &mut outputs,
                    &events,
                    &device,
                    battery,
                    charging,
                )?;
            }

            let mut received = false;
            if let Some(event) = device.poll()? {
                received = true;
                match event {
                    DeviceEvent::Report(Report::State(state)) => {
                        Self::emit(mapper.process(&state), &mut outputs, &events, &device)?;
                    }
                    DeviceEvent::Report(Report::Battery(state)) => {
                        battery = Some(state.level_percent.min(100));
                        charging = Some(state.charge_state.is_charging());
                    }
                    DeviceEvent::Disconnected => {
                        Self::emit(mapper.release_all(), &mut outputs, &events, &device)?;
                        battery = None;
                        charging = None;
                        log::info!("controller disconnected");
                    }
                    DeviceEvent::Report(_) => {}
                }
            }

            if device.connected() && last_lizard_refresh.elapsed() >= Duration::from_secs(3) {
                device.suppress_lizard_mode()?;
                last_lizard_refresh = Instant::now();
            }
            if let Some(next) = outputs.poll_rumble()? {
                rumble = next;
                device.rumble(rumble.0, rumble.1)?;
                last_rumble = Instant::now();
            } else if rumble != (0, 0) && last_rumble.elapsed() >= Duration::from_millis(40) {
                device.rumble(rumble.0, rumble.1)?;
                last_rumble = Instant::now();
            }
            if !received {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_control(
        &self,
        command: ControlCommand,
        mapper: &mut Mapper,
        outputs: &mut Outputs,
        events: &EventPublisher,
        device: &DeviceManager,
        battery_percent: Option<u8>,
        charging: Option<bool>,
    ) -> Result<(), DaemonError> {
        let reply = match command.request {
            ControlRequest::Status => Ok(ControlReply::Status(Status {
                connected: device.connected(),
                mode: mapper.active_mode().to_owned(),
                battery_percent,
                charging,
                device: device.device_name(),
            })),
            ControlRequest::Mode => Ok(ControlReply::Mode(mapper.active_mode().to_owned())),
            ControlRequest::SetMode(name) => match mapper.set_mode(&name) {
                Ok(mapped) => {
                    Self::emit(mapped, outputs, events, device)?;
                    Ok(ControlReply::Done)
                }
                Err(error) => Err(error.to_string()),
            },
            ControlRequest::NextMode => {
                Self::emit(mapper.next_mode(), outputs, events, device)?;
                Ok(ControlReply::Done)
            }
            ControlRequest::Reload => match Config::load(&self.config_path) {
                Ok(config) => match mapper.reload(config) {
                    Ok(mapped) => {
                        Self::emit(mapped, outputs, events, device)?;
                        Ok(ControlReply::Done)
                    }
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(error.to_string()),
            },
        };
        let _ = command.reply.send(reply);
        Ok(())
    }

    fn emit(
        mapped: Vec<Output>,
        outputs: &mut Outputs,
        events: &EventPublisher,
        device: &DeviceManager,
    ) -> Result<(), DaemonError> {
        for output in mapped {
            match &output {
                Output::Event { name } => events.publish(NamedEvent { name: name.clone() }),
                Output::ModeChanged { name } => log::info!("active mode: {name}"),
                Output::TrackpadHaptic { pad } => device.trackpad_haptic(*pad)?,
                _ => outputs.emit(&output)?,
            }
        }
        Ok(())
    }
}
