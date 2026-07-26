use crate::protocol::{
    ControllerState, PROTEUS_PRODUCT_ID, Report, Trackpad, VALVE_VENDOR_ID, imu_mode_report,
    lizard_mode_report, parse_report, rumble_report, trackpad_haptic_report,
};
use hidapi::{HidApi, HidDevice};
use std::ffi::CString;
use std::time::{Duration, Instant};

use crate::{Result, ResultExt};

pub struct DeviceManager {
    api: HidApi,
    slots: Vec<Slot>,
    active: Option<usize>,
    last_state: Option<Instant>,
    last_scan: Instant,
}

pub enum DeviceEvent {
    State(ControllerState),
    Battery { percent: u8, charging: bool },
    Disconnected,
}

struct Slot {
    interface: i32,
    path: CString,
    device: HidDevice,
}

impl DeviceManager {
    pub fn new() -> Result<Self> {
        let api = HidApi::new().whence()?;
        let mut manager = Self {
            api,
            slots: Vec::new(),
            active: None,
            last_state: None,
            last_scan: Instant::now() - Duration::from_secs(2),
        };
        manager.scan()?;
        Ok(manager)
    }

    pub fn poll(&mut self) -> Result<Option<DeviceEvent>> {
        if self.active.is_none() && self.last_scan.elapsed() >= Duration::from_secs(1) {
            self.scan()?;
        }

        if self.active.is_some()
            && self
                .last_state
                .is_some_and(|last| last.elapsed() >= Duration::from_millis(750))
        {
            self.active = None;
            self.last_state = None;
            return Ok(Some(DeviceEvent::Disconnected));
        }

        let mut report = [0; 64];
        for index in 0..self.slots.len() {
            match self.slots[index].device.read(&mut report) {
                Ok(0) => continue,
                Ok(length) => {
                    let Some(parsed) = parse_report(&report[..length]) else {
                        continue;
                    };
                    match parsed {
                        Report::Disconnected if self.active == Some(index) => {
                            self.active = None;
                            self.last_state = None;
                            return Ok(Some(DeviceEvent::Disconnected));
                        }
                        Report::Disconnected => {}
                        Report::State(state) => {
                            if self.active.is_none() {
                                self.active = Some(index);
                                self.slots[index]
                                    .device
                                    .send_feature_report(&lizard_mode_report(false))
                                    .whence()?;
                                self.slots[index]
                                    .device
                                    .send_feature_report(&imu_mode_report(true))
                                    .whence()?;
                                log::info!(
                                    "controller connected on puck interface {}",
                                    self.slots[index].interface
                                );
                            }
                            if self.active == Some(index) {
                                self.last_state = Some(Instant::now());
                                return Ok(Some(DeviceEvent::State(state)));
                            }
                            log::warn!(
                                "ignoring additional controller on puck interface {}",
                                self.slots[index].interface
                            );
                        }
                        Report::Battery { percent, charging } if self.active == Some(index) => {
                            return Ok(Some(DeviceEvent::Battery { percent, charging }));
                        }
                        Report::Battery { .. } => {}
                    }
                }
                Err(error) => {
                    log::warn!("controller read failed: {error}");
                    let disconnected = self.active.take().is_some();
                    self.last_state = None;
                    self.slots.clear();
                    self.last_scan = Instant::now() - Duration::from_secs(2);
                    if disconnected {
                        return Ok(Some(DeviceEvent::Disconnected));
                    }
                    return Ok(None);
                }
            }
        }

        Ok(None)
    }

    pub fn connected(&self) -> bool {
        self.active.is_some()
    }

    pub fn device_name(&self) -> Option<String> {
        self.active
            .map(|index| format!("28de:1304 interface {}", self.slots[index].interface))
    }

    pub fn suppress_lizard_mode(&self) -> Result<()> {
        if let Some(active) = self.active {
            self.slots[active]
                .device
                .send_feature_report(&lizard_mode_report(false))
                .whence()?;
        }
        Ok(())
    }

    pub fn rumble(&self, low_frequency: u16, high_frequency: u16) -> Result<()> {
        if let Some(active) = self.active {
            self.slots[active]
                .device
                .write(&rumble_report(low_frequency, high_frequency))
                .whence()?;
        }
        Ok(())
    }

    pub fn trackpad_haptic(&self, trackpad: Trackpad) -> Result<()> {
        if let Some(active) = self.active {
            self.slots[active]
                .device
                .write(&trackpad_haptic_report(trackpad))
                .whence()?;
        }
        Ok(())
    }

    fn scan(&mut self) -> Result<()> {
        self.last_scan = Instant::now();
        self.api.refresh_devices().whence()?;
        let previous_slot_count = self.slots.len();
        for info in self.api.device_list().filter(|info| {
            info.vendor_id() == VALVE_VENDOR_ID
                && info.product_id() == PROTEUS_PRODUCT_ID
                && (2..=5).contains(&info.interface_number())
        }) {
            if self
                .slots
                .iter()
                .any(|slot| slot.path.as_c_str() == info.path())
            {
                continue;
            }
            match self.api.open_path(info.path()) {
                Ok(device) => {
                    if let Err(error) = device.set_blocking_mode(false) {
                        log::warn!("could not make puck interface nonblocking: {error}");
                        continue;
                    }
                    self.slots.push(Slot {
                        interface: info.interface_number(),
                        path: info.path().to_owned(),
                        device,
                    });
                }
                Err(error) => log::warn!(
                    "could not open puck interface {}: {error}",
                    info.interface_number()
                ),
            }
        }
        self.slots.sort_by_key(|slot| slot.interface);
        if self.slots.len() != previous_slot_count {
            log::info!("opened {} Steam Controller puck slots", self.slots.len());
        }
        Ok(())
    }
}
