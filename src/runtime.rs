use crate::Result;
use crate::config::{Config, is_keyboard_key};
use crate::device::{DeviceEvent, DeviceManager};
use crate::ipc::{OskPadSide, OskState, Request, Response, Server, Status};
use crate::mapper::{Mapper, Output};
use crate::output::Outputs;
use evdev::KeyCode;
use std::path::{Path, PathBuf};
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
        let trackpad_click_pressure = config.trackpads.click_pressure;
        let mut mapper = Mapper::new(config);
        let mut mapped = Vec::new();
        let mut outputs = Outputs::new()?;
        let mut device = DeviceManager::new(trackpad_click_pressure)?;
        let (ipc, commands) = Server::bind(&self.socket_path)?;
        let mut keyboard = OskState::default();
        keyboard.set_bindings(mapper.osk_bindings());
        Self::publish_keyboard(&mapper, &mut keyboard, &ipc);
        let mut keyboard_closed_at: Option<Instant> = None;
        let mut battery = None;
        let mut charging = None;
        let mut last_lizard_refresh = Instant::now();
        let mut rumble = (0, 0);
        let mut last_rumble = Instant::now();

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
                            Self::emit(
                                &mut mapped,
                                &mut mapper,
                                &mut outputs,
                                &ipc,
                                &device,
                                &mut keyboard,
                            )?;
                            Self::publish_keyboard(&mapper, &mut keyboard, &ipc);
                            Response::Done
                        }
                        Err(error) => Response::Error {
                            message: error.to_string(),
                        },
                    },
                    Request::ModeNext => {
                        mapper.next_mode(&mut mapped);
                        Self::emit(
                            &mut mapped,
                            &mut mapper,
                            &mut outputs,
                            &ipc,
                            &device,
                            &mut keyboard,
                        )?;
                        Self::publish_keyboard(&mapper, &mut keyboard, &ipc);
                        Response::Done
                    }
                    Request::Sound { sound } => match device.play_haptic(sound) {
                        Ok(()) => Response::Done,
                        Err(error) => Response::Error {
                            message: error.to_string(),
                        },
                    },
                    Request::Reload => match Config::load(&self.config_path) {
                        Ok(config) => {
                            device.set_trackpad_click_pressure(config.trackpads.click_pressure)?;
                            mapper.reload(config, &mut mapped);
                            keyboard.set_bindings(mapper.osk_bindings());
                            Self::emit(
                                &mut mapped,
                                &mut mapper,
                                &mut outputs,
                                &ipc,
                                &device,
                                &mut keyboard,
                            )?;
                            Self::publish_keyboard(&mapper, &mut keyboard, &ipc);
                            Response::Done
                        }
                        Err(error) => Response::Error {
                            message: error.to_string(),
                        },
                    },
                    Request::OskHide { session } => {
                        if session == 0 || session != keyboard.session() || !keyboard.visible {
                            Response::Error {
                                message: "keyboard session is no longer active".into(),
                            }
                        } else {
                            mapper.suspend(&mut mapped);
                            Self::emit(
                                &mut mapped,
                                &mut mapper,
                                &mut outputs,
                                &ipc,
                                &device,
                                &mut keyboard,
                            )?;
                            keyboard.set_visible(false);
                            keyboard_closed_at = None;
                            Self::publish_keyboard(&mapper, &mut keyboard, &ipc);
                            Response::Done
                        }
                    }
                    Request::Key {
                        code,
                        shift,
                        session,
                    } => {
                        if session == 0
                            || session != keyboard.session()
                            || (!keyboard.visible
                                && !keyboard_closed_at.is_some_and(|closed| {
                                    closed.elapsed() < Duration::from_secs(1)
                                }))
                        {
                            Response::Error {
                                message: "keyboard session is no longer active".into(),
                            }
                        } else if !is_keyboard_key(KeyCode::new(code)) {
                            Response::Error {
                                message: format!("invalid keyboard code {code}"),
                            }
                        } else {
                            outputs.key(KeyCode::new(code), shift)?;
                            Response::Done
                        }
                    }
                    Request::Events | Request::Osk => unreachable!(),
                };
                let _ = command.reply.send(response);
            }

            let mut received = false;
            if let Some(event) = device.poll()? {
                received = true;
                match event {
                    DeviceEvent::State(state) => {
                        let was_visible = keyboard.visible;
                        mapper.process(&state, keyboard.visible, &mut mapped);
                        Self::emit(
                            &mut mapped,
                            &mut mapper,
                            &mut outputs,
                            &ipc,
                            &device,
                            &mut keyboard,
                        )?;
                        match (was_visible, keyboard.visible) {
                            (true, false) => keyboard_closed_at = Some(Instant::now()),
                            (false, true) => keyboard_closed_at = None,
                            _ => {}
                        }
                        if keyboard.visible {
                            Self::sync_keyboard(&mapper, &mut keyboard);
                            for (side, source) in [
                                (OskPadSide::Left, state.left_pad),
                                (OskPadSide::Right, state.right_pad),
                            ] {
                                keyboard.update_pad(
                                    side,
                                    crate::ipc::OskPad {
                                        touched: source.touched,
                                        pressed: source.clicked,
                                        position: source.position,
                                    },
                                    was_visible,
                                );
                            }
                        }
                        ipc.publish_osk(keyboard);
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
                        Self::emit(
                            &mut mapped,
                            &mut mapper,
                            &mut outputs,
                            &ipc,
                            &device,
                            &mut keyboard,
                        )?;
                        keyboard.set_visible(false);
                        keyboard_closed_at = None;
                        Self::publish_keyboard(&mapper, &mut keyboard, &ipc);
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
        mapper: &mut Mapper,
        outputs: &mut Outputs,
        ipc: &Server,
        device: &DeviceManager,
        keyboard: &mut OskState,
    ) -> Result<()> {
        if mapped
            .iter()
            .any(|output| matches!(output, Output::KeyboardToggle))
        {
            mapped.retain(|output| !matches!(output, Output::KeyboardToggle));
            mapper.suspend(mapped);
            keyboard.set_visible(!keyboard.visible);
        }
        for output in mapped.drain(..) {
            match output {
                Output::Event { name } => {
                    ipc.publish_event(name);
                }
                Output::ModeChanged { name } => {
                    log::info!("active mode: {name}");
                    device.mode_switch_haptic()?;
                }
                Output::TrackpadHaptic { pad } => device.trackpad_haptic(pad)?,
                output => outputs.emit(&output)?,
            }
        }
        Ok(())
    }

    fn sync_keyboard(mapper: &Mapper, keyboard: &mut OskState) {
        if keyboard.visible {
            keyboard.set_active_bindings(mapper.active_osk_bindings());
            keyboard.shift_held = mapper.keyboard_shifted();
        }
    }

    fn publish_keyboard(mapper: &Mapper, keyboard: &mut OskState, ipc: &Server) {
        Self::sync_keyboard(mapper, keyboard);
        ipc.publish_osk(*keyboard);
    }
}
