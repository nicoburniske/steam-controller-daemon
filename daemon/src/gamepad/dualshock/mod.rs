//! wired DualShock 4 v2 presented through local USB/IP and `vhci_hcd`

mod usbip;

use scd::Result;
use scd::config::GamepadButton;
use scd::protocol::{ControllerState, Trackpad};

use crate::mapper::GamepadAxis;

use usbip::Transport;

// hid-tools PS4ControllerUSB descriptor
const REPORT_DESCRIPTOR: &[u8; 507] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35,
    0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00, 0x25,
    0x07, 0x35, 0x00, 0x46, 0x3b, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00,
    0x05, 0x09, 0x19, 0x01, 0x29, 0x0e, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0e, 0x81, 0x02,
    0x06, 0x00, 0xff, 0x09, 0x20, 0x75, 0x06, 0x95, 0x01, 0x15, 0x00, 0x25, 0x7f, 0x81, 0x02, 0x05,
    0x01, 0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02,
    0x06, 0x00, 0xff, 0x09, 0x21, 0x95, 0x36, 0x81, 0x02, 0x85, 0x05, 0x09, 0x22, 0x95, 0x1f, 0x91,
    0x02, 0x85, 0x04, 0x09, 0x23, 0x95, 0x24, 0xb1, 0x02, 0x85, 0x02, 0x09, 0x24, 0x95, 0x24, 0xb1,
    0x02, 0x85, 0x08, 0x09, 0x25, 0x95, 0x03, 0xb1, 0x02, 0x85, 0x10, 0x09, 0x26, 0x95, 0x04, 0xb1,
    0x02, 0x85, 0x11, 0x09, 0x27, 0x95, 0x02, 0xb1, 0x02, 0x85, 0x12, 0x06, 0x02, 0xff, 0x09, 0x21,
    0x95, 0x0f, 0xb1, 0x02, 0x85, 0x13, 0x09, 0x22, 0x95, 0x16, 0xb1, 0x02, 0x85, 0x14, 0x06, 0x05,
    0xff, 0x09, 0x20, 0x95, 0x10, 0xb1, 0x02, 0x85, 0x15, 0x09, 0x21, 0x95, 0x2c, 0xb1, 0x02, 0x06,
    0x80, 0xff, 0x85, 0x80, 0x09, 0x20, 0x95, 0x06, 0xb1, 0x02, 0x85, 0x81, 0x09, 0x21, 0x95, 0x06,
    0xb1, 0x02, 0x85, 0x82, 0x09, 0x22, 0x95, 0x05, 0xb1, 0x02, 0x85, 0x83, 0x09, 0x23, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0x84, 0x09, 0x24, 0x95, 0x04, 0xb1, 0x02, 0x85, 0x85, 0x09, 0x25, 0x95, 0x06,
    0xb1, 0x02, 0x85, 0x86, 0x09, 0x26, 0x95, 0x06, 0xb1, 0x02, 0x85, 0x87, 0x09, 0x27, 0x95, 0x23,
    0xb1, 0x02, 0x85, 0x88, 0x09, 0x28, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x89, 0x09, 0x29, 0x95, 0x02,
    0xb1, 0x02, 0x85, 0x90, 0x09, 0x30, 0x95, 0x05, 0xb1, 0x02, 0x85, 0x91, 0x09, 0x31, 0x95, 0x03,
    0xb1, 0x02, 0x85, 0x92, 0x09, 0x32, 0x95, 0x03, 0xb1, 0x02, 0x85, 0x93, 0x09, 0x33, 0x95, 0x0c,
    0xb1, 0x02, 0x85, 0x94, 0x09, 0x34, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xa0, 0x09, 0x40, 0x95, 0x06,
    0xb1, 0x02, 0x85, 0xa1, 0x09, 0x41, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xa2, 0x09, 0x42, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0xa3, 0x09, 0x43, 0x95, 0x30, 0xb1, 0x02, 0x85, 0xa4, 0x09, 0x44, 0x95, 0x0d,
    0xb1, 0x02, 0x85, 0xf0, 0x09, 0x47, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf1, 0x09, 0x48, 0x95, 0x3f,
    0xb1, 0x02, 0x85, 0xf2, 0x09, 0x49, 0x95, 0x0f, 0xb1, 0x02, 0x85, 0xa7, 0x09, 0x4a, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0xa8, 0x09, 0x4b, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xa9, 0x09, 0x4c, 0x95, 0x08,
    0xb1, 0x02, 0x85, 0xaa, 0x09, 0x4e, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xab, 0x09, 0x4f, 0x95, 0x39,
    0xb1, 0x02, 0x85, 0xac, 0x09, 0x50, 0x95, 0x39, 0xb1, 0x02, 0x85, 0xad, 0x09, 0x51, 0x95, 0x0b,
    0xb1, 0x02, 0x85, 0xae, 0x09, 0x52, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xaf, 0x09, 0x53, 0x95, 0x02,
    0xb1, 0x02, 0x85, 0xb0, 0x09, 0x54, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xe0, 0x09, 0x57, 0x95, 0x02,
    0xb1, 0x02, 0x85, 0xb3, 0x09, 0x55, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xb4, 0x09, 0x55, 0x95, 0x3f,
    0xb1, 0x02, 0x85, 0xb5, 0x09, 0x56, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xd0, 0x09, 0x58, 0x95, 0x3f,
    0xb1, 0x02, 0x85, 0xd4, 0x09, 0x59, 0x95, 0x3f, 0xb1, 0x02, 0xc0,
];

