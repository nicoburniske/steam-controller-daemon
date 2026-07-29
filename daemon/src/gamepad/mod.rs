mod dualshock4;
mod xbox;

use scd::Result;
use scd::config::{Gamepad, GamepadButton};

use crate::mapper::GamepadAxis;

use dualshock4::DualShock4;
use xbox::Xbox;

pub enum VirtualGamepad {
    Xbox(Xbox),
    DualShock4(DualShock4),
}

impl VirtualGamepad {
    pub fn new(kind: Gamepad) -> Result<Self> {
        match kind {
            Gamepad::Xbox => Ok(Self::Xbox(Xbox::new()?)),
            Gamepad::DualShock4 => Ok(Self::DualShock4(DualShock4::new()?)),
            Gamepad::None => unreachable!("none has no virtual gamepad"),
        }
    }

    pub fn emit_button(&mut self, button: GamepadButton, pressed: bool) -> Result<()> {
        match self {
            Self::Xbox(gamepad) => gamepad.emit_button(button, pressed),
            Self::DualShock4(gamepad) => {
                gamepad.emit_button(button, pressed);
                Ok(())
            }
        }
    }

    pub fn emit_axis(&mut self, axis: GamepadAxis, value: f32) -> Result<()> {
        match self {
            Self::Xbox(gamepad) => gamepad.emit_axis(axis, value),
            Self::DualShock4(gamepad) => {
                gamepad.emit_axis(axis, value);
                Ok(())
            }
        }
    }

    pub fn poll_rumble(&mut self) -> Result<Option<(u16, u16)>> {
        match self {
            Self::Xbox(gamepad) => gamepad.poll_rumble(),
            Self::DualShock4(gamepad) => gamepad.poll_rumble(),
        }
    }
}
