//! minimal Linux usbfs interrupt transport

use linux_raw_sys::ioctl::{
    USBDEVFS_CLEAR_HALT, USBDEVFS_CONNECT, USBDEVFS_CONTROL, USBDEVFS_DISCARDURB,
    USBDEVFS_DISCONNECT_CLAIM, USBDEVFS_IOCTL, USBDEVFS_REAPURB, USBDEVFS_REAPURBNDELAY,
    USBDEVFS_RELEASEINTERFACE, USBDEVFS_SUBMITURB,
};
use std::ffi::{c_int, c_uint, c_void};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::ptr;

const INTERRUPT: u8 = 1;

pub struct Device {
    file: File,
    slots: Vec<Slot>,
    claimed: Vec<u8>,
}

struct Slot {
    interface: u8,
    input: [Box<Transfer<64>>; 2],
    output: Box<Transfer<10>>,
}

#[repr(C)]
struct Transfer<const N: usize> {
    urb: Urb,
    buffer: [u8; N],
    pending: bool,
}

#[repr(C)]
struct Urb {
    ep_type: u8,
    endpoint: u8,
    status: c_int,
    flags: c_uint,
    buffer: *mut u8,
    buffer_length: c_int,
    actual_length: c_int,
    start_frame: c_int,
    packets_or_stream_id: c_uint,
    error_count: c_int,
    signr: c_uint,
    usercontext: *mut c_void,
}

#[repr(C)]
struct DetachAndClaim {
    interface: c_uint,
    flags: c_uint,
    driver: [u8; 256],
}

#[repr(C)]
struct UsbfsIoctl {
    interface: c_uint,
    ioctl_code: c_uint,
    data: *mut c_void,
}

#[repr(C)]
struct Control {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
    timeout: u32,
    data: *mut c_void,
}

