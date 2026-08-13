//! fixed local USB/IP transport for the wired DualShock 4
//!
//! a local socket pair connects the fixed device backend to `vhci_hcd`

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const VHCI_PATH: &str = "/sys/devices/platform/vhci_hcd.0";
const DEVID: u32 = 0x0001_0001;
const USB_SPEED_FULL: u32 = 2;

const USBIP_CMD_SUBMIT: u32 = 1;
const USBIP_CMD_UNLINK: u32 = 2;
const USBIP_RET_SUBMIT: u32 = 3;
const USBIP_RET_UNLINK: u32 = 4;

#[derive(Clone, Copy)]
struct Header {
    seqnum: u32,
    devid: u32,
    direction: u32,
    endpoint: u32,
}

const DEVICE_DESCRIPTOR: &[u8] = &[
    18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x4c, 0x05, 0xcc, 0x09, 0x00, 0x01, 1, 2, 0, 1,
];
const CONFIGURATION_DESCRIPTOR: &[u8] = &[
    9, 2, 41, 0, 1, 1, 0, 0xc0, 250, 9, 4, 0, 0, 2, 3, 0, 0, 0, 9, 0x21, 0x11, 0x01, 0, 1, 0x22,
    0xfb, 0x01, 7, 5, 0x84, 3, 64, 0, 5, 7, 5, 0x03, 3, 64, 0, 5,
];

pub struct Transport {
    port: u16,
    socket: UnixStream,
    worker: Option<JoinHandle<io::Result<()>>>,
    report: Arc<Mutex<[u8; 64]>>,
    rumble: Arc<Mutex<(u16, u16)>>,
}

impl Transport {
    pub fn new(report: [u8; 64]) -> io::Result<Self> {
        let (shutdown_socket, attached_socket) = UnixStream::pair()?;
        let mut worker_socket = shutdown_socket.try_clone()?;
        let port = attach(attached_socket.as_raw_fd())?;
        drop(attached_socket);

        let report = Arc::new(Mutex::new(report));
        let rumble = Arc::new(Mutex::new((0, 0)));
        let worker_report = report.clone();
        let worker_rumble = rumble.clone();
        let worker = match thread::Builder::new()
            .name("scd-ds4-usbip".into())
            .spawn(move || {
                let result = serve(&mut worker_socket, &worker_report, &worker_rumble);
                let _ = worker_socket.shutdown(std::net::Shutdown::Both);
                result
            }) {
            Ok(worker) => worker,
            Err(error) => {
                let _ = detach(port);
                return Err(error);
            }
        };

        Ok(Self {
            port,
            socket: shutdown_socket,
            worker: Some(worker),
            report,
            rumble,
        })
    }

    pub fn set_report(&self, report: [u8; 64]) {
        *self.report.lock().unwrap() = report;
    }

    pub fn rumble(&self) -> io::Result<(u16, u16)> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "DualShock USB/IP worker stopped",
            ));
        }
        Ok(*self.rumble.lock().unwrap())
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if let Err(error) = detach(self.port) {
            log::warn!("failed to detach DualShock USB/IP device: {error}");
        }
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) if error.kind() == io::ErrorKind::BrokenPipe => {}
                Ok(Err(error)) => log::warn!("DualShock USB/IP worker failed: {error}"),
                Err(_) => log::warn!("DualShock USB/IP worker panicked"),
            }
        }
    }
}

fn attach(socket: i32) -> io::Result<u16> {
    for _ in 0..8 {
        let port = free_port()?;
        match fs::write(
            format!("{VHCI_PATH}/attach"),
            format!("{port} {socket} {DEVID} {USB_SPEED_FULL}"),
        ) {
            Ok(()) => return Ok(port),
            Err(error) if error.kind() == io::ErrorKind::ResourceBusy => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::ResourceBusy,
        "no free vhci_hcd port",
    ))
}

