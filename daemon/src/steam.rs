use scd::{Error, Result};
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::ptr;

pub struct SteamDevice {
    device: File,
    bus_id: String,
}

#[repr(C)]
struct UsbfsIoctl {
    interface: i32,
    ioctl: i32,
    data: *mut libc::c_void,
}

impl SteamDevice {
    pub fn attach(bus_id: String) -> Result<Self> {
        let device = Self::open(&bus_id)?;
        Self::set_mode(&device, 0o660)?;
        if let Err(error) = fs::write(STEAM_DEVICE, &bus_id) {
            let _ = Self::set_mode(&device, 0o600);
            return Err(error.into());
        }
        if let Err(error) = Self::rebind(&device) {
            let _ = fs::remove_file(STEAM_DEVICE);
            let _ = Self::set_mode(&device, 0o600);
            return Err(error);
        }
        log::info!("handed physical controller to Steam");
        Ok(Self { device, bus_id })
    }

    pub fn recover() -> Result<Option<Self>> {
        let bus_id = match fs::read_to_string(STEAM_DEVICE) {
            Ok(bus_id) => bus_id,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let bus_id = bus_id.trim().to_owned();
        let device = Self::open(&bus_id)?;
        log::info!("recovered physical Steam handoff");
        Ok(Some(Self { device, bus_id }))
    }

    pub fn detach(&self) -> Result<()> {
        Self::set_mode(&self.device, 0o600)?;
        if let Err(error) = fs::remove_file(STEAM_DEVICE) {
            let _ = Self::set_mode(&self.device, 0o660);
            return Err(error.into());
        }
        if let Err(error) = Self::rebind(&self.device) {
            let _ = fs::write(STEAM_DEVICE, &self.bus_id);
            let _ = Self::set_mode(&self.device, 0o660);
            return Err(error);
        }
        log::info!("reclaimed physical controller from Steam");
        Ok(())
    }

    fn open(bus_id: &str) -> Result<File> {
        let path = Path::new("/sys/bus/usb/devices").join(bus_id);
        if !fs::read_to_string(path.join("idVendor")).is_ok_and(|value| value == "28de\n")
            || !fs::read_to_string(path.join("idProduct")).is_ok_and(|value| value == "1304\n")
        {
            return Err(Error::message("Steam handoff device is no longer present"));
        }
        let bus = fs::read_to_string(path.join("busnum"))?
            .trim()
            .parse::<u16>()?;
        let device = fs::read_to_string(path.join("devnum"))?
            .trim()
            .parse::<u16>()?;
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/dev/bus/usb/{bus:03}/{device:03}"))?)
    }

    fn rebind(device: &File) -> Result<()> {
        let mut result = Ok(());
        for ioctl in [USBDEVFS_DISCONNECT, USBDEVFS_CONNECT] {
            for interface in 2..=6 {
                let mut command = UsbfsIoctl {
                    interface,
                    ioctl,
                    data: ptr::null_mut(),
                };
                if unsafe { libc::ioctl(device.as_raw_fd(), USBDEVFS_IOCTL, &mut command) } < 0
                    && result.is_ok()
                {
                    result = Err(Error::message(std::io::Error::last_os_error()));
                }
            }
        }
        result
    }

    fn set_mode(device: &File, mode: u32) -> Result<()> {
        Ok(device.set_permissions(fs::Permissions::from_mode(mode))?)
    }
}

const STEAM_DEVICE: &str = "/run/scd/steam-device";
const USBDEVFS_IOCTL: libc::c_ulong = 0xc0105512;
const USBDEVFS_DISCONNECT: i32 = 0x5516;
const USBDEVFS_CONNECT: i32 = 0x5517;
