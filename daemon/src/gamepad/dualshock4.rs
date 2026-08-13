use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use scd::config::GamepadButton;
use scd::protocol::{ControllerState, Trackpad};
use scd::{Error, Result};

use crate::mapper::GamepadAxis;

const GADGET_PATH: &str = "/sys/kernel/config/usb_gadget/scd-dualshock";
const UDC_PATH: &str = "/sys/kernel/config/usb_gadget/scd-dualshock/UDC";
const GADGET_HID_READ_GET_REPORT_ID: libc::c_ulong = 0x8001_6741;
const GADGET_HID_WRITE_GET_REPORT: libc::c_ulong = 0x4048_6742;

const REPORT_DESCRIPTOR: &[u8; 531] = &[
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
    0xb1, 0x02, 0x85, 0x88, 0x09, 0x28, 0x95, 0x22, 0xb1, 0x02, 0x85, 0x89, 0x09, 0x29, 0x95, 0x02,
    0xb1, 0x02, 0x85, 0x90, 0x09, 0x30, 0x95, 0x05, 0xb1, 0x02, 0x85, 0x91, 0x09, 0x31, 0x95, 0x03,
    0xb1, 0x02, 0x85, 0x92, 0x09, 0x32, 0x95, 0x03, 0xb1, 0x02, 0x85, 0x93, 0x09, 0x33, 0x95, 0x0c,
    0xb1, 0x02, 0x85, 0xa0, 0x09, 0x40, 0x95, 0x06, 0xb1, 0x02, 0x85, 0xa1, 0x09, 0x41, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0xa2, 0x09, 0x42, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xa3, 0x09, 0x43, 0x95, 0x30,
    0xb1, 0x02, 0x85, 0xa4, 0x09, 0x44, 0x95, 0x0d, 0xb1, 0x02, 0x85, 0xa5, 0x09, 0x45, 0x95, 0x15,
    0xb1, 0x02, 0x85, 0xa6, 0x09, 0x46, 0x95, 0x15, 0xb1, 0x02, 0x85, 0xf0, 0x09, 0x47, 0x95, 0x3f,
    0xb1, 0x02, 0x85, 0xf1, 0x09, 0x48, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf2, 0x09, 0x49, 0x95, 0x0f,
    0xb1, 0x02, 0x85, 0xa7, 0x09, 0x4a, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xa8, 0x09, 0x4b, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0xa9, 0x09, 0x4c, 0x95, 0x08, 0xb1, 0x02, 0x85, 0xaa, 0x09, 0x4e, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0xab, 0x09, 0x4f, 0x95, 0x39, 0xb1, 0x02, 0x85, 0xac, 0x09, 0x50, 0x95, 0x39,
    0xb1, 0x02, 0x85, 0xad, 0x09, 0x51, 0x95, 0x0b, 0xb1, 0x02, 0x85, 0xae, 0x09, 0x52, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0xaf, 0x09, 0x53, 0x95, 0x02, 0xb1, 0x02, 0x85, 0xb0, 0x09, 0x54, 0x95, 0x3f,
    0xb1, 0x02, 0x85, 0xb1, 0x09, 0x55, 0x95, 0x02, 0xb1, 0x02, 0x85, 0xb2, 0x09, 0x56, 0x95, 0x02,
    0xb1, 0x02, 0x85, 0xe0, 0x09, 0x57, 0x95, 0x02, 0xb1, 0x02, 0x85, 0xb3, 0x09, 0x55, 0x95, 0x3f,
    0xb1, 0x02, 0x85, 0xb4, 0x09, 0x55, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xe1, 0x09, 0x57, 0x95, 0x30,
    0xb1, 0x02, 0x85, 0xe2, 0x09, 0x57, 0x95, 0x10, 0xb1, 0x02, 0x85, 0xe3, 0x09, 0x57, 0x95, 0x04,
    0xb1, 0x02, 0xc0,
];