fn free_port() -> io::Result<u16> {
    for index in 0.. {
        let name = if index == 0 {
            "status".to_owned()
        } else {
            format!("status.{index}")
        };
        let status = match fs::read_to_string(format!("{VHCI_PATH}/{name}")) {
            Ok(status) => status,
            Err(error) if index != 0 && error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        };
        for line in status.lines().skip(1) {
            let mut fields = line.split_whitespace();
            if fields.next() == Some("hs")
                && let (Some(port), Some(status)) = (fields.next(), fields.next())
                && status.parse::<u8>() == Ok(4)
            {
                return port.parse().map_err(io::Error::other);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no free high-speed vhci_hcd port",
    ))
}

fn detach(port: u16) -> io::Result<()> {
    fs::write(format!("{VHCI_PATH}/detach"), port.to_string())
}

fn serve(
    socket: &mut UnixStream,
    report: &Mutex<[u8; 64]>,
    rumble: &Mutex<(u16, u16)>,
) -> io::Result<()> {
    let mut counter = 0u8;
    let mut next_input = Instant::now();
    loop {
        let mut request = [0; 48];
        match socket.read_exact(&mut request) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        }
        let command = u32::from_be_bytes(request[..4].try_into().unwrap());
        let seqnum = u32::from_be_bytes(request[4..8].try_into().unwrap());
        let devid = u32::from_be_bytes(request[8..12].try_into().unwrap());
        let direction = u32::from_be_bytes(request[12..16].try_into().unwrap());
        let endpoint = u32::from_be_bytes(request[16..20].try_into().unwrap());
        let header = Header {
            seqnum,
            devid,
            direction,
            endpoint,
        };
        if command == USBIP_CMD_UNLINK {
            write_reply(socket, USBIP_RET_UNLINK, header, 0, &[])?;
            continue;
        }
        if command != USBIP_CMD_SUBMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid USB/IP request",
            ));
        }

        let length = u32::from_be_bytes(request[24..28].try_into().unwrap()) as usize;
        let packet_count = u32::from_be_bytes(request[32..36].try_into().unwrap());
        if length > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized USB/IP transfer",
            ));
        }
        let mut output = vec![];
        if direction == 0 {
            output.resize(length, 0);
            socket.read_exact(&mut output)?;
        }

        if devid != DEVID || !matches!(packet_count, 0 | u32::MAX) {
            write_error(socket, header, libc::EINVAL)?;
        } else if endpoint == 0 {
            match control(&request[40..48], &output, rumble) {
                Ok(data) => write_reply(
                    socket,
                    USBIP_RET_SUBMIT,
                    header,
                    if direction == 0 { length } else { data.len() },
                    &data,
                )?,
                Err(errno) => write_error(socket, header, errno)?,
            }
        } else if endpoint == 4 && direction == 1 {
            next_input += Duration::from_millis(5);
            if let Some(delay) = next_input.checked_duration_since(Instant::now()) {
                thread::sleep(delay);
            } else {
                next_input = Instant::now();
            }
            let mut input = *report.lock().unwrap();
            input[7] = (input[7] & 0x03) | ((counter & 0x3f) << 2);
            counter = counter.wrapping_add(1) & 0x3f;
            let input = &input[..input.len().min(length)];
            write_reply(socket, USBIP_RET_SUBMIT, header, input.len(), input)?;
        } else if endpoint == 3 && direction == 0 {
            if let Some(value) = parse_output(&output) {
                *rumble.lock().unwrap() = value;
            }
            write_reply(socket, USBIP_RET_SUBMIT, header, length, &[])?;
        } else {
            write_error(socket, header, libc::EPIPE)?;
        }
    }
}

fn control(setup: &[u8], output: &[u8], rumble: &Mutex<(u16, u16)>) -> Result<Vec<u8>, i32> {
    let request_type = setup[0];
    let request = setup[1];
    let value = u16::from_le_bytes(setup[2..4].try_into().unwrap());
    let length = usize::from(u16::from_le_bytes(setup[6..8].try_into().unwrap()));
    let mut data = match (request_type, request) {
        (0x80, 6) if value >> 8 == 1 => DEVICE_DESCRIPTOR.to_vec(),
        (0x80, 6) if value >> 8 == 2 => CONFIGURATION_DESCRIPTOR.to_vec(),
        (0x80, 6) if value >> 8 == 3 => string_descriptor(value as u8).ok_or(libc::EPIPE)?,
        (0x81, 6) if value >> 8 == 0x21 => CONFIGURATION_DESCRIPTOR[18..27].to_vec(),
        (0x81, 6) if value >> 8 == 0x22 => super::REPORT_DESCRIPTOR.to_vec(),
        (0x80, 0) | (0x81, 0) | (0x82, 0) => vec![0, 0],
        (0x80, 8) => vec![1],
        (0x81, 10) => vec![0],
        (0x00, 5 | 9) | (0x01, 11) | (0x02, 1) => vec![],
        (0xa1, 1) => match value as u8 {
            0x02 => super::CALIBRATION.as_slice(),
            0x12 => super::PAIRING.as_slice(),
            0xa3 => super::FIRMWARE.as_slice(),
            _ => return Err(libc::EPIPE),
        }
        .to_vec(),
        (0xa1, 2 | 3) => vec![0],
        (0x21, 9) => {
            if value as u8 == 0x05 {
                let value = parse_output(output).ok_or(libc::EINVAL)?;
                *rumble.lock().unwrap() = value;
            }
            vec![]
        }
        (0x21, 10 | 11) => vec![],
        _ => return Err(libc::EPIPE),
    };
    data.truncate(length);
    Ok(data)
}

