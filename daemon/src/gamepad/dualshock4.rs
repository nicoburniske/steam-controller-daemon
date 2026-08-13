//! DualShock 4 emulation through a local Bluetooth-tagged UHID device.
//!
//! USB-tagged UHID did not expose enough USB identity to Wine for native DS4
//! detection. Bluetooth identity does not have that limitation: no adapter,
//! pairing, or radio is involved, but reports use the real Bluetooth framing,
//! CRC, and periodic cadence. `UHID_CREATE2` and `UHID_DESTROY` naturally bind
//! the virtual device lifetime to DS4 mode.

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
const PRODUCT: u32 = 0x09cc;
const VERSION: u32 = 0x0100;
const NAME: &[u8] = b"Wireless Controller";
const PHYS: &[u8] = b"1c:66:6d:07:09:8b/input0";
const UNIQ: &[u8] = b"1c:66:6d:07:09:8b";
const REPORT_INTERVAL: Duration = Duration::from_millis(10);

// captured DS4 Bluetooth descriptor from hid-tools
const REPORT_DESCRIPTOR: &[u8; 364] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35,
    0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00, 0x25,
    0x07, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x05, 0x09, 0x19, 0x01, 0x29, 0x0e, 0x15, 0x00, 0x25,
    0x01, 0x75, 0x01, 0x95, 0x0e, 0x81, 0x02, 0x75, 0x06, 0x95, 0x01, 0x81, 0x01, 0x05, 0x01, 0x09,
    0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, 0x06, 0x04,
    0xff, 0x85, 0x02, 0x09, 0x24, 0x95, 0x24, 0xb1, 0x02, 0x85, 0xa3, 0x09, 0x25, 0x95, 0x30, 0xb1,
    0x02, 0x85, 0x05, 0x09, 0x26, 0x95, 0x28, 0xb1, 0x02, 0x85, 0x06, 0x09, 0x27, 0x95, 0x34, 0xb1,
    0x02, 0x85, 0x07, 0x09, 0x28, 0x95, 0x30, 0xb1, 0x02, 0x85, 0x08, 0x09, 0x29, 0x95, 0x2f, 0xb1,
    0x02, 0x85, 0x09, 0x09, 0x2a, 0x95, 0x13, 0xb1, 0x02, 0x06, 0x03, 0xff, 0x85, 0x03, 0x09, 0x21,
    0x95, 0x26, 0xb1, 0x02, 0x85, 0x04, 0x09, 0x22, 0x95, 0x2e, 0xb1, 0x02, 0x85, 0xf0, 0x09, 0x47,
    0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf1, 0x09, 0x48, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf2, 0x09, 0x49,
    0x95, 0x0f, 0xb1, 0x02, 0x06, 0x00, 0xff, 0x85, 0x11, 0x09, 0x20, 0x15, 0x00, 0x26, 0xff, 0x00,
    0x75, 0x08, 0x95, 0x4d, 0x81, 0x02, 0x09, 0x21, 0x91, 0x02, 0x85, 0x12, 0x09, 0x22, 0x95, 0x8d,
    0x81, 0x02, 0x09, 0x23, 0x91, 0x02, 0x85, 0x13, 0x09, 0x24, 0x95, 0xcd, 0x81, 0x02, 0x09, 0x25,
    0x91, 0x02, 0x85, 0x14, 0x09, 0x26, 0x96, 0x0d, 0x01, 0x81, 0x02, 0x09, 0x27, 0x91, 0x02, 0x85,
    0x15, 0x09, 0x28, 0x96, 0x4d, 0x01, 0x81, 0x02, 0x09, 0x29, 0x91, 0x02, 0x85, 0x16, 0x09, 0x2a,
    0x96, 0x8d, 0x01, 0x81, 0x02, 0x09, 0x2b, 0x91, 0x02, 0x85, 0x17, 0x09, 0x2c, 0x96, 0xcd, 0x01,
    0x81, 0x02, 0x09, 0x2d, 0x91, 0x02, 0x85, 0x18, 0x09, 0x2e, 0x96, 0x0d, 0x02, 0x81, 0x02, 0x09,
    0x2f, 0x91, 0x02, 0x85, 0x19, 0x09, 0x30, 0x96, 0x22, 0x02, 0x81, 0x02, 0x09, 0x31, 0x91, 0x02,
    0x06, 0x80, 0xff, 0x85, 0x82, 0x09, 0x22, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x83, 0x09, 0x23, 0xb1,
    0x02, 0x85, 0x84, 0x09, 0x24, 0xb1, 0x02, 0x85, 0x90, 0x09, 0x30, 0xb1, 0x02, 0x85, 0x91, 0x09,
    0x31, 0xb1, 0x02, 0x85, 0x92, 0x09, 0x32, 0xb1, 0x02, 0x85, 0x93, 0x09, 0x33, 0xb1, 0x02, 0x85,
    0xa0, 0x09, 0x40, 0xb1, 0x02, 0x85, 0xa4, 0x09, 0x44, 0xb1, 0x02, 0xc0,
];