const CALIBRATION_REPORT: &[u8; 37] = &[
    0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x87, 0x22, 0x7b, 0xdd, 0xb2, 0x22, 0x47, 0xdd, 0xbd,
    0x22, 0x43, 0xdd, 0x1c, 0x02, 0x1c, 0x02, 0x7f, 0x1e, 0x2e, 0xdf, 0x60, 0x1f, 0x4c, 0xe0, 0x3a,
    0x1d, 0xc6, 0xde, 0x08, 0x00,
];
const PAIRING_REPORT: &[u8; 16] = &[
    0x12, 0x8b, 0x09, 0x07, 0x6d, 0x66, 0x1c, 0x08, 0x25, 0x00, 0xac, 0x9e, 0x17, 0x94, 0x05, 0xb0,
];
const MAC_REPORT: &[u8; 7] = &[0x81, 0x8b, 0x09, 0x07, 0x6d, 0x66, 0x1c];
const ADAPTER_CONTROLLER_REPORT: &[u8; 5] = &[0xe3, 0x4c, 0x05, 0xcc, 0x09];
const FIRMWARE_REPORT: &[u8; 49] = &[
    0xa3, 0x41, 0x75, 0x67, 0x20, 0x20, 0x33, 0x20, 0x32, 0x30, 0x31, 0x33, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x30, 0x37, 0x3a, 0x30, 0x31, 0x3a, 0x31, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x31, 0x03, 0x00, 0x00, 0x00, 0x49, 0x00, 0x05, 0x00, 0x00, 0x80, 0x03,
    0x00,
];

pub fn setup() -> Result<()> {
    if Path::new(GADGET_PATH).exists() {
        unbind()?;
        let link = format!("{GADGET_PATH}/configs/c.1/hid.usb0");
        if let Err(error) = fs::remove_file(&link)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(Error::message(error));
        }
        for directory in [
            "configs/c.1/strings/0x409",
            "configs/c.1",
            "functions/hid.usb0",
            "strings/0x409",
            "",
        ] {
            let path = if directory.is_empty() {
                GADGET_PATH.to_owned()
            } else {
                format!("{GADGET_PATH}/{directory}")
            };
            if let Err(error) = fs::remove_dir(&path)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(Error::message(format_args!(
                    "failed to remove stale DualShock gadget {path}: {error}"
                )));
            }
        }
    }

    fs::create_dir(GADGET_PATH)?;
    fs::write(format!("{GADGET_PATH}/idVendor"), b"0x054c")?;
    fs::write(format!("{GADGET_PATH}/idProduct"), b"0x0ba0")?;
    fs::write(format!("{GADGET_PATH}/bcdDevice"), b"0x0100")?;
    fs::write(format!("{GADGET_PATH}/bcdUSB"), b"0x0200")?;

    fs::create_dir(format!("{GADGET_PATH}/strings/0x409"))?;
    fs::write(
        format!("{GADGET_PATH}/strings/0x409/manufacturer"),
        b"Sony Interactive Entertainment",
    )?;
    fs::write(
        format!("{GADGET_PATH}/strings/0x409/product"),
        b"DUALSHOCK 4 USB Wireless Adaptor",
    )?;
    fs::write(
        format!("{GADGET_PATH}/strings/0x409/serialnumber"),
        b"1c660d07098b",
    )?;

    fs::create_dir(format!("{GADGET_PATH}/configs/c.1"))?;
    fs::write(format!("{GADGET_PATH}/configs/c.1/MaxPower"), b"500")?;
    fs::create_dir(format!("{GADGET_PATH}/configs/c.1/strings/0x409"))?;
    fs::write(
        format!("{GADGET_PATH}/configs/c.1/strings/0x409/configuration"),
        b"DualShock 4 wireless adapter",
    )?;

    fs::create_dir(format!("{GADGET_PATH}/functions/hid.usb0"))?;
    fs::write(format!("{GADGET_PATH}/functions/hid.usb0/protocol"), b"0")?;
    fs::write(format!("{GADGET_PATH}/functions/hid.usb0/subclass"), b"0")?;
    fs::write(
        format!("{GADGET_PATH}/functions/hid.usb0/report_length"),
        b"64",
    )?;
    let no_out_endpoint = format!("{GADGET_PATH}/functions/hid.usb0/no_out_endpoint");
    if Path::new(&no_out_endpoint).exists() {
        fs::write(no_out_endpoint, b"0")?;
    }
    fs::write(
        format!("{GADGET_PATH}/functions/hid.usb0/report_desc"),
        REPORT_DESCRIPTOR,
    )?;
    std::os::unix::fs::symlink(
        "../../functions/hid.usb0",
        format!("{GADGET_PATH}/configs/c.1/hid.usb0"),
    )?;
    log::info!(
        "configured unbound DualShock 4 USB gadget 054c:0ba0 version 0100 with {}-byte descriptor",
        REPORT_DESCRIPTOR.len()
    );
    Ok(())
}

