use std::fmt;

pub const VALVE_VENDOR_ID: u16 = 0x28de;
pub const PROTEUS_PRODUCT_ID: u16 = 0x1304;

pub fn parse_report(report: &[u8]) -> Result<Report, ParseError> {
    let Some(&id) = report.first() else {
        return Err(ParseError::Empty);
    };

    match id {
        0x40 => {
            ensure_len(report, 6)?;
            Ok(Report::LizardMouse)
        }
        0x41 => {
            ensure_len(report, 9)?;
            Ok(Report::LizardKeyboard)
        }
        0x42 => parse_state(report, StateFormat::Standard).map(Report::State),
        0x43 => {
            ensure_len(report, 15)?;
            Ok(Report::Battery(BatteryState {
                charge_state: ChargeState::from(report[1]),
                level_percent: report[2],
                battery_voltage: le_u16(report, 3),
                system_voltage: le_u16(report, 5),
                input_voltage: le_u16(report, 7),
                current: le_u16(report, 9),
                input_current: le_u16(report, 11),
                temperature: le_u16(report, 13),
            }))
        }
        0x45 => parse_state(report, StateFormat::Ble).map(Report::State),
        0x46 | 0x79 => {
            ensure_len(report, 2)?;
            Ok(Report::Wireless(WirelessState::from(report[1])))
        }
        0x47 => parse_state(report, StateFormat::Timestamp32Us).map(Report::State),
        0x7b => {
            ensure_len(report, 13)?;
            Ok(Report::PuckStatus(PuckStatus {
                sequence: report[1],
                flags: report[2],
                controller_to_puck_rssi_dbm: report[8] as i8,
                link_quality: report[10],
            }))
        }
        _ => Ok(Report::Unknown {
            id,
            length: report.len(),
        }),
    }
}

pub fn lizard_mode_report(enabled: bool) -> [u8; 64] {
    setting_report(9, u16::from(enabled))
}

pub fn imu_mode_report(enabled: bool) -> [u8; 64] {
    setting_report(48, if enabled { 0x0018 } else { 0 })
}

pub fn rumble_report(low_frequency: u16, high_frequency: u16) -> [u8; 10] {
    let mut report = [0; 10];
    report[0] = 0x80;
    report[4..6].copy_from_slice(&low_frequency.to_le_bytes());
    report[7..9].copy_from_slice(&high_frequency.to_le_bytes());
    report
}

