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
const USB_TIMEOUT: Duration = Duration::from_millis(100);

pub struct DeviceManager {
    context: Context,
    puck: Option<DeviceHandle<Context>>,
    active: Option<u8>,
    trackpad_click_pressure: u16,
    last_state: Option<Instant>,
    last_scan: Instant,
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
        };
        manager.scan()?;
        Ok(manager)
    }

    pub fn poll(&mut self) -> Result<Option<DeviceEvent>> {
        if self.puck.is_none() && self.last_scan.elapsed() >= Duration::from_secs(1) {
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

        let Some(puck) = &self.puck else {
            return Ok(None);
        };
        let mut report = [0; 64];
        for slot in 0..SLOT_COUNT {
            let interface = FIRST_INTERFACE + slot;
            let length =
                match puck.read_interrupt(0x80 | (interface + 1), &mut report, POLL_TIMEOUT) {
                    Ok(length) => length,
                    Err(rusb::Error::Timeout) => continue,
                    Err(error) => {
                        log::warn!("controller read failed: {error}");
                        let disconnected = self.active.take().is_some();
                        self.last_state = None;
                        self.puck = None;
                        self.last_scan = Instant::now() - Duration::from_secs(2);
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
                        self.active = Some(interface);
                        self.configure(interface)?;
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

    pub fn suppress_lizard_mode(&self) -> Result<()> {
        if let Some(interface) = self.active {
            self.send_feature(interface, &lizard_mode_report(false))?;
        }
        Ok(())
    }

    pub fn set_trackpad_click_pressure(&mut self, pressure: u16) -> Result<()> {
        self.trackpad_click_pressure = pressure;
        if let Some(interface) = self.active {
            for pad in [Trackpad::Left, Trackpad::Right] {
                self.send_feature(interface, &trackpad_click_pressure_report(pad, pressure))?;
            }
        }
        Ok(())
    }

    pub fn rumble(&self, low_frequency: u16, high_frequency: u16) -> Result<()> {
        self.play_haptic(Haptic::Rumble {
            low_frequency,
            high_frequency,
        })
    }

    pub fn trackpad_haptic(&self, trackpad: Trackpad) -> Result<()> {
        self.play_haptic(Haptic::TrackpadClick {
            trackpad,
            gain: -15,
        })
    }

    pub fn play_haptic(&self, haptic: Haptic) -> Result<()> {
        let (Some(puck), Some(interface)) = (&self.puck, self.active) else {
            return Ok(());
        };
        let mut report = [0; 10];
        let length = haptic.encode(&mut report);
        puck.write_interrupt(interface, &report[..length], USB_TIMEOUT)?;
        Ok(())
    }

    fn scan(&mut self) -> Result<()> {
        self.last_scan = Instant::now();
        for device in self.context.devices()?.iter() {
            let descriptor = device.device_descriptor()?;
            if descriptor.vendor_id() != VALVE_VENDOR_ID
                || descriptor.product_id() != PROTEUS_PRODUCT_ID
            {
                continue;
            }
            let handle = device.open()?;
            handle.set_auto_detach_kernel_driver(true)?;
            for interface in FIRST_INTERFACE..FIRST_INTERFACE + SLOT_COUNT {
                handle.claim_interface(interface)?;
            }
            self.puck = Some(handle);
            log::info!("claimed {SLOT_COUNT} Steam Controller puck slots");
            break;
        }
        Ok(())
    }

    fn configure(&self, interface: u8) -> Result<()> {
        self.send_feature(interface, &lizard_mode_report(false))?;
        self.send_feature(interface, &imu_mode_report(true))?;
        for pad in [Trackpad::Left, Trackpad::Right] {
            self.send_feature(
                interface,
                &trackpad_click_pressure_report(pad, self.trackpad_click_pressure),
            )?;
        }
        Ok(())
    }

    fn send_feature(&self, interface: u8, report: &[u8; 64]) -> Result<()> {
        if let Some(puck) = &self.puck {
            puck.write_control(0x21, 9, 0x0301, u16::from(interface), report, USB_TIMEOUT)?;
        }
        Ok(())
    }
}