pub fn unbind() -> Result<()> {
    if !Path::new(UDC_PATH).exists() {
        return Ok(());
    }
    fs::write(UDC_PATH, b"\n")?;
    log::info!("unbound DualShock 4 USB gadget");
    Ok(())
}

pub struct DualShock4 {
    device: File,
    state: State,
    counter: u8,
    touch_timestamp: u8,
    rumble: (u16, u16),
    reported_rumble: (u16, u16),
}

#[repr(C)]
struct HidReport {
    report_id: u8,
    userspace_req: u8,
    length: u16,
    data: [u8; 64],
    padding: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<HidReport>() == 72);

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
        let mut udc: Option<OsString> = None;
        for entry in fs::read_dir("/sys/class/udc")? {
            let name = entry?.file_name();
            if name.as_encoded_bytes().starts_with(b"dummy_udc.") {
                if udc.is_some() {
                    return Err(Error::message(
                        "multiple dummy USB device controllers found",
                    ));
                }
                udc = Some(name);
            }
        }
        let udc = udc.ok_or_else(|| Error::message("no dummy USB device controller found"))?;
        fs::write(UDC_PATH, udc.as_encoded_bytes())?;

        let deadline = Instant::now() + Duration::from_secs(2);
        let device = loop {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open("/dev/hidg0")
            {
                Ok(device) => break device,
                Err(error)
                    if Instant::now() < deadline
                        && matches!(
                            error.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                        ) =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    if let Err(unbind_error) = fs::write(UDC_PATH, b"\n") {
                        log::error!("failed to unbind DualShock USB gadget: {unbind_error}");
                    }
                    return Err(Error::message(error));
                }
            }
        };
        log::info!(
            "bound DualShock 4 wireless adapter to {}",
            udc.to_string_lossy()
        );

        let mut gamepad = Self {
            device,
            state: State::default(),
            counter: 0,
            touch_timestamp: 0,
            rumble: (0, 0),
            reported_rumble: (0, 0),
        };
        gamepad.sync()?;
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

    pub fn sync(&mut self) -> Result<()> {
        let report = self.state.report(self.counter, self.touch_timestamp);
        match self.device.write(&report) {
            Ok(size) if size == report.len() => {
                self.counter = self.counter.wrapping_add(1) & 0x3f;
                log::info!("DualShock input report: {report:02x?}");
                Ok(())
            }
            Ok(size) => Err(Error::message(format_args!(
                "short DualShock input report write: {size} bytes"
            ))),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || matches!(
                        error.raw_os_error(),
                        Some(libc::ESHUTDOWN | libc::ENODEV | libc::EPIPE)
                    ) =>
            {
                log::info!("DualShock input report deferred: {error}");
                Ok(())
            }
            Err(error) => Err(Error::message(error)),
        }
    }

    pub fn poll_rumble(&mut self) -> Result<Option<(u16, u16)>> {
        loop {
            let mut pollfd = libc::pollfd {
                fd: self.device.as_raw_fd(),
                events: libc::POLLIN | libc::POLLPRI,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pollfd, 1, 0) };
            if ready < 0 {
                return Err(Error::message(io::Error::last_os_error()));
            }
            if ready == 0 {
                break;
            }
            if pollfd.revents & libc::POLLPRI != 0 {
                let mut report_id = 0_u8;
                if unsafe {
                    libc::ioctl(
                        self.device.as_raw_fd(),
                        GADGET_HID_READ_GET_REPORT_ID,
                        &mut report_id,
                    )
                } < 0
                {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::WouldBlock {
                        return Err(Error::message(error));
                    }
                } else {
                    let mut data = [0; 64];
                    let report: &[u8] = match report_id {
                        0x02 => CALIBRATION_REPORT,
                        0x12 => PAIRING_REPORT,
                        0x81 => MAC_REPORT,
                        0xa3 => FIRMWARE_REPORT,
                        0xe1 => {
                            data[..FIRMWARE_REPORT.len()].copy_from_slice(FIRMWARE_REPORT);
                            data[0] = 0xe1;
                            &data[..FIRMWARE_REPORT.len()]
                        }
                        0xe3 => ADAPTER_CONTROLLER_REPORT,
                        report => {
                            let length = match report {
                                0x04 => 37,
                                0x08 => 4,
                                0x11 | 0x89 | 0xaf | 0xb1 | 0xb2 | 0xe0 => 3,
                                0x10 | 0x84 => 5,
                                0xf2 => 16,
                                0x13 => 23,
                                0x14 => 17,
                                0x15 => 45,
                                0x80 | 0x85 | 0x86 | 0xa0 => 7,
                                0x82 => 6,
                                0x83 | 0xa1 | 0xa2 | 0xa7 | 0xa8 | 0xaa | 0xae => 2,
                                0x87 => 36,
                                0x88 => 35,
                                0x90 => 6,
                                0x91 | 0x92 => 4,
                                0x93 => 13,
                                0xa4 => 14,
                                0xa5 | 0xa6 => 22,
                                0xa9 => 9,
                                0xab | 0xac => 58,
                                0xad => 12,
                                0xb0 | 0xb3 | 0xb4 | 0xf0 | 0xf1 => 64,
                                0xe2 => 17,
                                _ => {
                                    log::info!("unknown DualShock get report number={report:02x}");
                                    1
                                }
                            };
                            data[0] = report;
                            &data[..length]
                        }
                    };
                    let mut response = HidReport {
                        report_id,
                        userspace_req: 1,
                        length: report.len() as u16,
                        data: [0; 64],
                        padding: [0; 4],
                    };
                    response.data[..report.len()].copy_from_slice(report);
                    if unsafe {
                        libc::ioctl(
                            self.device.as_raw_fd(),
                            GADGET_HID_WRITE_GET_REPORT,
                            &response,
                        )
                    } < 0
                    {
                        return Err(Error::message(io::Error::last_os_error()));
                    }
                    log::info!("DualShock get report number={report_id:02x} data={report:02x?}");
                }
            }
            if pollfd.revents & libc::POLLIN != 0 {
                let mut report = [0; 64];
                match self.device.read(&mut report) {
                    Ok(0) => return Err(Error::message("/dev/hidg0 closed")),
                    Ok(size) => {
                        log::info!("DualShock output report: {:02x?}", &report[..size]);
                        if size >= 6 && report[0] == 0x05 && report[1] & 1 != 0 {
                            self.rumble = (u16::from(report[5]) * 257, u16::from(report[4]) * 257);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(Error::message(error)),
                }
            }
            if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                log::info!("DualShock USB gadget poll error: {:#x}", pollfd.revents);
                break;
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
        if let Err(error) = fs::write(UDC_PATH, b"\n")
            && error.kind() != io::ErrorKind::NotFound
        {
            log::error!("failed to unbind DualShock USB gadget: {error}");
        } else {
            log::info!("unbound DualShock 4 wireless adapter");
        }
    }
}

impl State {
    fn report(&self, counter: u8, touch_timestamp: u8) -> [u8; 64] {
        let mut report = [0; 64];
        report[0] = 1;
        report[1..5].copy_from_slice(&self.sticks);
        let x = i8::from(self.dpad[3]) - i8::from(self.dpad[2]);
        let y = i8::from(self.dpad[1]) - i8::from(self.dpad[0]);
        report[5] = match (x, y) {
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
        report[6] = ((self.buttons >> 4) & 0xff) as u8;
        if self.trigger_buttons[0] || self.triggers[0] != 0 {
            report[6] |= 1 << 2;
        }
        if self.trigger_buttons[1] || self.triggers[1] != 0 {
            report[6] |= 1 << 3;
        }
        report[7] = ((self.buttons >> 12) & 1) as u8
            | (u8::from(self.touchpad_clicked) << 1)
            | ((counter & 0x3f) << 2);
        report[8] = if self.trigger_buttons[0] {
            255
        } else {
            self.triggers[0]
        };
        report[9] = if self.trigger_buttons[1] {
            255
        } else {
            self.triggers[1]
        };
        report[10..12].copy_from_slice(&self.sensor_timestamp.to_le_bytes());
        report[12] = 25;
        for (index, value) in self.gyro.iter().chain(&self.accel).enumerate() {
            report[13 + index * 2..15 + index * 2].copy_from_slice(&value.to_le_bytes());
        }
        report[30] = 0x10
            | if self.battery_percent >= 100 {
                11
            } else {
                self.battery_percent / 10
            };
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
    fn packs_ds4_usb_report() {
        let mut state = State {
            buttons: (1 << 1) | (1 << 3) | (1 << 4) | (1 << 12),
            touches: [
                Touch {
                    active: true,
                    id: 3,
                    x: 1919,
                    y: 941,
                },
                Touch::default(),
            ],
            ..State::default()
        };
        state.trigger_buttons[0] = true;
        state.triggers[1] = 127;
        state.dpad[0] = true;
        state.dpad[3] = true;
        let report = state.report(3, 7);

        assert_eq!(
            &report[..13],
            &[1, 128, 128, 128, 128, 0xa1, 0x0d, 0x0d, 255, 127, 0, 0, 25]
        );
        assert_eq!(&report[30..35], &[0x15, 0, 0, 1, 7]);
        assert_eq!(&report[35..39], &[3, 0x7f, 0xd7, 0x3a]);
        assert_eq!(report[39], 0x80);
    }

    #[test]
    fn advertises_wireless_adapter_reports() {
        assert!(
            REPORT_DESCRIPTOR
                .windows(8)
                .any(|entry| entry == [0x85, 0xe1, 0x09, 0x57, 0x95, 0x30, 0xb1, 0x02])
        );
        assert!(
            REPORT_DESCRIPTOR
                .windows(8)
                .any(|entry| entry == [0x85, 0xe2, 0x09, 0x57, 0x95, 0x10, 0xb1, 0x02])
        );
        assert!(
            REPORT_DESCRIPTOR
                .windows(8)
                .any(|entry| entry == [0x85, 0xe3, 0x09, 0x57, 0x95, 0x04, 0xb1, 0x02])
        );
    }

    #[test]
    fn identifies_attached_ds4() {
        assert_eq!(ADAPTER_CONTROLLER_REPORT, &[0xe3, 0x4c, 0x05, 0xcc, 0x09]);
        assert_eq!(&FIRMWARE_REPORT[35..37], &[0x00, 0x31]);
        assert_eq!(State::default().report(0, 0)[31] & 0x04, 0);
    }
}