pub fn trackpad_haptic_report(trackpad: Trackpad) -> [u8; 4] {
    [0x82, trackpad as u8, 1, (-9_i8) as u8]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Report {
    State(ControllerState),
    Battery(BatteryState),
    Wireless(WirelessState),
    PuckStatus(PuckStatus),
    LizardMouse,
    LizardKeyboard,
    Unknown { id: u8, length: usize },
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ControllerState {
    pub format: StateFormat,
    pub sequence: u8,
    pub buttons: Buttons,
    pub triggers: [f32; 2],
    pub left_stick: [f32; 2],
    pub right_stick: [f32; 2],
    pub left_pad: TouchpadState,
    pub right_pad: TouchpadState,
    pub trackpad_timestamp_us: Option<u32>,
    pub imu_timestamp_us: u32,
    pub accel: [f32; 3],
    pub gyro: [f32; 3],
    pub quaternion: Option<[f32; 4]>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TouchpadState {
    pub touched: bool,
    pub clicked: bool,
    pub position: [f32; 2],
    pub pressure: f32,
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

    #[cfg(test)]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[cfg(test)]
    pub fn insert(&mut self, button: Button) {
        self.0 |= button.mask();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Button {
    A = 1 << 0,
    B = 1 << 1,
    X = 1 << 2,
    Y = 1 << 3,
    Qam = 1 << 4,
    R3 = 1 << 5,
    View = 1 << 6,
    R4 = 1 << 7,
    R5 = 1 << 8,
    Rb = 1 << 9,
    DpadDown = 1 << 10,
    DpadRight = 1 << 11,
    DpadLeft = 1 << 12,
    DpadUp = 1 << 13,
    Menu = 1 << 14,
    L3 = 1 << 15,
    Steam = 1 << 16,
    L4 = 1 << 17,
    L5 = 1 << 18,
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
    pub const fn mask(self) -> u32 {
        self as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryState {
    pub charge_state: ChargeState,
    pub level_percent: u8,
    pub battery_voltage: u16,
    pub system_voltage: u16,
    pub input_voltage: u16,
    pub current: u16,
    pub input_current: u16,
    pub temperature: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeState {
    Reset,
    Discharging,
    Charging,
    SourceValidate,
    ChargingDone,
    Unknown(u8),
}

impl ChargeState {
    pub const fn is_charging(self) -> bool {
        matches!(self, Self::Charging | Self::ChargingDone)
    }
}

impl From<u8> for ChargeState {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Reset,
            1 => Self::Discharging,
            2 => Self::Charging,
            3 => Self::SourceValidate,
            4 => Self::ChargingDone,
            value => Self::Unknown(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WirelessState {
    Disconnected,
    Connected,
    Unknown(u8),
}

impl From<u8> for WirelessState {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Disconnected,
            2 => Self::Connected,
            value => Self::Unknown(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PuckStatus {
    pub sequence: u8,
    pub flags: u8,
    pub controller_to_puck_rssi_dbm: i8,
    pub link_quality: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    Truncated {
        report_id: u8,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("empty HID report"),
            Self::Truncated {
                report_id,
                expected,
                actual,
            } => write!(
                formatter,
                "truncated HID report 0x{report_id:02x}: expected {expected} bytes, got {actual}"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

fn parse_state(report: &[u8], format: StateFormat) -> Result<ControllerState, ParseError> {
    ensure_len(report, 46)?;

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
    let accel_scale = 2.0 * 9.80665 / 32768.0;

    Ok(ControllerState {
        format,
        sequence: report[1],
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
            pressure: pressure_unit(le_u16(report, pad_offset + 4)),
        },
        right_pad: TouchpadState {
            touched: buttons.contains(Button::RightPadTouch),
            clicked: buttons.contains(Button::RightPadClick),
            position: [
                signed_unit(le_i16(report, pad_offset + 6)),
                -signed_unit(le_i16(report, pad_offset + 8)),
            ],
            pressure: pressure_unit(le_u16(report, pad_offset + 10)),
        },
        trackpad_timestamp_us,
        imu_timestamp_us,
        accel: [
            f32::from(le_i16(report, 34)) * accel_scale,
            f32::from(le_i16(report, 38)) * accel_scale,
            -f32::from(le_i16(report, 36)) * accel_scale,
        ],
        gyro: [
            f32::from(le_i16(report, 40)) * gyro_scale,
            f32::from(le_i16(report, 44)) * gyro_scale,
            -f32::from(le_i16(report, 42)) * gyro_scale,
        ],
        quaternion: (format == StateFormat::Standard && report.len() >= 54).then(|| {
            [
                signed_unit(le_i16(report, 46)),
                signed_unit(le_i16(report, 48)),
                signed_unit(le_i16(report, 50)),
                signed_unit(le_i16(report, 52)),
            ]
        }),
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

fn ensure_len(report: &[u8], expected: usize) -> Result<(), ParseError> {
    if report.len() < expected {
        return Err(ParseError::Truncated {
            report_id: report[0],
            expected,
            actual: report.len(),
        });
    }
    Ok(())
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

fn pressure_unit(value: u16) -> f32 {
    (f32::from(value) / 32768.0).min(1.0)
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
        assert_eq!(state.sequence, 0xcc);
        assert!(state.buttons.is_empty());
        assert!((state.left_stick[0] - 222.0 / 32767.0).abs() < f32::EPSILON);
        assert!((state.left_stick[1] + 336.0 / 32767.0).abs() < f32::EPSILON);
        assert_eq!(state.imu_timestamp_us, 566_874);
        assert!(state.quaternion.is_some());
    }

    #[test]
    fn parses_buttons_and_normalizes_extremes() {
        let mut report = [0; 54];
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
        report[22..24].copy_from_slice(&u16::MAX.to_le_bytes());

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
        assert_eq!(state.left_pad.pressure, 1.0);
    }

    #[test]
    fn parses_ble_and_timestamped_layouts() {
        let mut ble = [0; 46];
        ble[0] = 0x45;
        let Report::State(ble) = parse_report(&ble).unwrap() else {
            panic!("expected BLE state report");
        };
        assert_eq!(ble.format, StateFormat::Ble);
        assert_eq!(ble.quaternion, None);

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
    fn parses_standard_reports_with_and_without_quaternion() {
        let mut no_quaternion = [0; 46];
        no_quaternion[0] = 0x42;
        let Report::State(no_quaternion) = parse_report(&no_quaternion).unwrap() else {
            panic!("expected state report");
        };
        assert_eq!(no_quaternion.format, StateFormat::Standard);
        assert_eq!(no_quaternion.quaternion, None);

        let mut quaternion = [0; 54];
        quaternion[0] = 0x42;
        quaternion[46..48].copy_from_slice(&i16::MAX.to_le_bytes());
        let Report::State(quaternion) = parse_report(&quaternion).unwrap() else {
            panic!("expected state report");
        };
        assert_eq!(quaternion.quaternion, Some([1.0, 0.0, 0.0, 0.0]));
    }

    #[test]
    fn parses_battery_wireless_and_puck_status() {
        let battery = [
            0x43, 2, 83, 0x34, 0x12, 0x78, 0x56, 0xbc, 0x9a, 1, 0, 2, 0, 3, 0,
        ];
        let Report::Battery(battery) = parse_report(&battery).unwrap() else {
            panic!("expected battery report");
        };
        assert_eq!(battery.charge_state, ChargeState::Charging);
        assert!(battery.charge_state.is_charging());
        assert_eq!(battery.level_percent, 83);
        assert_eq!(battery.battery_voltage, 0x1234);

        assert_eq!(
            parse_report(&[0x79, 2]),
            Ok(Report::Wireless(WirelessState::Connected))
        );
        assert_eq!(
            parse_report(&[0x46, 1]),
            Ok(Report::Wireless(WirelessState::Disconnected))
        );

        let mut puck = [0; 13];
        puck[0] = 0x7b;
        puck[1] = 0xf7;
        puck[2] = 0x86;
        puck[8] = 0xc2;
        puck[10] = 0x4c;
        assert_eq!(
            parse_report(&puck),
            Ok(Report::PuckStatus(PuckStatus {
                sequence: 0xf7,
                flags: 0x86,
                controller_to_puck_rssi_dbm: -62,
                link_quality: 0x4c,
            }))
        );
    }

    #[test]
    fn builds_feature_and_rumble_reports() {
        let lizard = lizard_mode_report(false);
        assert_eq!(&lizard[..6], &[1, 0x87, 3, 9, 0, 0]);
        assert!(lizard[6..].iter().all(|byte| *byte == 0));

        let imu = imu_mode_report(true);
        assert_eq!(&imu[..6], &[1, 0x87, 3, 48, 0x18, 0]);

        assert_eq!(
            rumble_report(0x1234, 0x5678),
            [0x80, 0, 0, 0, 0x34, 0x12, 0, 0x78, 0x56, 0]
        );
        assert_eq!(trackpad_haptic_report(Trackpad::Left), [0x82, 0, 1, 0xf7]);
        assert_eq!(trackpad_haptic_report(Trackpad::Right), [0x82, 1, 1, 0xf7]);
    }

    #[test]
    fn reports_truncation_without_rejecting_unknown_reports() {
        assert_eq!(parse_report(&[]), Err(ParseError::Empty));
        assert_eq!(
            parse_report(&[0x42; 45]),
            Err(ParseError::Truncated {
                report_id: 0x42,
                expected: 46,
                actual: 45,
            })
        );
        assert_eq!(
            parse_report(&[0xaa, 1, 2]),
            Ok(Report::Unknown {
                id: 0xaa,
                length: 3,
            })
        );
    }
}
