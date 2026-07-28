mod device;
mod ipc;
mod mapper;
mod output;
mod steam;

use clap::Parser;
use device::{DeviceEvent, DeviceManager};
use evdev::KeyCode;
use ipc::Server;
use mapper::{Mapper, Output};
use output::Outputs;
use scd::Result;
use scd::config::{Config, Gamepad, is_keyboard_key};
use scd::ipc::{OskPad, OskPadSide, OskState, Request, Response, Status};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(version, about = "Steam Controller userspace daemon")]
struct Args {
    #[arg(long, default_value = "/etc/scd/config.toml")]
    config: PathBuf,
    #[arg(long, default_value = "/run/scd/control.sock")]
    socket: PathBuf,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let Args {
        config: config_path,
        socket,
    } = Args::parse();
    let config = Config::parse(&fs::read_to_string(&config_path)?)?;
    let mut device = DeviceManager::new(config.click_pressure)?;
    let mut mapper = Mapper::new(config);
    let mut mapped = Vec::new();
    let gamepad = if device.steam_enabled() {
        Gamepad::None
    } else {
        mapper.gamepad()
    };
    let mut outputs = Outputs::new(gamepad)?;
    let (mut ipc, commands) = Server::bind(socket)?;
    let mut keyboard = OskState::default();
    keyboard.set_bindings(mapper.osk_bindings());
    publish_keyboard(&mapper, &mut keyboard, &mut ipc);
    let mut keyboard_closed_at: Option<Instant> = None;
    let mut battery = None;
    let mut charging = None;
    let mut last_lizard_refresh = Instant::now();
    let mut rumble = (0, 0);
    let mut last_rumble = Instant::now();