const CALIBRATION: &[u8; 37] = &[
    0x02, 0x1e, 0x00, 0x05, 0x00, 0xe2, 0xff, 0xf2, 0x22, 0x4f, 0xdd, 0xbe, 0x22, 0x4d, 0xdd, 0x8d,
    0x22, 0x39, 0xdd, 0x1c, 0x02, 0x1c, 0x02, 0xe3, 0x1f, 0x8b, 0xdf, 0x8c, 0x1e, 0xb4, 0xde, 0x30,
    0x20, 0x71, 0xe0, 0x10, 0x00,
];
const PAIRING: &[u8; 16] = &[
    0x12, 0x8b, 0x09, 0x07, 0x6d, 0x66, 0x1c, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const FIRMWARE: &[u8; 49] = &[
    0xa3, 0x41, 0x70, 0x72, 0x20, 0x20, 0x38, 0x20, 0x32, 0x30, 0x31, 0x34, 0, 0, 0, 0, 0, 0x30,
    0x39, 0x3a, 0x34, 0x36, 0x3a, 0x30, 0x36, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0x43, 3, 0, 0, 0,
    0x51, 0, 5, 0, 0, 0x80, 3, 0,
];

pub struct DualShock4 {
    transport: Transport,
    state: State,
    touch_timestamp: u8,
    reported_rumble: (u16, u16),
}

struct State {
    sticks: [u8; 4],
    triggers: [u8; 2],
    trigger_buttons: [bool; 2],
    buttons: u16,
    dpad: [bool; 4],
    gyro: [i16; 3],
    accel: [i16; 3],
    sensor_timestamp: u16,
    battery_percent: u8,
    touchpad_clicked: bool,
    touches: [Touch; 2],
}

#[derive(Clone, Copy, Default)]
struct Touch {
    active: bool,
    id: u8,
    x: u16,
    y: u16,
}

impl Default for State {
    fn default() -> Self {
        Self {
            sticks: [128; 4],
            triggers: [0; 2],
            trigger_buttons: [false; 2],
            buttons: 0,
            dpad: [false; 4],
            gyro: [0; 3],
            accel: [0, 0, 8192],
            sensor_timestamp: 0,
            battery_percent: 50,
            touchpad_clicked: false,
            touches: [Touch::default(); 2],
        }
    }
}

impl DualShock4 {
    pub fn new() -> Result<Self> {
        let state = State::default();
        let transport = Transport::new(state.usb_report(0))?;
        log::info!(
            "created wired DualShock 4 054c:09cc with {}-byte descriptor",
            REPORT_DESCRIPTOR.len()
        );
        Ok(Self {
            transport,
            state,
            touch_timestamp: 0,
            reported_rumble: (0, 0),
        })
    }

    pub fn emit_button(&mut self, button: GamepadButton, pressed: bool) {
        let bit = match button {
            GamepadButton::West => 0,
            GamepadButton::South => 1,
            GamepadButton::East => 2,
            GamepadButton::North => 3,
            GamepadButton::LeftBumper => 4,
            GamepadButton::RightBumper => 5,
            GamepadButton::Select => 8,
            GamepadButton::Start => 9,
            GamepadButton::LeftStick => 10,
            GamepadButton::RightStick => 11,
            GamepadButton::Guide => 12,
            GamepadButton::LeftTrigger => {
                self.state.trigger_buttons[0] = pressed;
                self.update_report();
                return;
            }
            GamepadButton::RightTrigger => {
                self.state.trigger_buttons[1] = pressed;
                self.update_report();
                return;
            }
            GamepadButton::DpadUp => return self.set_dpad(0, pressed),
            GamepadButton::DpadDown => return self.set_dpad(1, pressed),
            GamepadButton::DpadLeft => return self.set_dpad(2, pressed),
            GamepadButton::DpadRight => return self.set_dpad(3, pressed),
            GamepadButton::PaddleLeftUpper
            | GamepadButton::PaddleLeftLower
            | GamepadButton::PaddleRightUpper
            | GamepadButton::PaddleRightLower => return,
        };
        if pressed {
            self.state.buttons |= 1 << bit;
        } else {
            self.state.buttons &= !(1 << bit);
        }
        self.update_report();
    }

    pub fn emit_axis(&mut self, axis: GamepadAxis, value: f32) {
        let value = match axis {
            GamepadAxis::LeftTrigger | GamepadAxis::RightTrigger => {
                (value.clamp(0.0, 1.0) * 255.0).round() as u8
            }
            _ => ((value.clamp(-1.0, 1.0) + 1.0) * 127.5).round() as u8,
        };
        match axis {
            GamepadAxis::LeftX => self.state.sticks[0] = value,
            GamepadAxis::LeftY => self.state.sticks[1] = value,
            GamepadAxis::RightX => self.state.sticks[2] = value,
            GamepadAxis::RightY => self.state.sticks[3] = value,
            GamepadAxis::LeftTrigger => self.state.triggers[0] = value,
            GamepadAxis::RightTrigger => self.state.triggers[1] = value,
        }
        self.update_report();
    }

    pub fn update_source(&mut self, state: &ControllerState, touchpad: Option<Trackpad>) {
        self.state.gyro = state.gyro_raw;
        self.state.accel = state.accel.map(|value| value / 2);
        self.state.sensor_timestamp = (u64::from(state.imu_timestamp_us) * 3 / 16) as u16;
        let pad = match touchpad {
            Some(Trackpad::Left) => state.left_pad,
            Some(Trackpad::Right) => state.right_pad,
            None => Default::default(),
        };
        self.state.touchpad_clicked = pad.clicked;
        self.touch_timestamp = (state
            .trackpad_timestamp_us
            .unwrap_or(state.imu_timestamp_us)
            / 1000) as u8;
        let touch = &mut self.state.touches[0];
        if pad.touched && !touch.active {
            touch.id = touch.id.wrapping_add(1) & 0x7f;
        }
        touch.active = pad.touched;
        touch.x = ((pad.position[0].clamp(-1.0, 1.0) + 1.0) * 0.5 * 1919.0).round() as u16;
        touch.y = ((1.0 - pad.position[1].clamp(-1.0, 1.0)) * 0.5 * 941.0).round() as u16;
        self.update_report();
    }

    pub fn set_battery(&mut self, percent: u8) {
        self.state.battery_percent = percent;
        self.update_report();
    }

    pub fn poll_rumble(&mut self) -> Result<Option<(u16, u16)>> {
        let rumble = self.transport.rumble()?;
        if rumble == self.reported_rumble {
            return Ok(None);
        }
        self.reported_rumble = rumble;
        Ok(Some(rumble))
    }

    fn set_dpad(&mut self, index: usize, pressed: bool) {
        self.state.dpad[index] = pressed;
        self.update_report();
    }

    fn update_report(&self) {
        self.transport
            .set_report(self.state.usb_report(self.touch_timestamp));
    }
}

impl State {
    fn common_report(&self) -> [u8; 32] {
        let mut report = [0; 32];
        report[..4].copy_from_slice(&self.sticks);
        let x = i8::from(self.dpad[3]) - i8::from(self.dpad[2]);
        let y = i8::from(self.dpad[1]) - i8::from(self.dpad[0]);
        report[4] = match (x, y) {
            (0, -1) => 0,
            (1, -1) => 1,
            (1, 0) => 2,
            (1, 1) => 3,
            (0, 1) => 4,
            (-1, 1) => 5,
            (-1, 0) => 6,
            (-1, -1) => 7,
            _ => 8,
        } | ((self.buttons as u8 & 0x0f) << 4);
        report[5] = ((self.buttons >> 4) & 0xff) as u8;
        if self.trigger_buttons[0] || self.triggers[0] != 0 {
            report[5] |= 1 << 2;
        }
        if self.trigger_buttons[1] || self.triggers[1] != 0 {
            report[5] |= 1 << 3;
        }
        report[6] = ((self.buttons >> 12) & 1) as u8 | (u8::from(self.touchpad_clicked) << 1);
        report[7] = if self.trigger_buttons[0] {
            255
        } else {
            self.triggers[0]
        };
        report[8] = if self.trigger_buttons[1] {
            255
        } else {
            self.triggers[1]
        };
        report[9..11].copy_from_slice(&self.sensor_timestamp.to_le_bytes());
        report[11] = 25;
        for (index, value) in self.gyro.iter().chain(&self.accel).enumerate() {
            report[12 + index * 2..14 + index * 2].copy_from_slice(&value.to_le_bytes());
        }
        report[29] = 0x10
            | if self.battery_percent >= 100 {
                11
            } else {
                self.battery_percent / 10
            };
        report
    }

    fn usb_report(&self, touch_timestamp: u8) -> [u8; 64] {
        let mut report = [0; 64];
        report[0] = 0x01;
        report[1..33].copy_from_slice(&self.common_report());
        report[33] = 1;
        report[34] = touch_timestamp;
        for (index, touch) in self.touches.iter().enumerate() {
            let offset = 35 + index * 4;
            report[offset] = touch.id | if touch.active { 0 } else { 0x80 };
            report[offset + 1] = touch.x as u8;
            report[offset + 2] = ((touch.x >> 8) as u8 & 0x0f) | ((touch.y as u8 & 0x0f) << 4);
            report[offset + 3] = (touch.y >> 4) as u8;
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_declares_wired_reports() {
        assert_eq!(REPORT_DESCRIPTOR.len(), 507);
        assert!(
            REPORT_DESCRIPTOR
                .windows(4)
                .any(|x| x == [0x95, 0x36, 0x81, 0x02])
        );
        assert!(
            REPORT_DESCRIPTOR
                .windows(8)
                .any(|x| x == [0x85, 0x05, 0x09, 0x22, 0x95, 0x1f, 0x91, 0x02])
        );
    }

    #[test]
    fn packs_wired_mapping() {
        let state = State {
            sticks: [0, 64, 192, 255],
            triggers: [0, 127],
            trigger_buttons: [true, false],
            buttons: (1 << 1) | (1 << 3) | (1 << 4) | (1 << 12),
            dpad: [true, false, false, true],
            gyro: [1, -2, 3],
            accel: [-4, 5, 8192],
            sensor_timestamp: 0x1234,
            battery_percent: 50,
            touchpad_clicked: true,
            touches: [
                Touch {
                    active: true,
                    id: 3,
                    x: 1919,
                    y: 941,
                },
                Touch::default(),
            ],
        };
        let report = state.usb_report(7);
        assert_eq!(report[0], 1);
        assert_eq!(
            &report[1..10],
            &[0, 64, 192, 255, 0xa1, 0x0d, 0x03, 255, 127]
        );
        assert_eq!(&report[10..13], &[0x34, 0x12, 25]);
        assert_eq!(
            &report[13..25],
            &[1, 0, 0xfe, 0xff, 3, 0, 0xfc, 0xff, 5, 0, 0, 0x20]
        );
        assert_eq!(report[30], 0x15);
        assert_eq!(&report[33..43], &[1, 7, 3, 0x7f, 0xd7, 0x3a, 0x80, 0, 0, 0]);
    }
}
