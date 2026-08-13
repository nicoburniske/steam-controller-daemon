mod dualshock4;
mod xbox;

use scd::Result;
use scd::config::{Gamepad, GamepadButton};

use crate::mapper::GamepadAxis;

use dualshock4::DualSense;
use xbox::Xbox;

pub enum VirtualGamepad {
    Xbox(Xbox),
    DualSense(DualSense),
}

impl VirtualGamepad {
    pub fn new(kind: Gamepad) -> Result<Self> {
        match kind {
            Gamepad::Xbox => Ok(Self::Xbox(Xbox::new()?)),
            Gamepad::DualShock4 => Ok(Self::DualSense(DualSense::new()?)),
            Gamepad::None => unreachable!("none has no virtual gamepad"),
        }
    }

    pub fn emit_button(&mut self, button: GamepadButton, pressed: bool) -> Result<()> {
        match self {
            Self::Xbox(gamepad) => gamepad.emit_button(button, pressed),
            Self::DualSense(gamepad) => {
                gamepad.emit_button(button, pressed);
                Ok(())
            }
        }
    }

    pub fn emit_axis(&mut self, axis: GamepadAxis, value: f32) -> Result<()> {
        match self {
            Self::Xbox(gamepad) => gamepad.emit_axis(axis, value),
            Self::DualSense(gamepad) => {
                gamepad.emit_axis(axis, value);
                Ok(())
            }
        }
    }

    pub fn poll_rumble(&mut self) -> Result<Option<(u16, u16)>> {
        match self {
            Self::Xbox(gamepad) => gamepad.poll_rumble(),
            Self::DualSense(gamepad) => gamepad.poll_rumble(),
        }
    }

    pub fn tick(&mut self, now: std::time::Instant) -> Result<()> {
        match self {
            Self::Xbox(_) => Ok(()),
            Self::DualSense(gamepad) => gamepad.tick(now),
        }
    }
}
