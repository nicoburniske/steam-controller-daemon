use crate::Result;
use crate::config::{Config, is_keyboard_key};
use crate::device::{DeviceEvent, DeviceManager};
use crate::ipc::{
    EventPublisher, NamedEvent, OskPadSide, OskPublisher, OskState, Request, Response, Server,
    Status,
};
use crate::mapper::{Mapper, Output};
use crate::output::Outputs;
use evdev::KeyCode;
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
        let (_server, events, osk) = Server::bind(&self.socket_path, command_sender)?;
        let mut keyboard = OskState::default();
        keyboard.set_bindings(mapper.osk_bindings());
        osk.send_replace(keyboard);
        let mut keyboard_closed_at: Option<Instant> = None;
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
                            keyboard.set_active_bindings(mapper.active_osk_bindings());
                            keyboard.shift_held = mapper.keyboard_shifted();
                            osk.send_replace(keyboard);
                            Self::emit(
                                &mut mapped,
                                &mut mapper,
                                &mut outputs,
                                &events,
                                &device,
                                &mut keyboard,
                                &osk,
                            )?;
                            Response::Done
                        }
                        Err(error) => Response::Error {
                            message: error.to_string(),
                        },
                    },
                    Request::ModeNext => {
                        mapper.next_mode(&mut mapped);
                        keyboard.set_active_bindings(mapper.active_osk_bindings());
                        keyboard.shift_held = mapper.keyboard_shifted();
                        osk.send_replace(keyboard);
                        Self::emit(
                            &mut mapped,
                            &mut mapper,
                            &mut outputs,
                            &events,
                            &device,
                            &mut keyboard,
                            &osk,
                        )?;
                        Response::Done
                    }
                    Request::Reload => match Config::load(&self.config_path) {
                        Ok(config) => match mapper.reload(config, &mut mapped) {
                            Ok(()) => {
                                keyboard.set_bindings(mapper.osk_bindings());
                                keyboard.set_active_bindings(mapper.active_osk_bindings());
                                keyboard.shift_held = mapper.keyboard_shifted();
                                osk.send_replace(keyboard);
                                Self::emit(
                                    &mut mapped,
                                    &mut mapper,
                                    &mut outputs,
                                    &events,
                                    &device,
                                    &mut keyboard,
                                    &osk,
                                )?;
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
                                &events,
                                &device,
                                &mut keyboard,
                                &osk,
                            )?;
                            keyboard.set_visible(false);
                            keyboard_closed_at = None;
                            osk.send_replace(keyboard);
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
                            &events,
                            &device,
                            &mut keyboard,
                            &osk,
                        )?;
                        match (was_visible, keyboard.visible) {
                            (true, false) => keyboard_closed_at = Some(Instant::now()),
                            (false, true) => keyboard_closed_at = None,
                            _ => {}
                        }
                        if keyboard.visible {
                            keyboard.set_active_bindings(mapper.active_osk_bindings());
                            keyboard.shift_held = mapper.keyboard_shifted();
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
                            let next = keyboard;
                            osk.send_if_modified(|state| {
                                if *state == next {
                                    false
                                } else {
                                    *state = next;
                                    true
                                }
                            });
                        }
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
                            &events,
                            &device,
                            &mut keyboard,
                            &osk,
                        )?;
                        keyboard.set_visible(false);
                        keyboard_closed_at = None;
                        osk.send_replace(keyboard);
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
        events: &EventPublisher,
        device: &DeviceManager,
        keyboard: &mut OskState,
        osk: &OskPublisher,
    ) -> Result<()> {
        if mapped
            .iter()
            .any(|output| matches!(output, Output::KeyboardToggle))
        {
            mapped.retain(|output| !matches!(output, Output::KeyboardToggle));
            mapper.suspend(mapped);
            keyboard.set_visible(!keyboard.visible);
            osk.send_replace(*keyboard);
        }
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
