use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, RelativeAxisCode};

use crate::gamepad::VirtualGamepad;
use crate::mapper::Output;
use scd::Result;
use scd::config::{Gamepad, MouseButton, is_keyboard_key};
use scd::protocol::{ControllerState, Trackpad};

pub struct Outputs {
    gamepad: Option<VirtualGamepad>,
    gamepad_type: Gamepad,
    pending_rumble_stop: bool,
    keyboard: VirtualDevice,
    left_shift_held: bool,
    right_shift_held: bool,
    mouse: VirtualDevice,
    mouse_remainder: [f32; 2],
    scroll_remainder: [f32; 2],
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
            pending_rumble_stop: false,
            keyboard,
            left_shift_held: false,
            right_shift_held: false,
            mouse,
            mouse_remainder: [0.0; 2],
            scroll_remainder: [0.0; 2],
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
            Gamepad::Xbox | Gamepad::DualShock4 => Some(VirtualGamepad::new(gamepad)?),
        };
        self.pending_rumble_stop |= self.gamepad.is_some();
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
                gamepad.emit_button(*button, *pressed)
            }
            Output::GamepadAxis { axis, value } => {
                let Some(gamepad) = &mut self.gamepad else {
                    return Ok(());
                };
                gamepad.emit_axis(*axis, *value)
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
        if self.pending_rumble_stop {
            self.pending_rumble_stop = false;
            return Ok(Some((0, 0)));
        }
        self.gamepad
            .as_mut()
            .map_or(Ok(None), VirtualGamepad::poll_rumble)
    }

    pub fn update_gamepad_source(&mut self, state: &ControllerState, touchpad: Option<Trackpad>) {
        if let Some(gamepad) = &mut self.gamepad {
            gamepad.update_source(state, touchpad);
        }
    }

    pub fn set_gamepad_battery(&mut self, percent: u8) {
        if let Some(VirtualGamepad::DualShock4(gamepad)) = &mut self.gamepad {
            gamepad.set_battery(percent);
        }
    }

    pub fn sync_gamepad(&mut self) -> Result<()> {
        self.gamepad.as_mut().map_or(Ok(()), VirtualGamepad::sync)
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