    log::info!("active mode: {}", mapper.active_mode());
    loop {
        for command in commands.try_iter() {
            let response = match &command.request {
                Request::Status => Response::Status {
                    status: Status {
                        connected: device.connected(),
                        steam: device.steam_enabled(),
                        mode: mapper.active_mode().to_owned(),
                        battery_percent: battery,
                        charging,
                        device: device.device_name(),
                    },
                },
                Request::ModeSet { name } => match mapper.set_mode(name, &mut mapped) {
                    Ok(()) => {
                        emit(
                            &mut mapped,
                            &mut mapper,
                            &mut outputs,
                            &device,
                            &mut keyboard,
                        )?;
                        publish_keyboard(&mapper, &mut keyboard, &mut ipc);
                        Response::Done
                    }
                    Err(error) => Response::Error {
                        message: error.to_string(),
                    },
                },
                Request::ModeNext => {
                    mapper.next_mode(&mut mapped);
                    emit(
                        &mut mapped,
                        &mut mapper,
                        &mut outputs,
                        &device,
                        &mut keyboard,
                    )?;
                    publish_keyboard(&mapper, &mut keyboard, &mut ipc);
                    Response::Done
                }
                Request::Haptic { haptic } => match device.play_haptic(*haptic) {
                    Ok(()) => Response::Done,
                    Err(error) => Response::Error {
                        message: error.to_string(),
                    },
                },
                Request::Reload => {
                    let source = fs::read_to_string(&config_path)?;
                    let config = Config::parse(&source);
                    match config {
                        Ok(config) => {
                            device.set_trackpad_click_pressure(config.click_pressure)?;
                            mapper.reload(config, &mut mapped);
                            keyboard.set_bindings(mapper.osk_bindings());
                            emit(
                                &mut mapped,
                                &mut mapper,
                                &mut outputs,
                                &device,
                                &mut keyboard,
                            )?;
                            publish_keyboard(&mapper, &mut keyboard, &mut ipc);
                            Response::Done
                        }
                        Err(error) => Response::Error {
                            message: error.to_string(),
                        },
                    }
                }
                Request::Steam { enabled } => match device.set_steam(*enabled) {
                    Ok(()) => {
                        if *enabled {
                            rumble = (0, 0);
                            mapper.release_all(&mut mapped);
                            emit(
                                &mut mapped,
                                &mut mapper,
                                &mut outputs,
                                &device,
                                &mut keyboard,
                            )?;
                            keyboard.set_visible(false);
                            keyboard_closed_at = None;
                            publish_keyboard(&mapper, &mut keyboard, &mut ipc);
                        }
                        outputs.set_gamepad(if *enabled {
                            Gamepad::None
                        } else {
                            mapper.gamepad()
                        })?;
                        Response::Done
                    }
                    Err(error) => {
                        outputs.set_gamepad(if device.steam_enabled() {
                            Gamepad::None
                        } else {
                            mapper.gamepad()
                        })?;
                        Response::Error {
                            message: error.to_string(),
                        }
                    }
                },
                Request::OskHide { session } => {
                    if *session == 0 || *session != keyboard.session() || !keyboard.visible {
                        Response::Error {
                            message: "keyboard session is no longer active".into(),
                        }
                    } else {
                        mapper.suspend(&mut mapped);
                        emit(
                            &mut mapped,
                            &mut mapper,
                            &mut outputs,
                            &device,
                            &mut keyboard,
                        )?;
                        keyboard.set_visible(false);
                        keyboard_closed_at = None;
                        publish_keyboard(&mapper, &mut keyboard, &mut ipc);
                        Response::Done
                    }
                }
                Request::Key {
                    code,
                    shift,
                    session,
                } => {
                    let key = KeyCode::new(*code);
                    if *session == 0
                        || *session != keyboard.session()
                        || (!keyboard.visible
                            && !keyboard_closed_at
                                .is_some_and(|closed| closed.elapsed() < Duration::from_secs(1)))
                    {
                        Response::Error {
                            message: "keyboard session is no longer active".into(),
                        }
                    } else if !is_keyboard_key(key) {
                        Response::Error {
                            message: format!("invalid keyboard code {code}"),
                        }
                    } else {
                        outputs.key(key, *shift)?;
                        Response::Done
                    }
                }
                Request::Osk => unreachable!(),
            };
            ipc.respond(command, response);
        }

        let mut received = false;
        if let Some(event) = device.poll()? {
            received = true;
            match event {
                DeviceEvent::State(state) => {
                    let was_visible = keyboard.visible;
                    mapper.process(&state, keyboard.visible, &mut mapped);
                    emit(
                        &mut mapped,
                        &mut mapper,
                        &mut outputs,
                        &device,
                        &mut keyboard,
                    )?;
                    match (was_visible, keyboard.visible) {
                        (true, false) => keyboard_closed_at = Some(Instant::now()),
                        (false, true) => keyboard_closed_at = None,
                        _ => {}
                    }
                    if keyboard.visible {
                        sync_keyboard(&mapper, &mut keyboard);
                        for (side, source) in [
                            (OskPadSide::Left, state.left_pad),
                            (OskPadSide::Right, state.right_pad),
                        ] {
                            keyboard.update_pad(
                                side,
                                OskPad {
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
                    outputs.set_gamepad(mapper.gamepad())?;
                    mapper.release_all(&mut mapped);
                    emit(
                        &mut mapped,
                        &mut mapper,
                        &mut outputs,
                        &device,
                        &mut keyboard,
                    )?;
                    keyboard.set_visible(false);
                    keyboard_closed_at = None;
                    publish_keyboard(&mapper, &mut keyboard, &mut ipc);
                    battery = None;
                    charging = None;
                    log::info!("controller disconnected");
                }
            }
        }

        if device.connected()
            && !device.steam_enabled()
            && last_lizard_refresh.elapsed() >= Duration::from_secs(3)
        {
            device.suppress_lizard_mode()?;
            last_lizard_refresh = Instant::now();
        }
        if let Some(next) = outputs.poll_rumble()? {
            rumble = next;
            if !device.steam_enabled() {
                device.rumble(rumble.0, rumble.1)?;
            }
            last_rumble = Instant::now();
        } else if !device.steam_enabled()
            && rumble != (0, 0)
            && last_rumble.elapsed() >= Duration::from_millis(40)
        {
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
            Output::ModeChanged { name, haptic } => {
                log::info!("active mode: {name}");
                outputs.set_gamepad(if device.steam_enabled() {
                    Gamepad::None
                } else {
                    mapper.gamepad()
                })?;
                if let Some(haptic) = haptic {
                    device.play_haptic(haptic)?;
                }
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

fn publish_keyboard(mapper: &Mapper, keyboard: &mut OskState, ipc: &mut Server) {
    sync_keyboard(mapper, keyboard);
    ipc.publish_osk(*keyboard);
}
