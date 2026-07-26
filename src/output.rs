use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventSummary, EventType, FFEffectCode,
    FFEffectKind, InputEvent, InputId, KeyCode, RelativeAxisCode, UInputCode, UinputAbsSetup,
};
use std::os::fd::AsRawFd;

use crate::config::{GamepadButton, MouseButton};
use crate::mapper::{GamepadAxis, Output};
use crate::{Error, Result, ResultExt};

pub struct Outputs {
    gamepad: VirtualDevice,
    keyboard: VirtualDevice,
    mouse: VirtualDevice,
    mouse_remainder: (f32, f32),
    scroll_remainder: (f32, f32),
    rumble: RumbleEffects,
}

struct RumbleEffects {
    effects: [Option<(u16, u16)>; 16],
    active: u16,
    last_output: (u16, u16),
}

impl Outputs {
    pub fn new() -> Result<Self> {
        let mut keyboard_keys = AttributeSet::<KeyCode>::new();
        for code in 1..=0x2ff {
            if !(0x100..=0x15f).contains(&code) && !(0x2c0..=0x2ff).contains(&code) {
                keyboard_keys.insert(KeyCode::new(code));
            }
        }
        let keyboard = VirtualDevice::builder()
            .whence()?
            .name("scd keyboard")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1209, 0x5344, 1))
            .with_keys(&keyboard_keys)
            .whence()?
            .build()
            .whence()?;

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
        let mouse = VirtualDevice::builder()
            .whence()?
            .name("scd mouse")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1209, 0x5345, 1))
            .with_keys(&mouse_keys)
            .whence()?
            .with_relative_axes(&mouse_axes)
            .whence()?
            .build()
            .whence()?;

        let gamepad_keys = AttributeSet::from_iter([
            KeyCode::BTN_SOUTH,
            KeyCode::BTN_EAST,
            KeyCode::BTN_NORTH,
            KeyCode::BTN_WEST,
            KeyCode::BTN_TL,
            KeyCode::BTN_TR,
            KeyCode::BTN_TL2,
            KeyCode::BTN_TR2,
            KeyCode::BTN_SELECT,
            KeyCode::BTN_START,
            KeyCode::BTN_MODE,
            KeyCode::BTN_THUMBL,
            KeyCode::BTN_THUMBR,
            KeyCode::BTN_DPAD_UP,
            KeyCode::BTN_DPAD_DOWN,
            KeyCode::BTN_DPAD_LEFT,
            KeyCode::BTN_DPAD_RIGHT,
            KeyCode::BTN_TRIGGER_HAPPY1,
            KeyCode::BTN_TRIGGER_HAPPY2,
            KeyCode::BTN_TRIGGER_HAPPY3,
            KeyCode::BTN_TRIGGER_HAPPY4,
        ]);
        let stick = AbsInfo::new(0, -32768, 32767, 1024, 4096, 1);
        let trigger = AbsInfo::new(0, 0, 32767, 0, 256, 1);
        let gamepad = VirtualDevice::builder()
            .whence()?
            .name("scd gamepad")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1209, 0x5343, 1))
            .with_keys(&gamepad_keys)
            .whence()?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, stick))
            .whence()?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, stick))
            .whence()?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RX, stick))
            .whence()?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RY, stick))
            .whence()?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Z, trigger))
            .whence()?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RZ, trigger))
            .whence()?
            .with_ff(&AttributeSet::from_iter([FFEffectCode::FF_RUMBLE]))
            .whence()?
            .with_ff_effects_max(16)
            .build()
            .whence()?;
        let flags = unsafe { libc::fcntl(gamepad.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(gamepad.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(Error::message(std::io::Error::last_os_error()));
        }

        Ok(Self {
            gamepad,
            keyboard,
            mouse,
            mouse_remainder: (0.0, 0.0),
            scroll_remainder: (0.0, 0.0),
            rumble: RumbleEffects::new(),
        })
    }

    pub fn emit(&mut self, command: &Output) -> Result<()> {
        match command {
            Output::Key { key, pressed } => self
                .keyboard
                .emit(&[InputEvent::new(
                    EventType::KEY.0,
                    key.code(),
                    i32::from(*pressed),
                )])
                .whence(),
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
                self.mouse
                    .emit(&[InputEvent::new(
                        EventType::KEY.0,
                        code.code(),
                        i32::from(*pressed),
                    )])
                    .whence()
            }
            Output::GamepadButton { button, pressed } => {
                let code = match button {
                    GamepadButton::South => KeyCode::BTN_SOUTH,
                    GamepadButton::East => KeyCode::BTN_EAST,
                    GamepadButton::North => KeyCode::BTN_NORTH,
                    GamepadButton::West => KeyCode::BTN_WEST,
                    GamepadButton::LeftBumper => KeyCode::BTN_TL,
                    GamepadButton::RightBumper => KeyCode::BTN_TR,
                    GamepadButton::LeftTrigger => KeyCode::BTN_TL2,
                    GamepadButton::RightTrigger => KeyCode::BTN_TR2,
                    GamepadButton::Select => KeyCode::BTN_SELECT,
                    GamepadButton::Start => KeyCode::BTN_START,
                    GamepadButton::Guide => KeyCode::BTN_MODE,
                    GamepadButton::LeftStick => KeyCode::BTN_THUMBL,
                    GamepadButton::RightStick => KeyCode::BTN_THUMBR,
                    GamepadButton::DpadUp => KeyCode::BTN_DPAD_UP,
                    GamepadButton::DpadDown => KeyCode::BTN_DPAD_DOWN,
                    GamepadButton::DpadLeft => KeyCode::BTN_DPAD_LEFT,
                    GamepadButton::DpadRight => KeyCode::BTN_DPAD_RIGHT,
                    GamepadButton::PaddleLeftUpper => KeyCode::BTN_TRIGGER_HAPPY1,
                    GamepadButton::PaddleLeftLower => KeyCode::BTN_TRIGGER_HAPPY2,
                    GamepadButton::PaddleRightUpper => KeyCode::BTN_TRIGGER_HAPPY3,
                    GamepadButton::PaddleRightLower => KeyCode::BTN_TRIGGER_HAPPY4,
                };
                self.gamepad
                    .emit(&[InputEvent::new(
                        EventType::KEY.0,
                        code.code(),
                        i32::from(*pressed),
                    )])
                    .whence()
            }
            Output::GamepadAxis { axis, value } => {
                let code = match axis {
                    GamepadAxis::LeftX => AbsoluteAxisCode::ABS_X,
                    GamepadAxis::LeftY => AbsoluteAxisCode::ABS_Y,
                    GamepadAxis::RightX => AbsoluteAxisCode::ABS_RX,
                    GamepadAxis::RightY => AbsoluteAxisCode::ABS_RY,
                    GamepadAxis::LeftTrigger => AbsoluteAxisCode::ABS_Z,
                    GamepadAxis::RightTrigger => AbsoluteAxisCode::ABS_RZ,
                };
                let value = if matches!(code, AbsoluteAxisCode::ABS_Z | AbsoluteAxisCode::ABS_RZ) {
                    (value.clamp(0.0, 1.0) * 32767.0).round() as i32
                } else {
                    (value.clamp(-1.0, 1.0) * 32767.0).round() as i32
                };
                self.gamepad
                    .emit(&[InputEvent::new(EventType::ABSOLUTE.0, code.0, value)])
                    .whence()
            }
            Output::MouseMotion { x, y } => {
                self.mouse_remainder.0 += x;
                self.mouse_remainder.1 += y;
                let x = self.mouse_remainder.0.trunc() as i32;
                let y = self.mouse_remainder.1.trunc() as i32;
                self.mouse_remainder.0 -= x as f32;
                self.mouse_remainder.1 -= y as f32;
                if x == 0 && y == 0 {
                    return Ok(());
                }
                self.mouse
                    .emit(&[
                        InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, x),
                        InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, y),
                    ])
                    .whence()
            }
            Output::Scroll { x, y } => {
                self.scroll_remainder.0 += x;
                self.scroll_remainder.1 += y;
                let x = self.scroll_remainder.0.trunc() as i32;
                let y = self.scroll_remainder.1.trunc() as i32;
                self.scroll_remainder.0 -= x as f32;
                self.scroll_remainder.1 -= y as f32;
                if x == 0 && y == 0 {
                    return Ok(());
                }
                self.mouse
                    .emit(&[
                        InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_HWHEEL.0, x),
                        InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_WHEEL.0, y),
                    ])
                    .whence()
            }
            Output::Event { .. } | Output::ModeChanged { .. } | Output::TrackpadHaptic { .. } => {
                Ok(())
            }
        }
    }

    pub fn poll_rumble(&mut self) -> Result<Option<(u16, u16)>> {
        let events = match self.gamepad.fetch_events() {
            Ok(events) => events.collect::<Vec<_>>(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(Error::message(error)),
        };
        for event in events {
            match event.destructure() {
                EventSummary::UInput(event, UInputCode::UI_FF_UPLOAD, ..) => {
                    let mut upload = self.gamepad.process_ff_upload(event).whence()?;
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
                    let erase = self.gamepad.process_ff_erase(event).whence()?;
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
}

impl RumbleEffects {
    fn new() -> Self {
        Self {
            effects: [None; 16],
            active: 0,
            last_output: (0, 0),
        }
    }

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
        let mut effects = RumbleEffects::new();
        let id = effects.upload(-1, Some((12, 34))).unwrap();

        effects.set_playback(id, 3);
        assert_eq!(effects.changed_output(), Some((12, 34)));
        effects.set_playback(id, 0);
        assert_eq!(effects.changed_output(), Some((0, 0)));
    }

    #[test]
    fn rejected_upload_does_not_consume_an_effect_id() {
        let mut effects = RumbleEffects::new();

        assert_eq!(effects.upload(-1, None), None);
        assert_eq!(effects.upload(-1, Some((1, 2))), Some(0));
    }
}
