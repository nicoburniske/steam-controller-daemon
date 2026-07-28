use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventSummary, EventType, FFEffectCode,
    FFEffectKind, InputEvent, InputId, KeyCode, RelativeAxisCode, UInputCode, UinputAbsSetup,
};
use std::os::fd::AsRawFd;

use crate::mapper::{GamepadAxis, Output};
use scd::config::{Gamepad, GamepadButton, MouseButton, is_keyboard_key};
use scd::{Error, Result};

pub struct Outputs {
    gamepad: Option<VirtualDevice>,
    gamepad_type: Gamepad,
    gamepad_dpad: [bool; 4],
    keyboard: VirtualDevice,
    left_shift_held: bool,
    right_shift_held: bool,
    mouse: VirtualDevice,
    mouse_remainder: [f32; 2],
    scroll_remainder: [f32; 2],
    rumble: RumbleEffects,
}

#[derive(Default)]
struct RumbleEffects {
    effects: [Option<(u16, u16)>; 16],
    active: u16,
    last_output: (u16, u16),
}

impl Outputs {
    pub fn new(gamepad: Gamepad) -> Result<Self> {
        let mut keyboard_keys = AttributeSet::<KeyCode>::new();
        for code in 1..=0x2ff {
            let key = KeyCode::new(code);
            if is_keyboard_key(key) {
                keyboard_keys.insert(key);
            }
        }
        let keyboard = VirtualDevice::builder()?
            .name("scd keyboard")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1209, 0x5344, 1))
            .with_keys(&keyboard_keys)?
            .build()?;

        let mouse_keys = AttributeSet::from_iter([
            KeyCode::BTN_LEFT,
            KeyCode::BTN_RIGHT,
            KeyCode::BTN_MIDDLE,
            KeyCode::BTN_SIDE,
            KeyCode::BTN_EXTRA,
            KeyCode::BTN_FORWARD,
            KeyCode::BTN_BACK,
            KeyCode::BTN_TASK,
        ]);
        let mouse_axes = AttributeSet::from_iter([
            RelativeAxisCode::REL_X,
            RelativeAxisCode::REL_Y,
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_HWHEEL,
        ]);
        let mouse = VirtualDevice::builder()?
            .name("scd mouse")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1209, 0x5345, 1))
            .with_keys(&mouse_keys)?
            .with_relative_axes(&mouse_axes)?
            .build()?;

        let mut outputs = Self {
            gamepad: None,
            gamepad_type: Gamepad::None,
            gamepad_dpad: [false; 4],
            keyboard,
            left_shift_held: false,
            right_shift_held: false,
            mouse,
            mouse_remainder: [0.0; 2],
            scroll_remainder: [0.0; 2],
            rumble: RumbleEffects::default(),
        };
        outputs.set_gamepad(gamepad)?;
        Ok(outputs)
    }

    pub fn set_gamepad(&mut self, gamepad: Gamepad) -> Result<()> {
        if gamepad == self.gamepad_type {
            return Ok(());
        }
        let device = match gamepad {
            Gamepad::None => None,
            Gamepad::Xbox => Some(Self::create_xbox()?),
        };
        self.rumble = RumbleEffects {
            last_output: self.rumble.last_output,
            ..RumbleEffects::default()
        };
        self.gamepad_dpad = [false; 4];
        self.gamepad = device;
        self.gamepad_type = gamepad;
        Ok(())
    }

    pub fn emit(&mut self, command: &Output) -> Result<()> {
        match command {
            Output::Key { key, pressed } => {
                self.keyboard.emit(&[InputEvent::new(
                    EventType::KEY.0,
                    key.code(),
                    i32::from(*pressed),
                )])?;
                if *key == KeyCode::KEY_LEFTSHIFT {
                    self.left_shift_held = *pressed;
                } else if *key == KeyCode::KEY_RIGHTSHIFT {
                    self.right_shift_held = *pressed;
                }
                Ok(())
            }
            Output::MouseButton { button, pressed } => {
                let code = match button {
                    MouseButton::Left => KeyCode::BTN_LEFT,
                    MouseButton::Right => KeyCode::BTN_RIGHT,
                    MouseButton::Middle => KeyCode::BTN_MIDDLE,
                    MouseButton::Side => KeyCode::BTN_SIDE,
                    MouseButton::Extra => KeyCode::BTN_EXTRA,
                    MouseButton::Forward => KeyCode::BTN_FORWARD,
                    MouseButton::Back => KeyCode::BTN_BACK,
                    MouseButton::Task => KeyCode::BTN_TASK,
                };
                Ok(self.mouse.emit(&[InputEvent::new(
                    EventType::KEY.0,
                    code.code(),
                    i32::from(*pressed),
                )])?)
            }
            Output::GamepadButton { button, pressed } => {
                let Some(gamepad) = &mut self.gamepad else {
                    return Ok(());
                };
                let pressed_value = i32::from(*pressed);
                let key = |code: KeyCode| (EventType::KEY, code.code(), pressed_value);
                let (event_type, code, value) = match button {
                    GamepadButton::South => key(KeyCode::BTN_SOUTH),
                    GamepadButton::East => key(KeyCode::BTN_EAST),
                    // xbox x/y aliases are BTN_NORTH/BTN_WEST
                    GamepadButton::North => key(KeyCode::BTN_WEST),
                    GamepadButton::West => key(KeyCode::BTN_NORTH),
                    GamepadButton::LeftBumper => key(KeyCode::BTN_TL),
                    GamepadButton::RightBumper => key(KeyCode::BTN_TR),
                    GamepadButton::LeftTrigger => (
                        EventType::ABSOLUTE,
                        AbsoluteAxisCode::ABS_Z.0,
                        pressed_value * 255,
                    ),
                    GamepadButton::RightTrigger => (
                        EventType::ABSOLUTE,
                        AbsoluteAxisCode::ABS_RZ.0,
                        pressed_value * 255,
                    ),
                    GamepadButton::Select => key(KeyCode::BTN_SELECT),
                    GamepadButton::Start => key(KeyCode::BTN_START),
                    GamepadButton::Guide => key(KeyCode::BTN_MODE),
                    GamepadButton::LeftStick => key(KeyCode::BTN_THUMBL),
                    GamepadButton::RightStick => key(KeyCode::BTN_THUMBR),
                    GamepadButton::DpadUp => {
                        self.gamepad_dpad[0] = *pressed;
                        (
                            EventType::ABSOLUTE,
                            AbsoluteAxisCode::ABS_HAT0Y.0,
                            i32::from(self.gamepad_dpad[1]) - i32::from(self.gamepad_dpad[0]),
                        )
                    }
                    GamepadButton::DpadDown => {
                        self.gamepad_dpad[1] = *pressed;
                        (
                            EventType::ABSOLUTE,
                            AbsoluteAxisCode::ABS_HAT0Y.0,
                            i32::from(self.gamepad_dpad[1]) - i32::from(self.gamepad_dpad[0]),
                        )
                    }
                    GamepadButton::DpadLeft => {
                        self.gamepad_dpad[2] = *pressed;
                        (
                            EventType::ABSOLUTE,
                            AbsoluteAxisCode::ABS_HAT0X.0,
                            i32::from(self.gamepad_dpad[3]) - i32::from(self.gamepad_dpad[2]),
                        )
                    }
                    GamepadButton::DpadRight => {
                        self.gamepad_dpad[3] = *pressed;
                        (
                            EventType::ABSOLUTE,
                            AbsoluteAxisCode::ABS_HAT0X.0,
                            i32::from(self.gamepad_dpad[3]) - i32::from(self.gamepad_dpad[2]),
                        )
                    }
                    GamepadButton::PaddleLeftUpper
                    | GamepadButton::PaddleLeftLower
                    | GamepadButton::PaddleRightUpper
                    | GamepadButton::PaddleRightLower => return Ok(()),
                };
                Ok(gamepad.emit(&[InputEvent::new(event_type.0, code, value)])?)
            }
            Output::GamepadAxis { axis, value } => {
                let Some(gamepad) = &mut self.gamepad else {
                    return Ok(());
                };
                let code = match axis {
                    GamepadAxis::LeftX => AbsoluteAxisCode::ABS_X,
                    GamepadAxis::LeftY => AbsoluteAxisCode::ABS_Y,
                    GamepadAxis::RightX => AbsoluteAxisCode::ABS_RX,
                    GamepadAxis::RightY => AbsoluteAxisCode::ABS_RY,
                    GamepadAxis::LeftTrigger => AbsoluteAxisCode::ABS_Z,
                    GamepadAxis::RightTrigger => AbsoluteAxisCode::ABS_RZ,
                };
                let value = if matches!(code, AbsoluteAxisCode::ABS_Z | AbsoluteAxisCode::ABS_RZ) {
                    (value.clamp(0.0, 1.0) * 255.0).round() as i32
                } else {
                    let value = value.clamp(-1.0, 1.0);
                    (value * if value < 0.0 { 32768.0 } else { 32767.0 }).round() as i32
                };
                Ok(gamepad.emit(&[InputEvent::new(EventType::ABSOLUTE.0, code.0, value)])?)
            }
            Output::MouseMotion { x, y } => Self::emit_relative(
                &mut self.mouse,
                &mut self.mouse_remainder,
                [RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y],
                [*x, *y],
            ),
            Output::Scroll { x, y } => Self::emit_relative(
                &mut self.mouse,
                &mut self.scroll_remainder,
                [RelativeAxisCode::REL_HWHEEL, RelativeAxisCode::REL_WHEEL],
                [*x, *y],
            ),
            Output::KeyboardToggle | Output::ModeChanged { .. } | Output::TrackpadHaptic { .. } => {
                Ok(())
            }
        }
    }

    pub fn key(&mut self, key: KeyCode, shift: bool) -> Result<()> {
        if shift && !self.left_shift_held && !self.right_shift_held {
            Ok(self.keyboard.emit(&[
                InputEvent::new(EventType::KEY.0, KeyCode::KEY_LEFTSHIFT.code(), 1),
                InputEvent::new(EventType::KEY.0, key.code(), 1),
                InputEvent::new(EventType::KEY.0, key.code(), 0),
                InputEvent::new(EventType::KEY.0, KeyCode::KEY_LEFTSHIFT.code(), 0),
            ])?)
        } else {
            Ok(self.keyboard.emit(&[
                InputEvent::new(EventType::KEY.0, key.code(), 1),
                InputEvent::new(EventType::KEY.0, key.code(), 0),
            ])?)
        }
    }

    pub fn poll_rumble(&mut self) -> Result<Option<(u16, u16)>> {
        let Some(gamepad) = &mut self.gamepad else {
            if self.rumble.last_output == (0, 0) {
                return Ok(None);
            }
            self.rumble.last_output = (0, 0);
            return Ok(Some((0, 0)));
        };
        let events = match gamepad.fetch_events() {
            Ok(events) => events.collect::<Vec<_>>(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(Error::message(error)),
        };
        for event in events {
            match event.destructure() {
                EventSummary::UInput(event, UInputCode::UI_FF_UPLOAD, ..) => {
                    let mut upload = gamepad.process_ff_upload(event)?;
                    let effect = upload.effect();
                    let rumble = match effect.kind {
                        FFEffectKind::Rumble {
                            strong_magnitude,
                            weak_magnitude,
                        } => Some((strong_magnitude, weak_magnitude)),
                        _ => None,
                    };
                    if let Some(id) = self.rumble.upload(upload.effect_id(), rumble) {
                        upload.set_effect_id(id);
                        upload.set_retval(0);
                    } else {
                        upload.set_retval(-1);
                    }
                }
                EventSummary::UInput(event, UInputCode::UI_FF_ERASE, ..) => {
                    let erase = gamepad.process_ff_erase(event)?;
                    self.rumble.erase(erase.effect_id() as i16);
                }
                EventSummary::ForceFeedback(_, id, repetitions) => {
                    self.rumble.set_playback(id.0 as i16, repetitions);
                }
                _ => {}
            }
        }

        Ok(self.rumble.changed_output())
    }

    fn create_xbox() -> Result<VirtualDevice> {
        let gamepad_keys = AttributeSet::from_iter([
            KeyCode::BTN_SOUTH,
            KeyCode::BTN_EAST,
            KeyCode::BTN_NORTH,
            KeyCode::BTN_WEST,
            KeyCode::BTN_TL,
            KeyCode::BTN_TR,
            KeyCode::BTN_SELECT,
            KeyCode::BTN_START,
            KeyCode::BTN_MODE,
            KeyCode::BTN_THUMBL,
            KeyCode::BTN_THUMBR,
        ]);
        let stick = AbsInfo::new(0, -32768, 32767, 16, 128, 0);
        let trigger = AbsInfo::new(0, 0, 255, 0, 0, 0);
        let dpad = AbsInfo::new(0, -1, 1, 0, 0, 0);
        let gamepad = VirtualDevice::builder()?
            .name("Microsoft X-Box 360 pad")
            .input_id(InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0114))
            .with_keys(&gamepad_keys)?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, stick))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, stick))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RX, stick))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RY, stick))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Z, trigger))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RZ, trigger))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_HAT0X, dpad))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_HAT0Y, dpad))?
            .with_ff(&AttributeSet::from_iter([FFEffectCode::FF_RUMBLE]))?
            .with_ff_effects_max(16)
            .build()?;
        let flags = unsafe { libc::fcntl(gamepad.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(gamepad.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(Error::message(std::io::Error::last_os_error()));
        }
        Ok(gamepad)
    }

    fn emit_relative(
        device: &mut VirtualDevice,
        remainder: &mut [f32; 2],
        axes: [RelativeAxisCode; 2],
        value: [f32; 2],
    ) -> Result<()> {
        remainder[0] += value[0];
        remainder[1] += value[1];
        let output = remainder.map(|value| value.trunc() as i32);
        remainder[0] -= output[0] as f32;
        remainder[1] -= output[1] as f32;
        if output == [0; 2] {
            return Ok(());
        }
        Ok(device.emit(&[
            InputEvent::new(EventType::RELATIVE.0, axes[0].0, output[0]),
            InputEvent::new(EventType::RELATIVE.0, axes[1].0, output[1]),
        ])?)
    }
}

