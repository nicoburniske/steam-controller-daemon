use evdev::KeyCode;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};

use super::{AxisComponent, Gamepad, GamepadButton, MouseButton, deserialize_key};
use crate::protocol::{Button, Buttons, Haptic};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub default_mode: String,
    #[serde(default)]
    pub mode_switch_haptic: Option<Haptic>,
    #[serde(default)]
    pub trackpad: Trackpad,
    #[serde(default)]
    pub osk: Osk,
    #[serde(default)]
    pub global: Global,
    pub mode: IndexMap<String, Mode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trackpad {
    #[serde(default = "default_click_pressure")]
    pub click_pressure: u16,
}

impl Default for Trackpad {
    fn default() -> Self {
        Self {
            click_pressure: default_click_pressure(),
        }
    }
}

const fn default_click_pressure() -> u16 {
    25
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Osk {
    #[serde(default)]
    pub bind: IndexMap<Button, OskKey>,
}

#[derive(Clone, Copy)]
pub struct OskKey(pub KeyCode);

impl<'de> Deserialize<'de> for OskKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_key(deserializer).map(Self)
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Global {
    #[serde(default)]
    pub bind: Vec<Binding>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    #[serde(default)]
    pub gamepad: Gamepad,
    #[serde(default)]
    pub gamepad_touchpad: Option<crate::protocol::Trackpad>,
    #[serde(default)]
    pub bind: Vec<Binding>,
    #[serde(default)]
    pub axis: Vec<AxisMapping>,
    #[serde(default)]
    pub layer: IndexMap<String, Layer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub hold: Button,
    #[serde(default)]
    pub bind: Vec<Binding>,
}

#[derive(Deserialize)]
pub struct Binding {
    pub input: BindingInput,
    #[serde(flatten)]
    pub action: Action,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum BindingInput {
    Button(Button),
    Chord(Vec<Button>),
}

impl BindingInput {
    pub fn buttons(&self) -> &[Button] {
        match self {
            Self::Button(button) => std::slice::from_ref(button),
            Self::Chord(buttons) => buttons,
        }
    }

    pub fn into_parts(self) -> Option<(Buttons, Button)> {
        let (mut inputs, trigger) = match self {
            Self::Button(trigger) => (Buttons::default(), trigger),
            Self::Chord(mut buttons) => {
                let trigger = buttons.pop()?;
                let mut inputs = Buttons::default();
                for button in buttons {
                    inputs.insert(button);
                }
                (inputs, trigger)
            }
        };
        inputs.insert(trigger);
        Some((inputs, trigger))
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Key {
        #[serde(alias = "code", deserialize_with = "deserialize_key")]
        key: KeyCode,
    },
    Mouse {
        button: MouseButton,
    },
    Gamepad {
        button: GamepadButton,
    },
    ModeSet {
        name: String,
    },
    ModeNext,
    KeyboardToggle,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AxisMapping {
    pub source: AnalogSource,
    pub target: AnalogTarget,
    #[serde(default)]
    pub activation: Option<AxisActivation>,
    #[serde(default)]
    pub deadzone: Option<f32>,
    #[serde(default)]
    pub sensitivity: Option<f32>,
    #[serde(default)]
    pub acceleration: Option<f32>,
    #[serde(default)]
    pub curve: Curve,
    #[serde(default)]
    pub exponent: Option<f32>,
    #[serde(default)]
    pub invert_x: bool,
    #[serde(default)]
    pub invert_y: bool,
    #[serde(default)]
    pub swap_xy: bool,
    #[serde(default)]
    pub components: Option<[AxisComponent; 2]>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum AxisActivation {
    Trigger(TriggerActivation),
    All { all: Vec<Button> },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerActivation {
    pub source: AnalogSource,
    pub engage: f32,
    pub release: f32,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AnalogSource {
    LeftStick,
    RightStick,
    LeftTrigger,
    RightTrigger,
    LeftPad,
    RightPad,
    Gyro,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AnalogTarget {
    GamepadLeftStick,
    GamepadRightStick,
    GamepadLeftTrigger,
    GamepadRightTrigger,
    MouseMotion,
    Scroll,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Curve {
    #[default]
    Linear,
    Exponential,
}