const CALIBRATION_USB: &[u8; 37] = &[
    0x02, 0x1e, 0x00, 0x05, 0x00, 0xe2, 0xff, 0xf2, 0x22, 0x4f, 0xdd, 0xbe, 0x22, 0x4d, 0xdd, 0x8d,
    0x22, 0x39, 0xdd, 0x1c, 0x02, 0x1c, 0x02, 0xe3, 0x1f, 0x8b, 0xdf, 0x8c, 0x1e, 0xb4, 0xde, 0x30,
    0x20, 0x71, 0xe0, 0x10, 0x00,
];
// captured DS4 Bluetooth calibration feature report, including its feature CRC
const CALIBRATION_BT: &[u8; 41] = &[
    0x05, 0x1e, 0x00, 0x05, 0x00, 0xe2, 0xff, 0xf2, 0x22, 0xbe, 0x22, 0x8d, 0x22, 0x4f, 0xdd, 0x4d,
    0xdd, 0x39, 0xdd, 0x1c, 0x02, 0x1c, 0x02, 0xe3, 0x1f, 0x8b, 0xdf, 0x8c, 0x1e, 0xb4, 0xde, 0x30,
    0x20, 0x71, 0xe0, 0x10, 0x00, 0xca, 0xfc, 0x64, 0x4d,
];
const FIRMWARE: &[u8; 49] = &[
    0xa3, 0x41, 0x70, 0x72, 0x20, 0x20, 0x38, 0x20, 0x32, 0x30, 0x31, 0x34, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x30, 0x39, 0x3a, 0x34, 0x36, 0x3a, 0x30, 0x36, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x43, 0x03, 0x00, 0x00, 0x00, 0x51, 0x00, 0x05, 0x00, 0x00, 0x80, 0x03,
    0x00,
];

pub struct DualShock4 {
    device: File,
    state: State,
    counter: u8,
    touch_timestamp: u8,
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
        let mut device = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/uhid")?;
        let event = create_event();
        device.write_all(&event)?;

        let now = Instant::now();
        let mut gamepad = Self {
            device,
            state: State::default(),
            counter: 0,
            touch_timestamp: 0,
            last_transmission: now.checked_sub(REPORT_INTERVAL).unwrap_or(now),
            rumble: (0, 0),
            reported_rumble: (0, 0),
        };
        gamepad.tick(now)?;
        log::info!(
            "created Bluetooth DualShock 4 {:04x}:{:04x} with {}-byte descriptor",
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
    }

    pub fn set_battery(&mut self, percent: u8) {
        self.state.battery_percent = percent;
    }

