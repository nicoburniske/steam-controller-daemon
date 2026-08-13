use scd::protocol::{
    ControllerState, Haptic, PROTEUS_PRODUCT_ID, Report, Trackpad, VALVE_VENDOR_ID,
    imu_mode_report, lizard_mode_report, parse_report, trackpad_click_pressure_report,
};
use std::time::{Duration, Instant};

use crate::usb;

const FIRST_INTERFACE: u8 = 2;
const SLOT_COUNT: u8 = 4;
const CONTROL_TIMEOUT_MS: u32 = 10;

pub struct DeviceManager {
    puck: Option<usb::Device>,
    active: Option<usize>,
    trackpad_click_pressure: u16,
    last_state: Option<Instant>,
    last_scan: Instant,
    disconnected: bool,
}

pub enum DeviceEvent {
    State(ControllerState),
    Battery { percent: u8, charging: bool },
    Disconnected,
}

impl DeviceManager {
    pub fn new() -> Self {
        let mut manager = Self {
            puck: None,
            active: None,
            trackpad_click_pressure: 25,
            last_state: None,
            last_scan: Instant::now() - Duration::from_secs(2),
            disconnected: false,
        };
        manager.scan();
        manager
    }

    pub fn poll(&mut self) -> Option<DeviceEvent> {
        if self.disconnected {
            self.disconnected = false;
            return Some(DeviceEvent::Disconnected);
        }
        if self.puck.is_none() && self.last_scan.elapsed() >= Duration::from_secs(1) {
            self.scan();
        }

        if self.active.is_some()
            && self
                .last_state
                .is_some_and(|last| last.elapsed() >= Duration::from_millis(750))
        {
            self.active = None;
            self.last_state = None;
            return Some(DeviceEvent::Disconnected);
        }

        let puck = self.puck.as_mut()?;
        let mut report = [0; 64];
        let polled = match puck.poll(self.active, &mut report) {
            Ok(report) => report,
            Err(error) => {
                log::warn!("controller read failed: {error}");
                let disconnected = self.reset();
                return disconnected.then_some(DeviceEvent::Disconnected);
            }
        };
        let (index, length) = polled?;
        let parsed = parse_report(&report[..length])?;
        let interface = puck.interface(index);
        match parsed {
            Report::Disconnected if self.active == Some(index) => {
                self.active = None;
                self.last_state = None;
                return Some(DeviceEvent::Disconnected);
            }
            Report::Disconnected => {}
            Report::State(state) => {
                if self.active.is_none() {
                    let mut configured = self.send_feature(index, &lizard_mode_report(false))
                        && self.send_feature(index, &imu_mode_report(true));
                    for pad in [Trackpad::Left, Trackpad::Right] {
                        configured &= self.send_feature(
                            index,
                            &trackpad_click_pressure_report(pad, self.trackpad_click_pressure),
                        );
                    }
                    if !configured {
                        self.reset();
                        return None;
                    }
                    self.active = Some(index);
                    log::info!("controller connected on puck interface {interface}");
                }
                if self.active == Some(index) {
                    self.last_state = Some(Instant::now());
                    return Some(DeviceEvent::State(state));
                }
                log::warn!("ignoring additional controller on puck interface {interface}");
            }
            Report::Battery { percent, charging } if self.active == Some(index) => {
                return Some(DeviceEvent::Battery { percent, charging });
            }
            Report::Battery { .. } => {}
        }
        None
    }

    pub fn connected(&self) -> bool {
        self.active.is_some()
    }

    pub fn device_name(&self) -> Option<String> {
        self.active
            .zip(self.puck.as_ref())
            .map(|(active, puck)| format!("28de:1304 interface {}", puck.interface(active)))
    }

    pub fn suppress_lizard_mode(&self) {
        if let Some(active) = self.active {
            self.send_feature(active, &lizard_mode_report(false));
        }
    }

    pub fn set_trackpad_click_pressure(&mut self, pressure: u16) {
        self.trackpad_click_pressure = pressure;
        if let Some(active) = self.active {
            let mut configured = true;
            for pad in [Trackpad::Left, Trackpad::Right] {
                configured &=
                    self.send_feature(active, &trackpad_click_pressure_report(pad, pressure));
            }
            if !configured {
                self.disconnected = self.reset();
            }
        }
    }

    pub fn rumble(&mut self, low_frequency: u16, high_frequency: u16) {
        self.play_haptic(Haptic::Rumble {
            low_frequency,
            high_frequency,
        });
    }

    pub fn play_haptic(&mut self, haptic: Haptic) {
        let (Some(active), Some(puck)) = (self.active, self.puck.as_mut()) else {
            return;
        };
        let mut report = [0; 10];
        let length = haptic.encode(&mut report);
        if let Err(error) = puck.write(active, &report[..length]) {
            log::warn!("controller haptic write failed: {error}");
            self.disconnected = self.reset();
        }
    }

    fn scan(&mut self) {
        self.last_scan = Instant::now();
        match usb::Device::open(
            VALVE_VENDOR_ID,
            PROTEUS_PRODUCT_ID,
            FIRST_INTERFACE..FIRST_INTERFACE + SLOT_COUNT,
        ) {
            Ok(puck) => {
                self.puck = Some(puck);
                log::info!("claimed {SLOT_COUNT} Steam Controller puck slots");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("could not open Steam Controller puck: {error}"),
        }
    }

    fn reset(&mut self) -> bool {
        let disconnected = self.active.take().is_some();
        self.last_state = None;
        self.puck = None;
        self.last_scan = Instant::now() - Duration::from_secs(2);
        disconnected
    }

    fn send_feature(&self, active: usize, report: &[u8; 64]) -> bool {
        let Some(puck) = &self.puck else {
            return false;
        };
        match puck.control(active, report, CONTROL_TIMEOUT_MS) {
            Ok(()) => return true,
            Err(error) => log::warn!("controller feature write failed: {error}"),
        }
        false
    }
}
