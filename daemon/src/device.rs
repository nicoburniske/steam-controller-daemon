use rusb::{Context, DeviceHandle, UsbContext};
use scd::Result;
use scd::protocol::{
    ControllerState, Haptic, PROTEUS_PRODUCT_ID, Report, Trackpad, VALVE_VENDOR_ID,
    imu_mode_report, lizard_mode_report, parse_report, trackpad_click_pressure_report,
};
use std::time::{Duration, Instant};

const FIRST_INTERFACE: u8 = 2;
const SLOT_COUNT: u8 = 4;
const POLL_TIMEOUT: Duration = Duration::from_millis(1);
const USB_TIMEOUT: Duration = Duration::from_millis(10);

pub struct DeviceManager {
    context: Context,
    puck: Option<DeviceHandle<Context>>,
    active: Option<u8>,
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
    pub fn new() -> Result<Self> {
        let mut manager = Self {
            context: Context::new()?,
            puck: None,
            active: None,
            trackpad_click_pressure: 25,
            last_state: None,
            last_scan: Instant::now() - Duration::from_secs(2),
            disconnected: false,
        };
        manager.scan();
        Ok(manager)
    }

    pub fn poll(&mut self) -> Result<Option<DeviceEvent>> {
        if self.disconnected {
            self.disconnected = false;
            return Ok(Some(DeviceEvent::Disconnected));
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
            return Ok(Some(DeviceEvent::Disconnected));
        }

        let Some(puck) = &self.puck else {
            return Ok(None);
        };
        let mut report = [0; 64];
        for slot in 0..SLOT_COUNT {
            let interface = FIRST_INTERFACE + slot;
            if self.active.is_some_and(|active| active != interface) {
                continue;
            }
            let length =
                match puck.read_interrupt(0x80 | (interface + 1), &mut report, POLL_TIMEOUT) {
                    Ok(length) => length,
                    Err(rusb::Error::Timeout) => continue,
                    Err(rusb::Error::Interrupted) => continue,
                    Err(rusb::Error::Pipe) if puck.clear_halt(0x80 | (interface + 1)).is_ok() => {
                        continue;
                    }
                    Err(error) => {
                        log::warn!("controller read failed: {error}");
                        let disconnected = self.reset();
                        return Ok(disconnected.then_some(DeviceEvent::Disconnected));
                    }
                };
            let Some(parsed) = parse_report(&report[..length]) else {
                continue;
            };
            match parsed {
                Report::Disconnected if self.active == Some(interface) => {
                    self.active = None;
                    self.last_state = None;
                    return Ok(Some(DeviceEvent::Disconnected));
                }
                Report::Disconnected => {}
                Report::State(state) => {
                    if self.active.is_none() {
                        let mut configured = self
                            .send_feature(interface, &lizard_mode_report(false))
                            && self.send_feature(interface, &imu_mode_report(true));
                        for pad in [Trackpad::Left, Trackpad::Right] {
                            configured &= self.send_feature(
                                interface,
                                &trackpad_click_pressure_report(pad, self.trackpad_click_pressure),
                            );
                        }
                        if !configured {
                            self.reset();
                            return Ok(None);
                        }
                        self.active = Some(interface);
                        log::info!("controller connected on puck interface {interface}");
                    }
                    if self.active == Some(interface) {
                        self.last_state = Some(Instant::now());
                        return Ok(Some(DeviceEvent::State(state)));
                    }
                    log::warn!("ignoring additional controller on puck interface {interface}");
                }
                Report::Battery { percent, charging } if self.active == Some(interface) => {
                    return Ok(Some(DeviceEvent::Battery { percent, charging }));
                }
                Report::Battery { .. } => {}
            }
        }
        Ok(None)
    }

    pub fn connected(&self) -> bool {
        self.active.is_some()
    }

    pub fn device_name(&self) -> Option<String> {
        self.active
            .map(|interface| format!("28de:1304 interface {interface}"))
    }

    pub fn suppress_lizard_mode(&self) {
        if let Some(interface) = self.active {
            self.send_feature(interface, &lizard_mode_report(false));
        }
    }

    pub fn set_trackpad_click_pressure(&mut self, pressure: u16) {
        self.trackpad_click_pressure = pressure;
        if let Some(interface) = self.active {
            let mut configured = true;
            for pad in [Trackpad::Left, Trackpad::Right] {
                configured &=
                    self.send_feature(interface, &trackpad_click_pressure_report(pad, pressure));
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
        let (Some(puck), Some(interface)) = (&self.puck, self.active) else {
            return;
        };
        let mut report = [0; 10];
        let length = haptic.encode(&mut report);
        let mut result = puck.write_interrupt(interface, &report[..length], USB_TIMEOUT);
        if result == Err(rusb::Error::Pipe) && puck.clear_halt(interface).is_ok() {
            result = puck.write_interrupt(interface, &report[..length], USB_TIMEOUT);
        }
        match result {
            Ok(written) if written == length => {}
            Ok(written) => log::warn!("short controller haptic write: {written}/{length}"),
            Err(rusb::Error::Pipe | rusb::Error::NoDevice) => {
                self.disconnected = self.reset();
            }
            Err(error) => log::warn!("controller haptic write failed: {error}"),
        }
    }

    fn scan(&mut self) {
        self.last_scan = Instant::now();
        let devices = match self.context.devices() {
            Ok(devices) => devices,
            Err(error) => {
                log::warn!("could not scan for Steam Controller puck: {error}");
                return;
            }
        };
        'devices: for device in devices.iter() {
            let Ok(descriptor) = device.device_descriptor() else {
                continue;
            };
            if descriptor.vendor_id() != VALVE_VENDOR_ID
                || descriptor.product_id() != PROTEUS_PRODUCT_ID
            {
                continue;
            }
            let handle = match device.open() {
                Ok(handle) => handle,
                Err(error) => {
                    log::warn!("could not open Steam Controller puck: {error}");
                    continue;
                }
            };
            if let Err(error) = handle.set_auto_detach_kernel_driver(true) {
                log::warn!("could not enable Steam Controller driver handoff: {error}");
                continue;
            }
            for interface in FIRST_INTERFACE..FIRST_INTERFACE + SLOT_COUNT {
                if let Err(error) = handle.claim_interface(interface) {
                    log::warn!(
                        "could not claim Steam Controller puck interface {interface}: {error}"
                    );
                    continue 'devices;
                }
            }
            self.puck = Some(handle);
            log::info!("claimed {SLOT_COUNT} Steam Controller puck slots");
            break;
        }
    }

    fn reset(&mut self) -> bool {
        let disconnected = self.active.take().is_some();
        self.last_state = None;
        self.puck = None;
        self.last_scan = Instant::now() - Duration::from_secs(2);
        disconnected
    }

    fn send_feature(&self, interface: u8, report: &[u8; 64]) -> bool {
        if let Some(puck) = &self.puck {
            match puck.write_control(0x21, 9, 0x0301, u16::from(interface), report, USB_TIMEOUT) {
                Ok(64) => return true,
                Ok(written) => log::warn!("short controller feature write: {written}/64"),
                Err(error) => log::warn!("controller feature write failed: {error}"),
            }
        }
        false
    }
}