fn string_descriptor(index: u8) -> Option<Vec<u8>> {
    if index == 0 {
        return Some(vec![4, 3, 0x09, 0x04]);
    }
    let value = match index {
        1 => "Sony Interactive Entertainment",
        2 => "Wireless Controller",
        _ => return None,
    };
    let mut descriptor = Vec::with_capacity(2 + value.len() * 2);
    descriptor.extend_from_slice(&[(2 + value.len() * 2) as u8, 3]);
    for unit in value.encode_utf16() {
        descriptor.extend_from_slice(&unit.to_le_bytes());
    }
    Some(descriptor)
}

fn write_reply(
    socket: &mut UnixStream,
    command: u32,
    header: Header,
    actual_length: usize,
    data: &[u8],
) -> io::Result<()> {
    let mut reply = [0; 48];
    for (offset, value) in [
        command,
        header.seqnum,
        header.devid,
        header.direction,
        header.endpoint,
    ]
    .into_iter()
    .enumerate()
    {
        reply[offset * 4..offset * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    reply[24..28].copy_from_slice(&(actual_length as u32).to_be_bytes());
    socket.write_all(&reply)?;
    socket.write_all(data)
}

fn write_error(socket: &mut UnixStream, header: Header, errno: i32) -> io::Result<()> {
    let mut reply = [0; 48];
    for (offset, value) in [
        USBIP_RET_SUBMIT,
        header.seqnum,
        header.devid,
        header.direction,
        header.endpoint,
    ]
    .into_iter()
    .enumerate()
    {
        reply[offset * 4..offset * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    reply[20..24].copy_from_slice(&(-errno).to_be_bytes());
    socket.write_all(&reply)
}

fn parse_output(report: &[u8]) -> Option<(u16, u16)> {
    (report.len() >= 6 && report[0] == 0x05 && report[1] & 1 != 0)
        .then(|| (u16::from(report[5]) * 257, u16::from(report[4]) * 257))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_identify_wired_ds4_v2() {
        assert_eq!(&DEVICE_DESCRIPTOR[8..12], &[0x4c, 0x05, 0xcc, 0x09]);
        assert_eq!(&DEVICE_DESCRIPTOR[12..14], &[0x00, 0x01]);
        assert_eq!(CONFIGURATION_DESCRIPTOR.len(), 41);
        assert_eq!(
            &CONFIGURATION_DESCRIPTOR[27..],
            &[7, 5, 0x84, 3, 64, 0, 5, 7, 5, 3, 3, 64, 0, 5]
        );
    }

    #[test]
    fn parses_wired_rumble() {
        let mut report = [0; 32];
        report[0] = 5;
        report[1] = 1;
        report[4] = 0x12;
        report[5] = 0x34;
        assert_eq!(parse_output(&report), Some((0x3434, 0x1212)));
        report[1] = 0;
        assert_eq!(parse_output(&report), None);
        assert_eq!(parse_output(&report[..5]), None);
    }

    #[test]
    fn answers_enumeration_and_hid_control_requests() {
        let rumble = Mutex::new((0, 0));
        assert_eq!(
            control(&[0x80, 6, 0, 1, 0, 0, 18, 0], &[], &rumble).unwrap(),
            DEVICE_DESCRIPTOR
        );
        assert_eq!(
            control(&[0x80, 6, 0, 2, 0, 0, 9, 0], &[], &rumble).unwrap(),
            CONFIGURATION_DESCRIPTOR[..9]
        );
        assert_eq!(
            control(&[0xa1, 1, 2, 3, 0, 0, 37, 0], &[], &rumble).unwrap(),
            super::super::CALIBRATION
        );
        assert_eq!(
            control(&[0xa1, 1, 0x12, 3, 0, 0, 16, 0], &[], &rumble).unwrap(),
            super::super::PAIRING
        );
    }

    #[test]
    fn set_report_updates_rumble() {
        let rumble = Mutex::new((0, 0));
        let mut report = [0; 32];
        report[0] = 5;
        report[1] = 1;
        report[4] = 0x12;
        report[5] = 0x34;
        assert_eq!(
            control(&[0x21, 9, 5, 2, 0, 0, 32, 0], &report, &rumble),
            Ok(vec![])
        );
        assert_eq!(*rumble.lock().unwrap(), (0x3434, 0x1212));
    }
}
