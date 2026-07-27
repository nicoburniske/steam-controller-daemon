use hidapi::{HidApi, HidDevice};
use scd::HapticSound;
use scd::protocol::{
    ControllerState, Haptic, PROTEUS_PRODUCT_ID, Report, Trackpad, VALVE_VENDOR_ID,
    imu_mode_report, lizard_mode_report, parse_report, trackpad_click_pressure_report,
};
use std::ffi::CString;
use std::time::{Duration, Instant};

use scd::{Result, ResultExt};

pub struct DeviceManager {
    api: HidApi,
    slots: Vec<Slot>,
    active: Option<usize>,
    trackpad_click_pressure: u16,
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
    pub fn new(trackpad_click_pressure: u16) -> Result<Self> {
        let api = HidApi::new().whence()?;
        let mut manager = Self {
            api,
            slots: Vec::new(),
            active: None,
            trackpad_click_pressure,
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
                                for pad in [Trackpad::Left, Trackpad::Right] {
                                    self.slots[index]
                                        .device
                                        .send_feature_report(&trackpad_click_pressure_report(
                                            pad,
                                            self.trackpad_click_pressure,
                                        ))
                                        .whence()?;
                                }
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
                    return Ok(disconnected.then_some(DeviceEvent::Disconnected));
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

    pub fn set_trackpad_click_pressure(&mut self, pressure: u16) -> Result<()> {
        self.trackpad_click_pressure = pressure;
        if let Some(active) = self.active {
            for pad in [Trackpad::Left, Trackpad::Right] {
                self.slots[active]
                    .device
                    .send_feature_report(&trackpad_click_pressure_report(pad, pressure))
                    .whence()?;
            }
        }
        Ok(())
    }

    pub fn rumble(&self, low_frequency: u16, high_frequency: u16) -> Result<()> {
        self.write_haptic(Haptic::Rumble {
            low_frequency,
            high_frequency,
        })
    }

    pub fn trackpad_haptic(&self, trackpad: Trackpad) -> Result<()> {
        self.write_haptic(Haptic::TrackpadClick {
            trackpad,
            gain: -15,
        })
    }

    pub fn mode_switch_haptic(&self) -> Result<()> {
        self.play_haptic(HapticSound::ToneHigh)
    }

    pub fn play_haptic(&self, sound: HapticSound) -> Result<()> {
        self.write_haptic(match sound {
            HapticSound::ControllerOn => Haptic::Script {
                script: 1,
                gain: -10,
            },
            HapticSound::ControllerOff => Haptic::Script {
                script: 5,
                gain: -10,
            },
            HapticSound::UpFive => Haptic::Script {
                script: 6,
                gain: -10,
            },
            HapticSound::DownFive => Haptic::Script {
                script: 7,
                gain: -10,
            },
            HapticSound::UpSix => Haptic::Script {
                script: 8,
                gain: -10,
            },
            HapticSound::DownSix => Haptic::Script {
                script: 9,
                gain: -10,
            },
            HapticSound::WhoopUpThree => Haptic::Script {
                script: 10,
                gain: -10,
            },
            HapticSound::WhoopDown => Haptic::Script {
                script: 11,
                gain: -10,
            },
            HapticSound::Pulse => Haptic::Pulse {
                on_us: 625,
                off_us: 625,
                repeat: 48,
            },
            HapticSound::ToneLow => Haptic::Tone {
                gain: 0,
                frequency: 440,
                duration_ms: 120,
            },
            HapticSound::ToneHigh => Haptic::Tone {
                gain: 0,
                frequency: 880,
                duration_ms: 90,
            },
            HapticSound::SweepUp => Haptic::LogSweep {
                gain: 0,
                duration_ms: 120,
                start_frequency: 400,
                end_frequency: 900,
            },
            HapticSound::SweepDown => Haptic::LogSweep {
                gain: 0,
                duration_ms: 120,
                start_frequency: 900,
                end_frequency: 400,
            },
            HapticSound::TrillUp => Haptic::Script {
                script: 3,
                gain: -10,
            },
            HapticSound::TrillDown => Haptic::Script {
                script: 4,
                gain: -10,
            },
        })
    }

    fn write_haptic(&self, haptic: Haptic) -> Result<()> {
        let Some(active) = self.active else {
            return Ok(());
        };
        let mut report = [0; 10];
        let length = haptic.encode(&mut report);
        self.slots[active]
            .device
            .write(&report[..length])
            .whence()?;
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
