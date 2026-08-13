//! DualSense emulation through a local Bluetooth-tagged UHID device.
//!
//! The `dualshock4` configuration mode keeps its existing name, but creates a
//! DualSense because native games support it more consistently. No adapter,
//! pairing, or radio is involved. Reports use Bluetooth framing, CRC, and a
//! periodic cadence, while `UHID_CREATE2` and `UHID_DESTROY` bind the device to
//! the selected mode.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use scd::config::GamepadButton;
use scd::protocol::{ControllerState, Trackpad};
use scd::{Error, Result};

use crate::mapper::GamepadAxis;

const UHID_EVENT_SIZE: usize = 4376;
const UHID_DESTROY: u32 = 1;
const UHID_OUTPUT: u32 = 6;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;

const BUS_BLUETOOTH: u16 = 0x0005;
const VENDOR: u32 = 0x054c;
const PRODUCT: u32 = 0x0ce6;
const VERSION: u32 = 0x8111;
const NAME: &[u8] = b"DualSense Wireless Controller";
const PHYS: &[u8] = b"1c:66:6d:07:09:8b/input0";
const UNIQ: &[u8] = b"1c:66:6d:07:09:8b";
const MAC: [u8; 6] = [0x1c, 0x66, 0x6d, 0x07, 0x09, 0x8b];
const REPORT_INTERVAL: Duration = Duration::from_millis(10);

// captured DualSense Bluetooth descriptor used by Inputtino
const REPORT_DESCRIPTOR: &[u8; 279] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35,
    0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00, 0x25,
    0x07, 0x35, 0x00, 0x46, 0x3b, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00,
    0x05, 0x09, 0x19, 0x01, 0x29, 0x0e, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0e, 0x81, 0x02,
    0x75, 0x06, 0x95, 0x01, 0x81, 0x01, 0x05, 0x01, 0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xff,
    0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, 0x06, 0x00, 0xff, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75,
    0x08, 0x95, 0x4d, 0x85, 0x31, 0x09, 0x31, 0x91, 0x02, 0x09, 0x3b, 0x81, 0x02, 0x85, 0x32, 0x09,
    0x32, 0x95, 0x8d, 0x91, 0x02, 0x85, 0x33, 0x09, 0x33, 0x95, 0xcd, 0x91, 0x02, 0x85, 0x34, 0x09,
    0x34, 0x96, 0x0d, 0x01, 0x91, 0x02, 0x85, 0x35, 0x09, 0x35, 0x96, 0x4d, 0x01, 0x91, 0x02, 0x85,
    0x36, 0x09, 0x36, 0x96, 0x8d, 0x01, 0x91, 0x02, 0x85, 0x37, 0x09, 0x37, 0x96, 0xcd, 0x01, 0x91,
    0x02, 0x85, 0x38, 0x09, 0x38, 0x96, 0x0d, 0x02, 0x91, 0x02, 0x85, 0x39, 0x09, 0x39, 0x96, 0x22,
    0x02, 0x91, 0x02, 0x06, 0x80, 0xff, 0x85, 0x05, 0x09, 0x33, 0x95, 0x28, 0xb1, 0x02, 0x85, 0x08,
    0x09, 0x34, 0x95, 0x2f, 0xb1, 0x02, 0x85, 0x09, 0x09, 0x24, 0x95, 0x13, 0xb1, 0x02, 0x85, 0x20,
    0x09, 0x26, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x22, 0x09, 0x40, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x80,
    0x09, 0x28, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x81, 0x09, 0x29, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x82,
    0x09, 0x2a, 0x95, 0x09, 0xb1, 0x02, 0x85, 0x83, 0x09, 0x2b, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf1,
    0x09, 0x31, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf2, 0x09, 0x32, 0x95, 0x0f, 0xb1, 0x02, 0x85, 0xf0,
    0x09, 0x30, 0x95, 0x3f, 0xb1, 0x02, 0xc0,
];