impl Device {
    pub fn open(
        vendor: u16,
        product: u16,
        interfaces: impl Iterator<Item = u8>,
    ) -> io::Result<Self> {
        let mut device_path = None;
        for entry in fs::read_dir("/sys/bus/usb/devices")? {
            let path = entry?.path();
            if read_hex(&path.join("idVendor")) == Some(vendor)
                && read_hex(&path.join("idProduct")) == Some(product)
            {
                let bus = fs::read_to_string(path.join("busnum"))?;
                let device = fs::read_to_string(path.join("devnum"))?;
                device_path = Some(format!(
                    "/dev/bus/usb/{:03}/{:03}",
                    bus.trim().parse::<u8>().map_err(io::Error::other)?,
                    device.trim().parse::<u8>().map_err(io::Error::other)?
                ));
                break;
            }
        }
        let path = device_path
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "device not found"))?;
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut device = Self {
            file,
            slots: Vec::new(),
            claimed: Vec::new(),
        };
        for interface in interfaces {
            device.claim(interface)?;
            let mut slot = Slot {
                interface,
                input: std::array::from_fn(|_| Transfer::new(0x80 | (interface + 1))),
                output: Transfer::new(interface),
            };
            for transfer in &mut slot.input {
                transfer.submit(device.file.as_raw_fd(), 64)?;
            }
            device.slots.push(slot);
        }
        Ok(device)
    }

    pub fn poll(
        &mut self,
        active: Option<usize>,
        report: &mut [u8; 64],
    ) -> io::Result<Option<(usize, usize)>> {
        self.reap()?;
        if !self.slots.iter().enumerate().any(|(index, slot)| {
            active.is_none_or(|active| active == index)
                && slot.input.iter().any(|transfer| !transfer.pending)
        }) {
            let mut poll = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            let result = unsafe { libc::poll(&raw mut poll, 1, 1) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            } else if result != 0 {
                self.reap()?;
            }
        }
        for index in 0..self.slots.len() {
            if active.is_some_and(|active| active != index) {
                continue;
            }
            for transfer in &mut self.slots[index].input {
                if transfer.pending {
                    continue;
                }
                let length = transfer.result()?;
                report[..length].copy_from_slice(&transfer.buffer[..length]);
                transfer.submit(self.file.as_raw_fd(), 64)?;
                return Ok(Some((index, length)));
            }
        }
        Ok(None)
    }

    pub fn write(&mut self, index: usize, data: &[u8]) -> io::Result<()> {
        self.reap()?;
        let transfer = &mut self.slots[index].output;
        if transfer.pending {
            return Ok(());
        }
        if transfer.urb.buffer_length != 0 {
            match transfer.result() {
                Ok(length) if length == transfer.urb.buffer_length as usize => {}
                Ok(length) => log::warn!(
                    "short controller haptic write: {length}/{}",
                    transfer.urb.buffer_length
                ),
                Err(error) if error.raw_os_error() == Some(libc::EPIPE) => {
                    clear_halt(self.file.as_raw_fd(), transfer.urb.endpoint)?;
                }
                Err(error) => return Err(error),
            }
        }
        transfer.buffer[..data.len()].copy_from_slice(data);
        transfer.submit(self.file.as_raw_fd(), data.len())
    }

    pub fn control(&self, index: usize, data: &[u8], timeout_ms: u32) -> io::Result<()> {
        let interface = self.slots[index].interface;
        let length = u16::try_from(data.len()).map_err(io::Error::other)?;
        let mut transfer = Control {
            request_type: 0x21,
            request: 9,
            value: 0x0301,
            index: u16::from(interface),
            length,
            timeout: timeout_ms,
            data: data.as_ptr().cast_mut().cast(),
        };
        let written = ioctl(
            self.file.as_raw_fd(),
            USBDEVFS_CONTROL as _,
            (&raw mut transfer).cast(),
        )?;
        if written == data.len() as c_int {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("short controller feature write: {written}/{}", data.len()),
            ))
        }
    }

    pub fn interface(&self, index: usize) -> u8 {
        self.slots[index].interface
    }

    fn claim(&mut self, interface: u8) -> io::Result<()> {
        let mut claim = DetachAndClaim {
            interface: c_uint::from(interface),
            flags: 0x02,
            driver: [0; 256],
        };
        claim.driver[..6].copy_from_slice(b"usbfs\0");
        ioctl(
            self.file.as_raw_fd(),
            USBDEVFS_DISCONNECT_CLAIM as _,
            (&raw mut claim).cast(),
        )?;
        self.claimed.push(interface);
        Ok(())
    }

    fn reap(&mut self) -> io::Result<()> {
        loop {
            let mut urb: *mut Urb = ptr::null_mut();
            match ioctl(
                self.file.as_raw_fd(),
                USBDEVFS_REAPURBNDELAY as _,
                (&raw mut urb).cast(),
            ) {
                Ok(_) => self.complete(urb)?,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }

    fn complete(&mut self, urb: *mut Urb) -> io::Result<()> {
        for slot in &mut self.slots {
            for transfer in &mut slot.input {
                if ptr::eq(&raw mut transfer.urb, urb) {
                    transfer.pending = false;
                    return Ok(());
                }
            }
            if ptr::eq(&raw mut slot.output.urb, urb) {
                slot.output.pending = false;
                return Ok(());
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown completed USB transfer",
        ))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let fd = self.file.as_raw_fd();
        let mut pending = 0;
        for slot in &mut self.slots {
            for transfer in &mut slot.input {
                if transfer.pending {
                    pending += 1;
                    let _ = ioctl(fd, USBDEVFS_DISCARDURB as _, (&raw mut transfer.urb).cast());
                }
            }
            if slot.output.pending {
                pending += 1;
                let _ = ioctl(
                    fd,
                    USBDEVFS_DISCARDURB as _,
                    (&raw mut slot.output.urb).cast(),
                );
            }
        }
        while pending != 0 {
            let mut urb: *mut Urb = ptr::null_mut();
            match ioctl(fd, USBDEVFS_REAPURB as _, (&raw mut urb).cast()) {
                Ok(_) => {
                    pending -= 1;
                    let _ = self.complete(urb);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        for interface in self.claimed.iter().rev().copied() {
            let mut interface = c_uint::from(interface);
            if ioctl(
                fd,
                USBDEVFS_RELEASEINTERFACE as _,
                (&raw mut interface).cast(),
            )
            .is_ok()
            {
                let mut command = UsbfsIoctl {
                    interface,
                    ioctl_code: USBDEVFS_CONNECT as _,
                    data: ptr::null_mut(),
                };
                let _ = ioctl(fd, USBDEVFS_IOCTL as _, (&raw mut command).cast());
            }
        }
    }
}

impl<const N: usize> Transfer<N> {
    fn new(endpoint: u8) -> Box<Self> {
        let mut transfer = Box::new(Self {
            urb: Urb {
                ep_type: INTERRUPT,
                endpoint,
                status: 0,
                flags: 0,
                buffer: ptr::null_mut(),
                buffer_length: 0,
                actual_length: 0,
                start_frame: 0,
                packets_or_stream_id: 0,
                error_count: 0,
                signr: 0,
                usercontext: ptr::null_mut(),
            },
            buffer: [0; N],
            pending: false,
        });
        transfer.urb.buffer = transfer.buffer.as_mut_ptr();
        transfer
    }

    fn submit(&mut self, fd: RawFd, length: usize) -> io::Result<()> {
        self.urb.buffer_length = c_int::try_from(length).map_err(io::Error::other)?;
        self.urb.actual_length = 0;
        self.urb.status = 0;
        ioctl(fd, USBDEVFS_SUBMITURB as _, (&raw mut self.urb).cast())?;
        self.pending = true;
        Ok(())
    }

    fn result(&self) -> io::Result<usize> {
        if self.urb.status == 0 {
            Ok(self.urb.actual_length as usize)
        } else {
            Err(io::Error::from_raw_os_error(self.urb.status.abs()))
        }
    }
}

fn ioctl(fd: RawFd, request: libc::c_ulong, value: *mut c_void) -> io::Result<c_int> {
    let result = unsafe { libc::ioctl(fd, request, value) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

fn clear_halt(fd: RawFd, endpoint: u8) -> io::Result<()> {
    let mut endpoint = c_uint::from(endpoint);
    ioctl(fd, USBDEVFS_CLEAR_HALT as _, (&raw mut endpoint).cast()).map(|_| ())
}

fn read_hex(path: &Path) -> Option<u16> {
    u16::from_str_radix(fs::read_to_string(path).ok()?.trim(), 16).ok()
}
