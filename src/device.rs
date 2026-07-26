use crate::protocol::{
    PROTEUS_PRODUCT_ID, Report, Trackpad, VALVE_VENDOR_ID, WirelessState, imu_mode_report,
    lizard_mode_report, parse_report, rumble_report, trackpad_haptic_report,
};
use hidapi::{HidApi, HidDevice};
use std::ffi::CString;
use std::time::{Duration, Instant};

pub struct DeviceManager {
    api: HidApi,
    slots: Vec<Slot>,
    active: Option<usize>,
    last_scan: Instant,
    disconnected_reported: bool,
}

pub enum DeviceEvent {
    Report(Report),
    Disconnected,
}

struct Slot {
    interface: i32,
    path: CString,
    device: HidDevice,
    last_state: Option<Instant>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("could not initialize HID access: {0}")]
    Initialize(#[source] hidapi::HidError),
    #[error("could not refresh HID devices: {0}")]
    Refresh(#[source] hidapi::HidError),
    #[error("could not configure controller: {0}")]
    Configure(#[source] hidapi::HidError),
    #[error("could not write controller output: {0}")]
    Write(#[source] hidapi::HidError),
}

impl DeviceManager {
    pub fn new() -> Result<Self, DeviceError> {
        let api = HidApi::new().map_err(DeviceError::Initialize)?;
        let mut manager = Self {
            api,
            slots: Vec::new(),
            active: None,
            last_scan: Instant::now() - Duration::from_secs(2),
            disconnected_reported: true,
        };
        manager.scan()?;
        Ok(manager)
    }

    pub fn poll(&mut self) -> Result<Option<DeviceEvent>, DeviceError> {
        if self.active.is_none() && self.last_scan.elapsed() >= Duration::from_secs(1) {
            self.scan()?;
        }

        if let Some(active) = self.active
            && self.slots[active]
                .last_state
                .is_some_and(|last| last.elapsed() >= Duration::from_millis(750))
            && mark_disconnected(&mut self.active, &mut self.disconnected_reported)
        {
            return Ok(Some(DeviceEvent::Disconnected));
        }

        let mut report = [0; 64];
        for index in 0..self.slots.len() {
            match self.slots[index].device.read(&mut report) {
                Ok(0) => continue,
                Ok(length) => {
                    let Ok(parsed) = parse_report(&report[..length]) else {
                        continue;
                    };
                    if self.active == Some(index)
                        && parsed == Report::Wireless(WirelessState::Disconnected)
                    {
                        self.slots[index].last_state = None;
                        if mark_disconnected(&mut self.active, &mut self.disconnected_reported) {
                            return Ok(Some(DeviceEvent::Disconnected));
                        }
                        return Ok(None);
                    }
                    if matches!(parsed, Report::State(_)) {
                        if self.active.is_none() {
                            self.active = Some(index);
                            self.disconnected_reported = false;
                            self.slots[index]
                                .device
                                .send_feature_report(&lizard_mode_report(false))
                                .map_err(DeviceError::Configure)?;
                            self.slots[index]
                                .device
                                .send_feature_report(&imu_mode_report(true))
                                .map_err(DeviceError::Configure)?;
                            log::info!(
                                "controller connected on puck interface {}",
                                self.slots[index].interface
                            );
                        }
                        if self.active == Some(index) {
                            self.slots[index].last_state = Some(Instant::now());
                            return Ok(Some(DeviceEvent::Report(parsed)));
                        }
                        log::warn!(
                            "ignoring additional controller on puck interface {}",
                            self.slots[index].interface
                        );
                    } else if self.active == Some(index) {
                        return Ok(Some(DeviceEvent::Report(parsed)));
                    }
                }
                Err(error) => {
                    log::warn!("controller read failed: {error}");
                    let disconnected =
                        mark_disconnected(&mut self.active, &mut self.disconnected_reported);
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

    pub fn suppress_lizard_mode(&self) -> Result<(), DeviceError> {
        if let Some(active) = self.active {
            self.slots[active]
                .device
                .send_feature_report(&lizard_mode_report(false))
                .map_err(DeviceError::Configure)?;
        }
        Ok(())
    }

    pub fn rumble(&self, low_frequency: u16, high_frequency: u16) -> Result<(), DeviceError> {
        if let Some(active) = self.active {
            self.slots[active]
                .device
                .write(&rumble_report(low_frequency, high_frequency))
                .map_err(DeviceError::Write)?;
        }
        Ok(())
    }

    pub fn trackpad_haptic(&self, trackpad: Trackpad) -> Result<(), DeviceError> {
        if let Some(active) = self.active {
            self.slots[active]
                .device
                .write(&trackpad_haptic_report(trackpad))
                .map_err(DeviceError::Write)?;
        }
        Ok(())
    }

    fn scan(&mut self) -> Result<(), DeviceError> {
        self.last_scan = Instant::now();
        self.api.refresh_devices().map_err(DeviceError::Refresh)?;
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
                        last_state: None,
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

fn mark_disconnected(active: &mut Option<usize>, disconnected_reported: &mut bool) -> bool {
    let was_connected = active.take().is_some();
    if was_connected && !*disconnected_reported {
        *disconnected_reported = true;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_clears_active_slot_and_is_reported_once() {
        let mut active = Some(2);
        let mut reported = false;

        assert!(mark_disconnected(&mut active, &mut reported));
        assert_eq!(active, None);
        assert!(reported);
        assert!(!mark_disconnected(&mut active, &mut reported));
    }
}