// feature fixtures from Inputtino
const CALIBRATION: &[u8; 41] = &[
    0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x27, 0xf0, 0xd8, 0x10, 0x27, 0xf0, 0xd8, 0x10,
    0x27, 0xf0, 0xd8, 0xf4, 0x01, 0xf4, 0x01, 0x10, 0x27, 0xf0, 0xd8, 0x10, 0x27, 0xf0, 0xd8, 0x10,
    0x27, 0xf0, 0xd8, 0x0b, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const PAIRING: &[u8; 20] = &[
    0x09, 0x74, 0xe7, 0xd6, 0x3a, 0x53, 0x35, 0x08, 0x25, 0x00, 0x1e, 0x00, 0xee, 0x74, 0xd0, 0xbc,
    0x00, 0x00, 0x00, 0x00,
];
const FIRMWARE: &[u8; 64] = &[
    0x20, 0x4a, 0x75, 0x6e, 0x20, 0x31, 0x39, 0x20, 0x32, 0x30, 0x32, 0x33, 0x31, 0x34, 0x3a, 0x34,
    0x37, 0x3a, 0x33, 0x34, 0x03, 0x00, 0x44, 0x00, 0x08, 0x02, 0x00, 0x01, 0x36, 0x00, 0x00, 0x01,
    0xc1, 0xc8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x54, 0x01, 0x00, 0x00,
    0x14, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

pub struct DualSense {
    device: File,
    state: State,
    sequence: u8,
    last_transmission: Instant,
    rumble: (u16, u16),
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
    sensor_timestamp: u32,
    battery_percent: u8,
    battery_charging: bool,
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
            battery_charging: false,
            touchpad_clicked: false,
            touches: [Touch::default(); 2],
        }
    }
}

