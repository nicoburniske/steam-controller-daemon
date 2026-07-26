use serde::Deserialize;

pub const VALVE_VENDOR_ID: u16 = 0x28de;
pub const PROTEUS_PRODUCT_ID: u16 = 0x1304;

pub fn parse_report(report: &[u8]) -> Option<Report> {
    match *report.first()? {
        0x42 => parse_state(report, StateFormat::Standard).map(Report::State),
        0x43 if report.len() >= 3 => Some(Report::Battery {
            percent: report[2].min(100),
            charging: matches!(report[1], 2 | 4),
        }),
        0x45 => parse_state(report, StateFormat::Ble).map(Report::State),
        0x46 | 0x79 if report.get(1) == Some(&1) => Some(Report::Disconnected),
        0x47 => parse_state(report, StateFormat::Timestamp32Us).map(Report::State),
        _ => None,
    }
}

pub fn lizard_mode_report(enabled: bool) -> [u8; 64] {
    setting_report(9, u16::from(enabled))
}

pub fn imu_mode_report(enabled: bool) -> [u8; 64] {
    setting_report(48, if enabled { 0x0018 } else { 0 })
}

pub fn trackpad_click_pressure_report(trackpad: Trackpad, pressure: u16) -> [u8; 64] {
    setting_report(
        match trackpad {
            Trackpad::Left => 52,
            Trackpad::Right => 53,
        },
        pressure,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Haptic {
    Rumble {
        low_frequency: u16,
        high_frequency: u16,
    },
    TrackpadClick {
        trackpad: Trackpad,
        gain: i8,
    },
    Pulse {
        on_us: u16,
        off_us: u16,
        repeat: u16,
    },
    Tone {
        gain: i8,
        frequency: u16,
        duration_ms: u16,
    },
    LogSweep {
        gain: i8,
        duration_ms: u16,
        start_frequency: u16,
        end_frequency: u16,
    },
    Script {
        script: u8,
        gain: i8,
    },
}

impl Haptic {
    pub fn encode(self, report: &mut [u8; 10]) -> usize {
        report.fill(0);
        match self {
            Self::Rumble {
                low_frequency,
                high_frequency,
            } => {
                report[0] = 0x80;
                report[4..6].copy_from_slice(&low_frequency.to_le_bytes());
                report[7..9].copy_from_slice(&high_frequency.to_le_bytes());
                10
            }
            Self::TrackpadClick { trackpad, gain } => {
                report[..4].copy_from_slice(&[0x82, trackpad as u8, 1, gain as u8]);
                4
            }
            Self::Pulse {
                on_us,
                off_us,
                repeat,
            } => {
                report[0] = 0x81;
                report[1] = 2;
                report[2..4].copy_from_slice(&on_us.to_le_bytes());
                report[4..6].copy_from_slice(&off_us.to_le_bytes());
                report[6..8].copy_from_slice(&repeat.to_le_bytes());
                8
            }
            Self::Tone {
                gain,
                frequency,
                duration_ms,
            } => {
                report[0] = 0x83;
                report[1] = 2;
                report[2] = gain as u8;
                report[3..5].copy_from_slice(&frequency.to_le_bytes());
                report[5..7].copy_from_slice(&duration_ms.to_le_bytes());
                10
            }
            Self::LogSweep {
                gain,
                duration_ms,
                start_frequency,
                end_frequency,
            } => {
                report[0] = 0x84;
                report[1] = 2;
                report[2] = gain as u8;
                report[3..5].copy_from_slice(&duration_ms.to_le_bytes());
                report[5..7].copy_from_slice(&start_frequency.to_le_bytes());
                report[7..9].copy_from_slice(&end_frequency.to_le_bytes());
                9
            }
            Self::Script { script, gain } => {
                report[..4].copy_from_slice(&[0x85, 2, script, gain as u8]);
                4
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Report {
    State(ControllerState),
    Battery { percent: u8, charging: bool },
    Disconnected,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ControllerState {
    pub format: StateFormat,
    pub buttons: Buttons,
    pub triggers: [f32; 2],
    pub left_stick: [f32; 2],
    pub right_stick: [f32; 2],
    pub left_pad: TouchpadState,
    pub right_pad: TouchpadState,
    pub trackpad_timestamp_us: Option<u32>,
    pub imu_timestamp_us: u32,
    pub gyro: [f32; 3],
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TouchpadState {
    pub touched: bool,
    pub clicked: bool,
    pub position: [f32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Trackpad {
    Left = 0,
    Right = 1,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StateFormat {
    #[default]
    Standard,
    Ble,
    Timestamp32Us,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons(u32);

impl Buttons {
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn contains(self, button: Button) -> bool {
        self.0 & button.mask() != 0
    }

    pub fn insert(&mut self, button: Button) {
        self.0 |= button.mask();
    }

    pub fn remove(&mut self, button: Button) {
        self.0 &= !button.mask();
    }

    pub fn remove_inactive(&mut self, active: Self) {
        self.0 &= active.0;
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
#[repr(u32)]
pub enum Button {
    A = 1 << 0,
    B = 1 << 1,
    X = 1 << 2,
    Y = 1 << 3,
    #[serde(rename = "quick-access", alias = "qam")]
    Qam = 1 << 4,
    #[serde(rename = "right-stick-click", alias = "r3")]
    R3 = 1 << 5,
    View = 1 << 6,
    R4 = 1 << 7,
    R5 = 1 << 8,
    #[serde(rename = "right-bumper", alias = "rb")]
    Rb = 1 << 9,
    DpadDown = 1 << 10,
    DpadRight = 1 << 11,
    DpadLeft = 1 << 12,
    DpadUp = 1 << 13,
    Menu = 1 << 14,
    #[serde(rename = "left-stick-click", alias = "l3")]
    L3 = 1 << 15,
    Steam = 1 << 16,
    L4 = 1 << 17,
    L5 = 1 << 18,
    #[serde(rename = "left-bumper", alias = "lb")]
    Lb = 1 << 19,
    RightStickTouch = 1 << 20,
    RightPadTouch = 1 << 21,
    RightPadClick = 1 << 22,
    RightTriggerClick = 1 << 23,
    LeftStickTouch = 1 << 24,
    LeftPadTouch = 1 << 25,
    LeftPadClick = 1 << 26,
    LeftTriggerClick = 1 << 27,
    RightGripTouch = 1 << 28,
    LeftGripTouch = 1 << 29,
}

impl Button {
    pub const ALL: [Self; 30] = [
        Self::A,
        Self::B,
        Self::X,
        Self::Y,
        Self::Qam,
        Self::R3,
        Self::View,
        Self::R4,
        Self::R5,
        Self::Rb,
        Self::DpadDown,
        Self::DpadRight,
        Self::DpadLeft,
        Self::DpadUp,
        Self::Menu,
        Self::L3,
        Self::Steam,
        Self::L4,
        Self::L5,
        Self::Lb,
        Self::RightStickTouch,
        Self::RightPadTouch,
        Self::RightPadClick,
        Self::RightTriggerClick,
        Self::LeftStickTouch,
        Self::LeftPadTouch,
        Self::LeftPadClick,
        Self::LeftTriggerClick,
        Self::RightGripTouch,
        Self::LeftGripTouch,
    ];

    pub const fn mask(self) -> u32 {
        self as u32
    }

    pub const fn index(self) -> usize {
        self.mask().trailing_zeros() as usize
    }
}

fn parse_state(report: &[u8], format: StateFormat) -> Option<ControllerState> {
    if report.len() < 46 {
        return None;
    }

    let buttons = Buttons::from_bits(le_u32(report, 2));
    let (pad_offset, trackpad_timestamp_us, imu_timestamp_us) =
        if format == StateFormat::Timestamp32Us {
            (
                20,
                Some(u32::from(le_u16(report, 18)) * 32),
                u32::from(le_u16(report, 32)) * 32,
            )
        } else {
            (18, None, le_u32(report, 30))
        };
    let gyro_scale = 2000.0 * (std::f32::consts::PI / 180.0) / 32768.0;

    Some(ControllerState {
        format,
        buttons,
        triggers: [
            trigger_unit(le_i16(report, 6)),
            trigger_unit(le_i16(report, 8)),
        ],
        left_stick: [
            signed_unit(le_i16(report, 10)),
            -signed_unit(le_i16(report, 12)),
        ],
        right_stick: [
            signed_unit(le_i16(report, 14)),
            -signed_unit(le_i16(report, 16)),
        ],
        left_pad: TouchpadState {
            touched: buttons.contains(Button::LeftPadTouch),
            clicked: buttons.contains(Button::LeftPadClick),
            position: [
                signed_unit(le_i16(report, pad_offset)),
                -signed_unit(le_i16(report, pad_offset + 2)),
            ],
        },
        right_pad: TouchpadState {
            touched: buttons.contains(Button::RightPadTouch),
            clicked: buttons.contains(Button::RightPadClick),
            position: [
                signed_unit(le_i16(report, pad_offset + 6)),
                -signed_unit(le_i16(report, pad_offset + 8)),
            ],
        },
        trackpad_timestamp_us,
        imu_timestamp_us,
        gyro: [
            f32::from(le_i16(report, 40)) * gyro_scale,
            f32::from(le_i16(report, 44)) * gyro_scale,
            -f32::from(le_i16(report, 42)) * gyro_scale,
        ],
    })
}

fn setting_report(setting: u8, value: u16) -> [u8; 64] {
    let mut report = [0; 64];
    report[0] = 1;
    report[1] = 0x87;
    report[2] = 3;
    report[3] = setting;
    report[4..6].copy_from_slice(&value.to_le_bytes());
    report
}

fn le_i16(report: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes([report[offset], report[offset + 1]])
}

fn le_u16(report: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([report[offset], report[offset + 1]])
}

fn le_u32(report: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        report[offset],
        report[offset + 1],
        report[offset + 2],
        report[offset + 3],
    ])
}

fn signed_unit(value: i16) -> f32 {
    if value < 0 {
        f32::from(value) / 32768.0
    } else {
        f32::from(value) / 32767.0
    }
}

fn trigger_unit(value: i16) -> f32 {
    f32::from(value.max(0)) / 32767.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reference_idle_report() {
        let report = [
            0x42, 0xcc, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xde, 0x00, 0x50, 0x01,
            0x88, 0xff, 0xe5, 0xfe, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x5a, 0xa6, 0x08, 0x00, 0x3f, 0x03, 0xe7, 0x38, 0x0e, 0x1a, 0x52, 0xfe,
            0x13, 0x01, 0x79, 0xfc, 0x5d, 0x67, 0x87, 0x3b, 0x05, 0xe8, 0x41, 0xd8,
        ];

        let Report::State(state) = parse_report(&report).unwrap() else {
            panic!("expected state report");
        };
        assert_eq!(state.format, StateFormat::Standard);
        assert_eq!(state.buttons, Buttons::default());
        assert!((state.left_stick[0] - 222.0 / 32767.0).abs() < f32::EPSILON);
        assert!((state.left_stick[1] + 336.0 / 32767.0).abs() < f32::EPSILON);
        assert_eq!(state.imu_timestamp_us, 566_874);
    }

    #[test]
    fn parses_buttons_and_normalizes_extremes() {
        let mut report = [0; 46];
        report[0] = 0x42;
        report[1] = 7;
        report[2..6].copy_from_slice(
            &(Button::Steam.mask()
                | Button::X.mask()
                | Button::LeftPadTouch.mask()
                | Button::LeftPadClick.mask())
            .to_le_bytes(),
        );
        report[6..8].copy_from_slice(&i16::MAX.to_le_bytes());
        report[10..12].copy_from_slice(&i16::MIN.to_le_bytes());
        report[12..14].copy_from_slice(&i16::MAX.to_le_bytes());
        report[18..20].copy_from_slice(&i16::MIN.to_le_bytes());
        report[20..22].copy_from_slice(&i16::MAX.to_le_bytes());

        let Report::State(state) = parse_report(&report).unwrap() else {
            panic!("expected state report");
        };
        assert!(state.buttons.contains(Button::Steam));
        assert!(state.buttons.contains(Button::X));
        assert!(state.left_pad.touched);
        assert!(state.left_pad.clicked);
        assert_eq!(state.triggers[0], 1.0);
        assert_eq!(state.left_stick, [-1.0, -1.0]);
        assert_eq!(state.left_pad.position, [-1.0, -1.0]);
    }

    #[test]
    fn parses_ble_and_timestamped_layouts() {
        let mut ble = [0; 46];
        ble[0] = 0x45;
        let Report::State(ble) = parse_report(&ble).unwrap() else {
            panic!("expected BLE state report");
        };
        assert_eq!(ble.format, StateFormat::Ble);

        let mut timestamped = [0; 46];
        timestamped[0] = 0x47;
        timestamped[18..20].copy_from_slice(&7_u16.to_le_bytes());
        timestamped[20..22].copy_from_slice(&16384_i16.to_le_bytes());
        timestamped[32..34].copy_from_slice(&11_u16.to_le_bytes());
        let Report::State(timestamped) = parse_report(&timestamped).unwrap() else {
            panic!("expected timestamped state report");
        };
        assert_eq!(timestamped.format, StateFormat::Timestamp32Us);
        assert_eq!(timestamped.trackpad_timestamp_us, Some(224));
        assert_eq!(timestamped.imu_timestamp_us, 352);
        assert!((timestamped.left_pad.position[0] - 16384.0 / 32767.0).abs() < f32::EPSILON);
    }

    #[test]
    fn parses_status_reports_and_ignores_irrelevant_input() {
        assert_eq!(
            parse_report(&[0x43, 2, 83]),
            Some(Report::Battery {
                percent: 83,
                charging: true
            })
        );
        assert_eq!(parse_report(&[0x46, 1]), Some(Report::Disconnected));
        assert_eq!(parse_report(&[0x79, 2]), None);
        assert_eq!(parse_report(&[0x7b; 13]), None);
        assert_eq!(parse_report(&[0xaa, 1, 2]), None);
        assert_eq!(parse_report(&[0x42; 45]), None);
        assert_eq!(parse_report(&[]), None);
    }

    #[test]
    fn builds_feature_and_haptic_reports() {
        let lizard = lizard_mode_report(false);
        assert_eq!(&lizard[..6], &[1, 0x87, 3, 9, 0, 0]);
        assert!(lizard[6..].iter().all(|byte| *byte == 0));

        let imu = imu_mode_report(true);
        assert_eq!(&imu[..6], &[1, 0x87, 3, 48, 0x18, 0]);

        assert_eq!(
            &trackpad_click_pressure_report(Trackpad::Left, 25)[..6],
            &[1, 0x87, 3, 52, 25, 0]
        );
        assert_eq!(
            &trackpad_click_pressure_report(Trackpad::Right, 25)[..6],
            &[1, 0x87, 3, 53, 25, 0]
        );

        let mut report = [0; 10];
        let length = Haptic::Rumble {
            low_frequency: 0x1234,
            high_frequency: 0x5678,
        }
        .encode(&mut report);
        assert_eq!(
            &report[..length],
            &[0x80, 0, 0, 0, 0x34, 0x12, 0, 0x78, 0x56, 0]
        );

        let length = Haptic::TrackpadClick {
            trackpad: Trackpad::Right,
            gain: -15,
        }
        .encode(&mut report);
        assert_eq!(&report[..length], &[0x82, 1, 1, 0xf1]);

        let length = Haptic::Pulse {
            on_us: 625,
            off_us: 625,
            repeat: 48,
        }
        .encode(&mut report);
        assert_eq!(&report[..length], &[0x81, 2, 0x71, 2, 0x71, 2, 48, 0]);
    }
}