    pub fn tick(&mut self, now: Instant) -> Result<()> {
        if !transmission_due(self.last_transmission, now) {
            return Ok(());
        }
        let report = self
            .state
            .bluetooth_report(self.counter, self.touch_timestamp);
        let mut event = [0; 84];
        event[..4].copy_from_slice(&UHID_INPUT2.to_ne_bytes());
        event[4..6].copy_from_slice(&(report.len() as u16).to_ne_bytes());
        event[6..].copy_from_slice(&report);
        match self.device.write(&event) {
            Ok(size) if size == event.len() => {
                self.counter = self.counter.wrapping_add(1) & 0x3f;
                self.touch_timestamp = self.touch_timestamp.wrapping_add(1);
                self.last_transmission = now;
                Ok(())
            }
            Ok(size) => Err(Error::message(format_args!(
                "short DualShock UHID input write: {size} bytes"
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
                log::warn!("short DualShock UHID event: {size} bytes");
                continue;
            }
            match u32::from_ne_bytes(event[..4].try_into().unwrap()) {
                UHID_OUTPUT => {
                    if size < 4103 {
                        log::warn!("short DualShock UHID output event: {size} bytes");
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
                        log::warn!("short DualShock UHID get-report event: {size} bytes");
                        continue;
                    }
                    let request = u32::from_ne_bytes(event[4..8].try_into().unwrap());
                    let report = match event[8] {
                        0x02 => Some(CALIBRATION_USB.as_slice()),
                        0x05 => Some(CALIBRATION_BT.as_slice()),
                        0xa3 => Some(FIRMWARE.as_slice()),
                        id => {
                            log::debug!("unknown DualShock feature report {id:#04x}");
                            None
                        }
                    };
                    let (reply, reply_size) = get_report_reply(request, report);
                    self.device.write_all(&reply[..reply_size])?;
                }
                UHID_SET_REPORT => {
                    if size < 12 {
                        log::warn!("short DualShock UHID set-report event: {size} bytes");
                        continue;
                    }
                    let request = u32::from_ne_bytes(event[4..8].try_into().unwrap());
                    let report_id = event[8];
                    let declared =
                        usize::from(u16::from_ne_bytes(event[10..12].try_into().unwrap()));
                    let error = if declared > size - 12 {
                        libc::EIO
                    } else if report_id == 0x11 && output_valid(&event[12..12 + declared]) {
                        if let Some(rumble) = parse_output(&event[12..12 + declared]) {
                            self.rumble = rumble;
                        }
                        0
                    } else {
                        libc::EINVAL
                    };
                    let reply = set_report_reply(request, error);
                    self.device.write_all(&reply)?;
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

impl Drop for DualShock4 {
    fn drop(&mut self) {
        if let Err(error) = self.device.write_all(&UHID_DESTROY.to_ne_bytes())
            && error.kind() != io::ErrorKind::WouldBlock
        {
            log::warn!("failed to destroy DualShock UHID device: {error}");
        }
    }
}

impl State {
    fn common_report(&self, counter: u8) -> [u8; 32] {
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
        report[6] = ((self.buttons >> 12) & 1) as u8
            | (u8::from(self.touchpad_clicked) << 1)
            | ((counter & 0x3f) << 2);
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

    fn bluetooth_report(&self, counter: u8, touch_timestamp: u8) -> [u8; 78] {
        let mut report = [0; 78];
        report[0] = 0x11;
        report[1] = 0xc0;
        report[3..35].copy_from_slice(&self.common_report(counter));
        report[35] = 1;
        report[36] = touch_timestamp;
        for (index, touch) in self.touches.iter().enumerate() {
            let offset = 37 + index * 4;
            report[offset] = touch.id | if touch.active { 0 } else { 0x80 };
            report[offset + 1] = touch.x as u8;
            report[offset + 2] = ((touch.x >> 8) as u8 & 0x0f) | ((touch.y as u8 & 0x0f) << 4);
            report[offset + 3] = (touch.y >> 4) as u8;
        }
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

fn get_report_reply(request: u32, report: Option<&[u8]>) -> ([u8; 61], usize) {
    let mut reply = [0; 61];
    reply[..4].copy_from_slice(&UHID_GET_REPORT_REPLY.to_ne_bytes());
    reply[4..8].copy_from_slice(&request.to_ne_bytes());
    match report {
        Some(report) => {
            reply[10..12].copy_from_slice(&(report.len() as u16).to_ne_bytes());
            reply[12..12 + report.len()].copy_from_slice(report);
            (reply, 12 + report.len())
        }
        None => {
            reply[8..10].copy_from_slice(&(libc::EIO as u16).to_ne_bytes());
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
    if !output_valid(report) || report[3] & 1 == 0 {
        return None;
    }
    Some((u16::from(report[7]) * 257, u16::from(report[6]) * 257))
}

fn output_valid(report: &[u8]) -> bool {
    report.len() == 78
        && report[0] == 0x11
        && report[1] & 0xc0 == 0xc0
        && bluetooth_crc(0xa2, &report[..74])
            == u32::from_le_bytes(report[74..].try_into().unwrap())
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

fn transmission_due(last: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last) >= REPORT_INTERVAL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_bluetooth_ds4_v2() {
        let event = create_event();
        assert_eq!(
            u16::from_ne_bytes(event[262..264].try_into().unwrap()),
            BUS_BLUETOOTH
        );
        assert_eq!(
            u32::from_ne_bytes(event[264..268].try_into().unwrap()),
            0x054c
        );
        assert_eq!(
            u32::from_ne_bytes(event[268..272].try_into().unwrap()),
            0x09cc
        );
        assert_ne!(u32::from_ne_bytes(event[272..276].try_into().unwrap()), 0);
        assert_eq!(&event[196..213], UNIQ);
        assert_eq!(
            u16::from_ne_bytes(event[260..262].try_into().unwrap()) as usize,
            REPORT_DESCRIPTOR.len()
        );
    }

    #[test]
    fn descriptor_declares_implemented_report_sizes() {
        assert_eq!(REPORT_DESCRIPTOR.len(), 364);
        assert!(
            REPORT_DESCRIPTOR
                .windows(10)
                .any(|x| x == [0x85, 0x11, 0x09, 0x20, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75])
        );
        assert!(
            REPORT_DESCRIPTOR
                .windows(4)
                .any(|x| x == [0x95, 0x4d, 0x81, 0x02])
        );
        assert!(
            REPORT_DESCRIPTOR
                .windows(4)
                .any(|x| x == [0x09, 0x21, 0x91, 0x02])
        );
        assert!(
            REPORT_DESCRIPTOR
                .windows(8)
                .any(|x| x == [0x85, 0x05, 0x09, 0x26, 0x95, 0x28, 0xb1, 0x02])
        );
    }

    #[test]
    fn packs_mapping_and_bluetooth_frame() {
        let mut state = State {
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
        state.trigger_buttons[0] = true;
        let report = state.bluetooth_report(3, 7);
        assert_eq!(report.len(), 78);
        assert_eq!(&report[..3], &[0x11, 0xc0, 0]);
        assert_eq!(
            &report[3..12],
            &[0, 64, 192, 255, 0xa1, 0x0d, 0x0f, 255, 127]
        );
        assert_eq!(&report[12..15], &[0x34, 0x12, 25]);
        assert_eq!(
            &report[15..27],
            &[1, 0, 0xfe, 0xff, 3, 0, 0xfc, 0xff, 5, 0, 0, 0x20]
        );
        assert_eq!(report[32], 0x15);
        assert_eq!(&report[35..45], &[1, 7, 3, 0x7f, 0xd7, 0x3a, 0x80, 0, 0, 0]);
    }

    #[test]
    fn input_crc_matches_external_vector() {
        let report = State::default().bluetooth_report(0, 0);
        assert_eq!(&report[74..], &[0x33, 0xee, 0x26, 0xab]);
    }

    #[test]
    fn calibration_matches_captured_crc() {
        assert_eq!(CALIBRATION_BT.len(), 41);
        assert_eq!(
            bluetooth_crc(0xa3, &CALIBRATION_BT[..37]).to_le_bytes(),
            CALIBRATION_BT[37..]
        );
        assert_eq!(&CALIBRATION_BT[37..], &[0xca, 0xfc, 0x64, 0x4d]);
    }

    #[test]
    fn parses_enabled_rumble_only() {
        let mut report = [0; 78];
        report[0] = 0x11;
        report[1] = 0xc0;
        report[3] = 1;
        report[6] = 0x12;
        report[7] = 0x34;
        let crc = bluetooth_crc(0xa2, &report[..74]);
        report[74..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(parse_output(&report), Some((0x3434, 0x1212)));
        report[3] = 0;
        let crc = bluetooth_crc(0xa2, &report[..74]);
        report[74..].copy_from_slice(&crc.to_le_bytes());
        assert!(output_valid(&report));
        assert_eq!(parse_output(&report), None);
        assert_eq!(parse_output(&report[..77]), None);
    }

    #[test]
    fn replies_preserve_request_ids() {
        let (get, size) = get_report_reply(0x1234_5678, Some(CALIBRATION_BT));
        assert_eq!(&get[4..8], &0x1234_5678_u32.to_ne_bytes());
        assert_eq!(size, 53);
        let set = set_report_reply(0x8765_4321, libc::EINVAL);
        assert_eq!(&set[4..8], &0x8765_4321_u32.to_ne_bytes());
    }

    #[test]
    fn schedules_at_ten_millisecond_intervals() {
        let start = Instant::now();
        assert!(!transmission_due(start, start + Duration::from_millis(9)));
        assert!(transmission_due(start, start + Duration::from_millis(10)));
        let sent = start + Duration::from_millis(10);
        assert!(!transmission_due(sent, start + Duration::from_millis(19)));
        assert!(transmission_due(sent, start + Duration::from_millis(20)));
    }
}