impl DualSense {
    pub fn new() -> Result<Self> {
        let mut device = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/uhid")?;
        device.write_all(&create_event())?;

        let now = Instant::now();
        let mut gamepad = Self {
            device,
            state: State::default(),
            sequence: 0,
            last_transmission: now.checked_sub(REPORT_INTERVAL).unwrap_or(now),
            rumble: (0, 0),
            reported_rumble: (0, 0),
        };
        gamepad.tick(now)?;
        log::info!(
            "created Bluetooth DualSense {:04x}:{:04x} with {}-byte descriptor",
            VENDOR,
            PRODUCT,
            REPORT_DESCRIPTOR.len()
        );
        Ok(gamepad)
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
                return;
            }
            GamepadButton::RightTrigger => {
                self.state.trigger_buttons[1] = pressed;
                return;
            }
            GamepadButton::DpadUp => {
                self.state.dpad[0] = pressed;
                return;
            }
            GamepadButton::DpadDown => {
                self.state.dpad[1] = pressed;
                return;
            }
            GamepadButton::DpadLeft => {
                self.state.dpad[2] = pressed;
                return;
            }
            GamepadButton::DpadRight => {
                self.state.dpad[3] = pressed;
                return;
            }
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
    }

    pub fn update_source(&mut self, state: &ControllerState, touchpad: Option<Trackpad>) {
        self.state.gyro = state.gyro_raw;
        self.state.accel = state.accel.map(|value| value / 2);
        self.state.sensor_timestamp = state.imu_timestamp_us.wrapping_mul(3);
        let pad = match touchpad {
            Some(Trackpad::Left) => state.left_pad,
            Some(Trackpad::Right) => state.right_pad,
            None => Default::default(),
        };
        self.state.touchpad_clicked = pad.clicked;
        let touch = &mut self.state.touches[0];
        if pad.touched && !touch.active {
            touch.id = touch.id.wrapping_add(1) & 0x7f;
        }
        touch.active = pad.touched;
        touch.x = ((pad.position[0].clamp(-1.0, 1.0) + 1.0) * 0.5 * 1919.0).round() as u16;
        touch.y = ((1.0 - pad.position[1].clamp(-1.0, 1.0)) * 0.5 * 1079.0).round() as u16;
    }

    pub fn set_battery(&mut self, percent: u8, charging: bool) {
        self.state.battery_percent = percent;
        self.state.battery_charging = charging;
    }

    pub fn tick(&mut self, now: Instant) -> Result<()> {
        if now.saturating_duration_since(self.last_transmission) < REPORT_INTERVAL {
            return Ok(());
        }
        let report = self.state.bluetooth_report(self.sequence);
        let mut event = [0; 84];
        event[..4].copy_from_slice(&UHID_INPUT2.to_ne_bytes());
        event[4..6].copy_from_slice(&(report.len() as u16).to_ne_bytes());
        event[6..].copy_from_slice(&report);
        match self.device.write(&event) {
            Ok(size) if size == event.len() => {
                self.sequence = self.sequence.wrapping_add(1);
                if self.sequence == u8::MAX {
                    self.sequence = 0;
                }
                self.last_transmission = now;
                Ok(())
            }
            Ok(size) => Err(Error::message(format_args!(
                "short DualSense UHID input write: {size} bytes"
            ))),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(Error::message(error)),
        }
    }

    pub fn poll_rumble(&mut self) -> Result<Option<(u16, u16)>> {
        let mut event = [0; UHID_EVENT_SIZE];
        loop {
            let size = match self.device.read(&mut event) {
                Ok(0) => return Err(Error::message("/dev/uhid closed")),
                Ok(size) => size,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(Error::message(error)),
            };
            if size < 4 {
                log::warn!("short DualSense UHID event: {size} bytes");
                continue;
            }
            match u32::from_ne_bytes(event[..4].try_into().unwrap()) {
                UHID_OUTPUT => {
                    if size < 4103 {
                        log::warn!("short DualSense UHID output event: {size} bytes");
                        continue;
                    }
                    let report_size =
                        usize::from(u16::from_ne_bytes(event[4100..4102].try_into().unwrap()));
                    if report_size <= 4096
                        && report_size + 4 <= size
                        && let Some(rumble) = parse_output(&event[4..4 + report_size])
                    {
                        self.rumble = rumble;
                    }
                }
                UHID_GET_REPORT => {
                    if size < 10 {
                        log::warn!("short DualSense UHID get-report event: {size} bytes");
                        continue;
                    }
                    let request = u32::from_ne_bytes(event[4..8].try_into().unwrap());
                    let report = feature_report(event[8]);
                    if report.is_none() {
                        log::debug!("unknown DualSense feature report {:#04x}", event[8]);
                    }
                    let (reply, reply_size) = get_report_reply(
                        request,
                        report.as_ref().map(|(data, size)| &data[..*size]),
                    );
                    self.device.write_all(&reply[..reply_size])?;
                }
                UHID_SET_REPORT => {
                    if size < 12 {
                        log::warn!("short DualSense UHID set-report event: {size} bytes");
                        continue;
                    }
                    let request = u32::from_ne_bytes(event[4..8].try_into().unwrap());
                    let declared =
                        usize::from(u16::from_ne_bytes(event[10..12].try_into().unwrap()));
                    let error = if declared > size - 12 {
                        libc::EIO
                    } else {
                        libc::EINVAL
                    };
                    self.device.write_all(&set_report_reply(request, error))?;
                }
                _ => {}
            }
        }
        if self.rumble == self.reported_rumble {
            return Ok(None);
        }
        self.reported_rumble = self.rumble;
        Ok(Some(self.rumble))
    }
}

impl Drop for DualSense {
    fn drop(&mut self) {
        if let Err(error) = self.device.write_all(&UHID_DESTROY.to_ne_bytes())
            && error.kind() != io::ErrorKind::WouldBlock
        {
            log::warn!("failed to destroy DualSense UHID device: {error}");
        }
    }
}