impl RumbleEffects {
    fn upload(&mut self, requested_id: i16, effect: Option<(u16, u16)>) -> Option<i16> {
        let effect = effect?;
        let index = if requested_id >= 0 {
            let index = usize::try_from(requested_id).ok()?;
            (index < self.effects.len()).then_some(index)?
        } else {
            self.effects.iter().position(Option::is_none)?
        };
        self.effects[index] = Some(effect);
        Some(index as i16)
    }

    fn erase(&mut self, id: i16) {
        let Ok(index) = usize::try_from(id) else {
            return;
        };
        if index < self.effects.len() {
            self.effects[index] = None;
            self.active &= !(1 << index);
        }
    }

    fn set_playback(&mut self, id: i16, repetitions: i32) {
        let Ok(index) = usize::try_from(id) else {
            return;
        };
        if index >= self.effects.len() {
            return;
        }
        if repetitions > 0 {
            self.active |= 1 << index;
        } else if repetitions == 0 {
            self.active &= !(1 << index);
        }
    }

    fn changed_output(&mut self) -> Option<(u16, u16)> {
        let mut output = (0, 0);
        for (index, effect) in self.effects.iter().enumerate() {
            if self.active & (1 << index) != 0
                && let Some(effect) = effect
            {
                output.0 = output.0.max(effect.0);
                output.1 = output.1.max(effect.1);
            }
        }
        if output == self.last_output {
            return None;
        }
        self.last_output = output;
        Some(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_repeat_counts_start_rumble_and_zero_stops_it() {
        let mut effects = RumbleEffects::default();
        let id = effects.upload(-1, Some((12, 34))).unwrap();

        effects.set_playback(id, 3);
        assert_eq!(effects.changed_output(), Some((12, 34)));
        effects.set_playback(id, 0);
        assert_eq!(effects.changed_output(), Some((0, 0)));
    }

    #[test]
    fn rejected_upload_does_not_consume_an_effect_id() {
        let mut effects = RumbleEffects::default();

        assert_eq!(effects.upload(-1, None), None);
        assert_eq!(effects.upload(-1, Some((1, 2))), Some(0));
    }
}
