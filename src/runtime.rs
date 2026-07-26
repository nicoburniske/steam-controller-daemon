use crate::Result;
use crate::config::Config;
use crate::device::{DeviceEvent, DeviceManager};
use crate::ipc::{EventPublisher, NamedEvent, Request, Response, Server, Status};
use crate::mapper::{Mapper, Output};
use crate::output::Outputs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub struct Daemon {
    config_path: PathBuf,
    socket_path: PathBuf,
}

impl Daemon {
    pub fn new(config_path: impl AsRef<Path>, socket_path: impl AsRef<Path>) -> Self {
        Self {
            config_path: config_path.as_ref().to_path_buf(),
            socket_path: socket_path.as_ref().to_path_buf(),
        }
    }

    pub fn run(self) -> Result<()> {
        let config = Config::load(&self.config_path)?;
        let mut mapper = Mapper::new(config);
        let mut mapped = Vec::new();
        let mut outputs = Outputs::new()?;
        let mut device = DeviceManager::new()?;
        let (command_sender, commands) = mpsc::sync_channel(32);
        let (_server, events) = Server::bind(&self.socket_path, command_sender)?;
        let mut battery = None;
        let mut charging = None;
        let mut last_lizard_refresh = Instant::now() - Duration::from_secs(4);
        let mut rumble = (0, 0);
        let mut last_rumble = Instant::now() - Duration::from_millis(50);

        log::info!("active mode: {}", mapper.active_mode());
        loop {
            while let Ok(command) = commands.try_recv() {
                let response = match command.request {
                    Request::Status => Response::Status {
                        status: Status {
                            connected: device.connected(),
                            mode: mapper.active_mode().to_owned(),
                            battery_percent: battery,
                            charging,
                            device: device.device_name(),
                        },
                    },
                    Request::Mode => Response::Mode {
                        name: mapper.active_mode().to_owned(),
                    },
                    Request::ModeSet { name } => match mapper.set_mode(&name, &mut mapped) {
                        Ok(()) => {
                            Self::emit(&mut mapped, &mut outputs, &events, &device)?;
                            Response::Done
                        }
                        Err(error) => Response::Error {
                            message: error.to_string(),
                        },
                    },
                    Request::ModeNext => {
                        mapper.next_mode(&mut mapped);
                        Self::emit(&mut mapped, &mut outputs, &events, &device)?;
                        Response::Done
                    }
                    Request::Reload => match Config::load(&self.config_path) {
                        Ok(config) => match mapper.reload(config, &mut mapped) {
                            Ok(()) => {
                                Self::emit(&mut mapped, &mut outputs, &events, &device)?;
                                Response::Done
                            }
                            Err(error) => Response::Error {
                                message: error.to_string(),
                            },
                        },
                        Err(error) => Response::Error {
                            message: error.to_string(),
                        },
                    },
                    Request::Events => unreachable!(),
                };
                let _ = command.reply.send(response);
            }

            let mut received = false;
            if let Some(event) = device.poll()? {
                received = true;
                match event {
                    DeviceEvent::State(state) => {
                        mapper.process(&state, &mut mapped);
                        Self::emit(&mut mapped, &mut outputs, &events, &device)?;
                    }
                    DeviceEvent::Battery {
                        percent,
                        charging: is_charging,
                    } => {
                        battery = Some(percent);
                        charging = Some(is_charging);
                    }
                    DeviceEvent::Disconnected => {
                        mapper.release_all(&mut mapped);
                        Self::emit(&mut mapped, &mut outputs, &events, &device)?;
                        battery = None;
                        charging = None;
                        log::info!("controller disconnected");
                    }
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

    fn emit(
        mapped: &mut Vec<Output>,
        outputs: &mut Outputs,
        events: &EventPublisher,
        device: &DeviceManager,
    ) -> Result<()> {
        for output in mapped.drain(..) {
            match output {
                Output::Event { name } => {
                    let _ = events.send(NamedEvent { name });
                }
                Output::ModeChanged { name } => log::info!("active mode: {name}"),
                Output::TrackpadHaptic { pad } => device.trackpad_haptic(pad)?,
                output => outputs.emit(&output)?,
            }
        }
        Ok(())
    }
}