impl State {
    fn bluetooth_report(&self, sequence: u8) -> [u8; 78] {
        let mut report = [0; 78];
        report[0] = 0x31;
        report[2..6].copy_from_slice(&self.sticks);
        report[6..8].copy_from_slice(&self.triggers);
        report[8] = sequence;
        let x = i8::from(self.dpad[3]) - i8::from(self.dpad[2]);
        let y = i8::from(self.dpad[1]) - i8::from(self.dpad[0]);
        report[9] = match (x, y) {
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
        report[10] = ((self.buttons >> 4) & 0xff) as u8;
        if self.trigger_buttons[0] || self.triggers[0] != 0 {
            report[10] |= 1 << 2;
        }
        if self.trigger_buttons[1] || self.triggers[1] != 0 {
            report[10] |= 1 << 3;
        }
        report[11] = ((self.buttons >> 12) & 1) as u8 | (u8::from(self.touchpad_clicked) << 1);
        for (index, value) in self.gyro.iter().chain(&self.accel).enumerate() {
            report[17 + index * 2..19 + index * 2].copy_from_slice(&value.to_le_bytes());
        }
        report[29..33].copy_from_slice(&self.sensor_timestamp.to_le_bytes());
        for (index, touch) in self.touches.iter().enumerate() {
            let offset = 34 + index * 4;
            report[offset] = touch.id | if touch.active { 0 } else { 0x80 };
            report[offset + 1] = touch.x as u8;
            report[offset + 2] = ((touch.x >> 8) as u8 & 0x0f) | ((touch.y as u8 & 0x0f) << 4);
            report[offset + 3] = (touch.y >> 4) as u8;
        }
        let status = if self.battery_percent >= 100 {
            2
        } else if self.battery_charging {
            1
        } else {
            0
        };
        report[54] = (status << 4) | (self.battery_percent.min(100) / 10);
        report[55] = 0x0c;
        let crc = bluetooth_crc(0xa1, &report[..74]);
        report[74..].copy_from_slice(&crc.to_le_bytes());
        report
    }
}

fn create_event() -> [u8; 280 + REPORT_DESCRIPTOR.len()] {
    let mut event = [0; 280 + REPORT_DESCRIPTOR.len()];
    event[..4].copy_from_slice(&UHID_CREATE2.to_ne_bytes());
    event[4..4 + NAME.len()].copy_from_slice(NAME);
    event[132..132 + PHYS.len()].copy_from_slice(PHYS);
    event[196..196 + UNIQ.len()].copy_from_slice(UNIQ);
    event[260..262].copy_from_slice(&(REPORT_DESCRIPTOR.len() as u16).to_ne_bytes());
    event[262..264].copy_from_slice(&BUS_BLUETOOTH.to_ne_bytes());
    event[264..268].copy_from_slice(&VENDOR.to_ne_bytes());
    event[268..272].copy_from_slice(&PRODUCT.to_ne_bytes());
    event[272..276].copy_from_slice(&VERSION.to_ne_bytes());
    event[280..].copy_from_slice(REPORT_DESCRIPTOR);
    event
}

fn feature_report(id: u8) -> Option<([u8; 64], usize)> {
    let mut report = [0; 64];
    let size = match id {
        0x05 => {
            report[..CALIBRATION.len()].copy_from_slice(CALIBRATION);
            CALIBRATION.len()
        }
        0x09 => {
            report[..PAIRING.len()].copy_from_slice(PAIRING);
            for (target, source) in report[1..7].iter_mut().zip(MAC.iter().rev()) {
                *target = *source;
            }
            PAIRING.len()
        }
        0x20 => {
            report.copy_from_slice(FIRMWARE);
            FIRMWARE.len()
        }
        _ => return None,
    };
    let end = size - 4;
    let crc = bluetooth_crc(0xa3, &report[..end]);
    report[end..size].copy_from_slice(&crc.to_le_bytes());
    Some((report, size))
}

fn get_report_reply(request: u32, report: Option<&[u8]>) -> ([u8; 76], usize) {
    let mut reply = [0; 76];
    reply[..4].copy_from_slice(&UHID_GET_REPORT_REPLY.to_ne_bytes());
    reply[4..8].copy_from_slice(&request.to_ne_bytes());
    match report {
        Some(report) => {
            reply[10..12].copy_from_slice(&(report.len() as u16).to_ne_bytes());
            reply[12..12 + report.len()].copy_from_slice(report);
            (reply, 12 + report.len())
        }
        None => {
            reply[8..10].copy_from_slice(&(libc::EINVAL as u16).to_ne_bytes());
            (reply, 12)
        }
    }
}

fn set_report_reply(request: u32, error: i32) -> [u8; 10] {
    let mut reply = [0; 10];
    reply[..4].copy_from_slice(&UHID_SET_REPORT_REPLY.to_ne_bytes());
    reply[4..8].copy_from_slice(&request.to_ne_bytes());
    reply[8..].copy_from_slice(&(error as u16).to_ne_bytes());
    reply
}

fn parse_output(report: &[u8]) -> Option<(u16, u16)> {
    let common =
        if matches!(report.len(), 77 | 78) && report.first() == Some(&0x31) && report[1] & 2 != 0 {
            &report[2..]
        } else if report.len() == 78 && report[0] == 0x31 && report[2] == 0x10 {
            &report[3..]
        } else {
            return None;
        };
    if common[0] & 1 != 0 || common[38] & 4 != 0 {
        Some((u16::from(common[3]) * 257, u16::from(common[2]) * 257))
    } else if common[0] == 0 && common[1] == 0 && common[38] == 0 {
        Some((0, 0))
    } else {
        None
    }
}

fn bluetooth_crc(prefix: u8, data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff;
    for byte in std::iter::once(&prefix).chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_bluetooth_dualsense() {
        let event = create_event();
        assert_eq!(u16::from_ne_bytes(event[262..264].try_into().unwrap()), 5);
        assert_eq!(
            u32::from_ne_bytes(event[264..268].try_into().unwrap()),
            0x054c
        );
        assert_eq!(
            u32::from_ne_bytes(event[268..272].try_into().unwrap()),
            0x0ce6
        );
        assert_eq!(
            u32::from_ne_bytes(event[272..276].try_into().unwrap()),
            0x8111
        );
        assert_eq!(&event[196..213], UNIQ);
        assert_eq!(
            u16::from_ne_bytes(event[260..262].try_into().unwrap()) as usize,
            279
        );
    }

    #[test]
    fn descriptor_declares_full_reports_and_features() {
        assert!(
            REPORT_DESCRIPTOR
                .windows(8)
                .any(|x| x == [0x95, 0x4d, 0x85, 0x31, 0x09, 0x31, 0x91, 0x02])
        );
        assert!(
            REPORT_DESCRIPTOR
                .windows(4)
                .any(|x| x == [0x09, 0x3b, 0x81, 0x02])
        );
        for declaration in [
            [0x85, 0x05, 0x09, 0x33, 0x95, 0x28],
            [0x85, 0x09, 0x09, 0x24, 0x95, 0x13],
            [0x85, 0x20, 0x09, 0x26, 0x95, 0x3f],
        ] {
            assert!(REPORT_DESCRIPTOR.windows(6).any(|x| x == declaration));
        }
    }

    #[test]
    fn packs_mapping_and_bluetooth_frame() {
        let state = State {
            sticks: [0, 64, 192, 255],
            triggers: [0, 127],
            trigger_buttons: [true, false],
            buttons: (1 << 1) | (1 << 3) | (1 << 4) | (1 << 12),
            dpad: [true, false, false, true],
            gyro: [1, -2, 3],
            accel: [-4, 5, 8192],
            sensor_timestamp: 0x1234_5678,
            battery_percent: 50,
            battery_charging: false,
            touchpad_clicked: true,
            touches: [
                Touch {
                    active: true,
                    id: 3,
                    x: 1919,
                    y: 1079,
                },
                Touch::default(),
            ],
        };
        let report = state.bluetooth_report(7);
        assert_eq!(report.len(), 78);
        assert_eq!(&report[..2], &[0x31, 0]);
        assert_eq!(
            &report[2..12],
            &[0, 64, 192, 255, 0, 127, 7, 0xa1, 0x0d, 0x03]
        );
        assert_eq!(
            &report[17..29],
            &[1, 0, 0xfe, 0xff, 3, 0, 0xfc, 0xff, 5, 0, 0, 0x20]
        );
        assert_eq!(&report[29..33], &[0x78, 0x56, 0x34, 0x12]);
        assert_eq!(&report[34..42], &[3, 0x7f, 0x77, 0x43, 0x80, 0, 0, 0]);
        assert_eq!(&report[54..56], &[0x05, 0x0c]);
    }

    #[test]
    fn crc_matches_external_vectors() {
        assert_eq!(bluetooth_crc(0xa1, b"123456789"), 0x88ed_2411);
        let report = State::default().bluetooth_report(0);
        assert_eq!(&report[74..], &[0x24, 0x5a, 0x59, 0xab]);
    }

    #[test]
    fn features_have_identity_and_crc() {
        for (id, length) in [(0x05, 41), (0x09, 20), (0x20, 64)] {
            let (report, size) = feature_report(id).unwrap();
            assert_eq!(size, length);
            assert_eq!(report[0], id);
            let end = size - 4;
            assert_eq!(
                bluetooth_crc(0xa3, &report[..end]).to_le_bytes(),
                report[end..size]
            );
        }
        assert_eq!(
            &feature_report(0x09).unwrap().0[1..7],
            &[0x8b, 0x09, 0x07, 0x6d, 0x66, 0x1c]
        );
        assert!(feature_report(0x08).is_none());
    }

    #[test]
    fn parses_sdl_and_kernel_rumble() {
        let mut direct = [0; 77];
        direct[0] = 0x31;
        direct[1] = 2;
        direct[2] = 1;
        direct[4] = 0x12;
        direct[5] = 0x34;
        assert_eq!(parse_output(&direct), Some((0x3434, 0x1212)));
        let mut padded_direct = [0; 78];
        padded_direct[..77].copy_from_slice(&direct);
        assert_eq!(parse_output(&padded_direct), Some((0x3434, 0x1212)));
        let mut kernel = [0; 78];
        kernel[0] = 0x31;
        kernel[2] = 0x10;
        kernel[3] = 1;
        kernel[5] = 0x12;
        kernel[6] = 0x34;
        assert_eq!(parse_output(&kernel), Some((0x3434, 0x1212)));
        direct[2] = 0;
        direct[3] = 1;
        assert_eq!(parse_output(&direct), None);
        assert_eq!(parse_output(&direct[..76]), None);
    }

    #[test]
    fn reports_battery_status() {
        let mut state = State::default();
        assert_eq!(state.bluetooth_report(0)[54], 0x05);
        state.battery_percent = 60;
        state.battery_charging = true;
        assert_eq!(state.bluetooth_report(0)[54], 0x16);
        state.battery_percent = 100;
        assert_eq!(state.bluetooth_report(0)[54], 0x2a);
    }

    #[test]
    fn replies_preserve_request_ids() {
        let (report, report_size) = feature_report(0x05).unwrap();
        let (get, size) = get_report_reply(0x1234_5678, Some(&report[..report_size]));
        assert_eq!(&get[4..8], &0x1234_5678_u32.to_ne_bytes());
        assert_eq!(size, 53);
        let set = set_report_reply(0x8765_4321, libc::EINVAL);
        assert_eq!(&set[4..8], &0x8765_4321_u32.to_ne_bytes());
    }
}
