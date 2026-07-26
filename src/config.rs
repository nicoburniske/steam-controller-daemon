use std::{error::Error, fmt, fs, path::Path, str::FromStr};

use evdev::KeyCode;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub default_mode: String,
    #[serde(default)]
    pub global: Global,
    pub modes: IndexMap<String, Mode>,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Self::parse(&fs::read_to_string(path).map_err(ConfigError::Io)?)
    }

    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(source).map_err(ConfigError::Parse)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();

        if self.version != 1 {
            errors.push(format!("version must be 1, got {}", self.version));
        }
        if self.default_mode.trim().is_empty() {
            errors.push("default_mode must not be empty".to_owned());
        } else if !self.modes.contains_key(&self.default_mode) {
            errors.push(format!(
                "default_mode {:?} does not name a configured mode",
                self.default_mode
            ));
        }
        if self.modes.is_empty() {
            errors.push("at least one mode is required".to_owned());
        }

        for (scope, bindings) in std::iter::once(("global".to_owned(), &self.global.bindings))
            .chain(
                self.modes
                    .iter()
                    .map(|(name, mode)| (format!("mode {name:?}"), &mode.bindings)),
            )
            .chain(self.modes.iter().flat_map(|(mode_name, mode)| {
                mode.layers.iter().map(move |(layer_name, layer)| {
                    (
                        format!("mode {mode_name:?} layer {layer_name:?}"),
                        &layer.bindings,
                    )
                })
            }))
        {
            for (index, binding) in bindings.iter().enumerate() {
                let location = format!("{scope} binding {index}");
                match (&binding.input, &binding.chord) {
                    (Some(_), Some(_)) => {
                        errors.push(format!(
                            "{location} must have either input or chord, not both"
                        ));
                    }
                    (None, None) => {
                        errors.push(format!("{location} must have input or chord"));
                    }
                    (_, Some(chord)) if chord.is_empty() => {
                        errors.push(format!("{location} chord must not be empty"));
                    }
                    (_, Some(chord)) => {
                        for (position, input) in chord.iter().enumerate() {
                            if chord[..position].contains(input) {
                                errors.push(format!(
                                    "{location} chord contains duplicate input {input:?}"
                                ));
                            }
                        }
                    }
                    _ => {}
                }

                for input in binding.input.iter().chain(binding.chord.iter().flatten()) {
                    if let DigitalInput::Axis(threshold) = input {
                        if !threshold.threshold.is_finite()
                            || threshold.threshold <= 0.0
                            || threshold.threshold > 1.0
                        {
                            errors.push(format!(
                                "{location} axis threshold must be finite and in (0, 1]"
                            ));
                        }
                    }
                }

                match &binding.action {
                    Action::Key { key } => {
                        if *key == KeyCode::KEY_RESERVED
                            || (0x100..=0x15f).contains(&key.code())
                            || (0x2c0..=0x2ff).contains(&key.code())
                        {
                            errors.push(format!("{location} key {key:?} is not a keyboard key"));
                        }
                        if binding.activation == Activation::Release {
                            errors.push(format!(
                                "{location} release activation is only valid for events and mode actions"
                            ));
                        }
                    }
                    Action::ModeSet { name } if !self.modes.contains_key(name) => {
                        errors.push(format!(
                            "{location} mode-set target {name:?} is not configured"
                        ));
                    }
                    Action::Event { name } if name.trim().is_empty() => {
                        errors.push(format!("{location} event name must not be empty"));
                    }
                    Action::Mouse { .. } | Action::Gamepad { .. }
                        if binding.activation == Activation::Release =>
                    {
                        errors.push(format!(
                            "{location} release activation is only valid for events and mode actions"
                        ));
                    }
                    _ => {}
                }
            }
        }

        for (name, mode) in &self.modes {
            if name.trim().is_empty() {
                errors.push("mode names must not be empty".to_owned());
            }

            for (layer_index, (layer_name, layer)) in mode.layers.iter().enumerate() {
                let location = format!("mode {name:?} layer {layer_name:?}");
                if layer_name.trim().is_empty() {
                    errors.push(format!("{location} name must not be empty"));
                }
                if let DigitalInput::Axis(threshold) = &layer.hold {
                    if !threshold.threshold.is_finite()
                        || threshold.threshold <= 0.0
                        || threshold.threshold > 1.0
                    {
                        errors.push(format!(
                            "{location} hold axis threshold must be finite and in (0, 1]"
                        ));
                    }
                }
                if mode.layers.values().take(layer_index).any(|earlier| {
                    match (&earlier.hold, &layer.hold) {
                        (DigitalInput::Button(left), DigitalInput::Button(right)) => left == right,
                        (DigitalInput::Axis(left), DigitalInput::Axis(right)) => {
                            left.axis == right.axis
                        }
                        _ => false,
                    }
                }) {
                    errors.push(format!(
                        "{location} hold conflicts with an earlier layer hold"
                    ));
                }
                for (binding_index, binding) in layer.bindings.iter().enumerate() {
                    if binding
                        .input
                        .iter()
                        .chain(binding.chord.iter().flatten())
                        .any(|input| match (input, &layer.hold) {
                            (DigitalInput::Button(left), DigitalInput::Button(right)) => {
                                left == right
                            }
                            (DigitalInput::Axis(left), DigitalInput::Axis(right)) => {
                                left.axis == right.axis
                            }
                            _ => false,
                        })
                    {
                        errors.push(format!(
                            "{location} binding {binding_index} must not use its own hold input"
                        ));
                    }
                }
            }

            for (index, mapping) in mode.axes.iter().enumerate() {
                let location = format!("mode {name:?} axis mapping {index}");
                let scalar_source = matches!(
                    mapping.source,
                    AnalogSource::LeftTrigger | AnalogSource::RightTrigger
                );
                let scalar_target = matches!(
                    mapping.target,
                    AnalogTarget::GamepadLeftTrigger | AnalogTarget::GamepadRightTrigger
                );

                if scalar_source != scalar_target {
                    errors.push(format!(
                        "{location} must connect a scalar source to a trigger or a vector source to a stick, mouse, or scroll target"
                    ));
                }
                if mapping.components.is_some() && !matches!(mapping.source, AnalogSource::Gyro) {
                    errors.push(format!(
                        "{location} components is only valid for a gyro source"
                    ));
                }
                if let Some(deadzone) = mapping.deadzone {
                    if !deadzone.is_finite() || !(0.0..1.0).contains(&deadzone) {
                        errors.push(format!("{location} deadzone must be finite and in [0, 1)"));
                    }
                }
                if let Some(sensitivity) = mapping.sensitivity {
                    if !sensitivity.is_finite() || sensitivity < 0.0 {
                        errors.push(format!(
                            "{location} sensitivity must be finite and non-negative"
                        ));
                    }
                }
                if let Some(acceleration) = mapping.acceleration {
                    if !acceleration.is_finite() || acceleration < 0.0 {
                        errors.push(format!(
                            "{location} acceleration must be finite and non-negative"
                        ));
                    }
                    if !matches!(
                        mapping.source,
                        AnalogSource::LeftPad | AnalogSource::RightPad
                    ) || mapping.target != AnalogTarget::MouseMotion
                    {
                        errors.push(format!(
                            "{location} acceleration is only valid for a trackpad source and mouse-motion target"
                        ));
                    }
                }
                if let Some(exponent) = mapping.exponent {
                    if !exponent.is_finite() || exponent <= 0.0 {
                        errors.push(format!(
                            "{location} exponent must be finite and greater than zero"
                        ));
                    }
                }
                if mode.axes[..index]
                    .iter()
                    .any(|earlier| earlier.target == mapping.target)
                {
                    errors.push(format!(
                        "{location} duplicates target {:?}; each mode may map a target once",
                        mapping.target
                    ));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(errors))
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Global {
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    #[serde(default)]
    pub bindings: Vec<Binding>,
    #[serde(default)]
    pub axes: Vec<AxisMapping>,
    #[serde(default)]
    pub layers: IndexMap<String, Layer>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub hold: DigitalInput,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    #[serde(default)]
    pub input: Option<DigitalInput>,
    #[serde(default)]
    pub chord: Option<Vec<DigitalInput>>,
    #[serde(default)]
    pub activation: Activation,
    #[serde(default)]
    pub consume: bool,
    pub action: Action,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum DigitalInput {
    Button(Button),
    Axis(AxisThreshold),
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AxisThreshold {
    pub axis: Axis,
    #[serde(default)]
    pub direction: Direction,
    pub threshold: f32,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
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
    Event {
        name: String,
    },
}

fn deserialize_key<'de, D>(deserializer: D) -> Result<KeyCode, D::Error>
where
    D: Deserializer<'de>,
{
    let name = String::deserialize(deserializer)?;
    let name = name.trim();
    let key = match name {
        "enter" | "return" => KeyCode::KEY_ENTER,
        "escape" | "esc" => KeyCode::KEY_ESC,
        "space" => KeyCode::KEY_SPACE,
        "tab" => KeyCode::KEY_TAB,
        "super" | "command" | "cmd" | "meta" => KeyCode::KEY_LEFTMETA,
        "control" | "ctrl" => KeyCode::KEY_LEFTCTRL,
        "shift" => KeyCode::KEY_LEFTSHIFT,
        "alt" => KeyCode::KEY_LEFTALT,
        "up" => KeyCode::KEY_UP,
        "down" => KeyCode::KEY_DOWN,
        "left" => KeyCode::KEY_LEFT,
        "right" => KeyCode::KEY_RIGHT,
        _ => {
            let prefixed = name.get(..4).is_some_and(|prefix| {
                prefix.eq_ignore_ascii_case("key_") || prefix.eq_ignore_ascii_case("btn_")
            });
            let mut code = String::with_capacity(name.len() + 4);
            if !prefixed {
                code.push_str("KEY_");
            }
            for character in name.chars() {
                if prefixed || !matches!(character, '-' | '_') {
                    code.extend(character.to_uppercase());
                }
            }
            KeyCode::from_str(&code)
                .map_err(|_| serde::de::Error::custom(format!("unknown key {name:?}")))?
        }
    };
    if key == KeyCode::KEY_RESERVED
        || (0x100..=0x15f).contains(&key.code())
        || (0x2c0..=0x2ff).contains(&key.code())
    {
        return Err(serde::de::Error::custom(format!(
            "{name:?} is not a keyboard key"
        )));
    }
    Ok(key)
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AxisMapping {
    pub source: AnalogSource,
    pub target: AnalogTarget,
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Button {
    A,
    B,
    X,
    Y,
    #[serde(alias = "qam")]
    QuickAccess,
    #[serde(alias = "r3")]
    RightStickClick,
    View,
    R4,
    R5,
    #[serde(alias = "rb")]
    RightBumper,
    DpadDown,
    DpadRight,
    DpadLeft,
    DpadUp,
    Menu,
    #[serde(alias = "l3")]
    LeftStickClick,
    Steam,
    L4,
    L5,
    #[serde(alias = "lb")]
    LeftBumper,
    RightStickTouch,
    RightPadTouch,
    RightPadClick,
    RightTriggerClick,
    LeftStickTouch,
    LeftPadTouch,
    LeftPadClick,
    LeftTriggerClick,
    RightGripTouch,
    LeftGripTouch,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Axis {
    LeftStickX,
    LeftStickY,
    RightStickX,
    RightStickY,
    LeftTrigger,
    RightTrigger,
    LeftPadX,
    LeftPadY,
    RightPadX,
    RightPadY,
    GyroX,
    GyroY,
    GyroZ,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    #[default]
    Positive,
    Negative,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum AnalogTarget {
    GamepadLeftStick,
    GamepadRightStick,
    GamepadLeftTrigger,
    GamepadRightTrigger,
    MouseMotion,
    Scroll,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Curve {
    #[default]
    Linear,
    Exponential,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AxisComponent {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Activation {
    #[default]
    Press,
    Release,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Side,
    Extra,
    Forward,
    Back,
    Task,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum GamepadButton {
    South,
    East,
    North,
    West,
    LeftBumper,
    RightBumper,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    Guide,
    LeftStick,
    RightStick,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    PaddleLeftUpper,
    PaddleLeftLower,
    PaddleRightUpper,
    PaddleRightLower,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Invalid(Vec<String>),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "failed to read configuration: {error}"),
            Self::Parse(error) => write!(formatter, "failed to parse configuration: {error}"),
            Self::Invalid(errors) => {
                write!(formatter, "invalid configuration: {}", errors.join("; "))
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Parse(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arbitrary_modes_and_global_chord() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "couch browsing"

                [[global.bindings]]
                chord = ["steam", "x"]
                activation = "press"
                consume = true
                action = { type = "event", name = "keyboard.toggle" }

                [modes."couch browsing"]
                [[modes."couch browsing".bindings]]
                input = "a"
                action = { type = "key", key = "enter" }

                [[modes."couch browsing".axes]]
                source = "right-pad"
                target = "mouse-motion"
                sensitivity = 1.5
                acceleration = 5.0

                [modes."anything at all"]
            "#,
        )
        .unwrap();

        assert_eq!(config.default_mode, "couch browsing");
        assert_eq!(
            config.modes.keys().map(String::as_str).collect::<Vec<_>>(),
            ["couch browsing", "anything at all"]
        );
        assert!(config.global.bindings[0].consume);
        assert_eq!(
            config.modes["couch browsing"].axes[0].acceleration,
            Some(5.0)
        );
    }

    #[test]
    fn parses_ordered_named_layers_and_friendly_keys() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "l4"
                action = { type = "key", key = "super" }

                [modes.desktop.layers.apps]
                hold = "left-bumper"
                [[modes.desktop.layers.apps.bindings]]
                input = "b"
                action = { type = "key", key = "q" }

                [modes.desktop.layers.future]
                hold = "right-bumper"
            "#,
        )
        .unwrap();

        assert_eq!(
            config.modes["desktop"]
                .layers
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["apps", "future"]
        );
        assert_eq!(
            config.modes["desktop"].layers["apps"].bindings[0].action,
            Action::Key {
                key: KeyCode::KEY_Q
            }
        );
        assert_eq!(
            config.modes["desktop"].bindings[0].action,
            Action::Key {
                key: KeyCode::KEY_LEFTMETA
            }
        );
    }

    #[test]
    fn rejects_conflicting_layer_holds_and_own_hold_bindings() {
        let error = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [modes.desktop.layers.first]
                hold = "left-bumper"
                [[modes.desktop.layers.first.bindings]]
                input = "left-bumper"
                action = { type = "key", key = "q" }

                [modes.desktop.layers.second]
                hold = "left-bumper"
            "#,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("must not use its own hold input"));
        assert!(error.contains("hold conflicts with an earlier layer hold"));
    }

    #[test]
    fn parses_axis_threshold_inputs() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"

                [modes.one]
                [[modes.one.bindings]]
                input = { axis = "right-trigger", threshold = 0.75 }
                action = { type = "gamepad", button = "south" }
            "#,
        )
        .unwrap();

        assert!(matches!(
            config.modes["one"].bindings[0].input,
            Some(DigitalInput::Axis(AxisThreshold {
                axis: Axis::RightTrigger,
                direction: Direction::Positive,
                threshold: 0.75,
            }))
        ));
    }

    #[test]
    fn rejects_bad_references_and_ranges_together() {
        let error = Config::parse(
            r#"
                version = 2
                default_mode = "missing"

                [modes.one]
                [[modes.one.bindings]]
                input = { axis = "right-trigger", threshold = 1.5 }
                action = { type = "mode-set", name = "also-missing" }
            "#,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("version must be 1"));
        assert!(error.contains("default_mode \"missing\""));
        assert!(error.contains("axis threshold"));
        assert!(error.contains("mode-set target \"also-missing\""));
    }

    #[test]
    fn rejects_non_keyboard_key_codes() {
        for code in ["KEY_RESERVED", "BTN_LEFT", "BTN_TRIGGER_HAPPY1"] {
            let source = format!(
                r#"
                    version = 1
                    default_mode = "one"
                    [modes.one]
                    [[modes.one.bindings]]
                    input = "a"
                    action = {{ type = "key", key = "{code}" }}
                "#
            );
            assert!(
                Config::parse(&source)
                    .unwrap_err()
                    .to_string()
                    .contains("is not a keyboard key")
            );
        }
    }

    #[test]
    fn rejects_invalid_or_misapplied_acceleration() {
        for (source, target, acceleration, expected) in [
            (
                "right-pad",
                "mouse-motion",
                "-1.0",
                "acceleration must be finite and non-negative",
            ),
            (
                "right-pad",
                "mouse-motion",
                "nan",
                "acceleration must be finite and non-negative",
            ),
            (
                "right-stick",
                "mouse-motion",
                "1.0",
                "acceleration is only valid for a trackpad source and mouse-motion target",
            ),
            (
                "left-pad",
                "scroll",
                "1.0",
                "acceleration is only valid for a trackpad source and mouse-motion target",
            ),
        ] {
            let source = format!(
                r#"
                    version = 1
                    default_mode = "one"
                    [modes.one]
                    [[modes.one.axes]]
                    source = "{source}"
                    target = "{target}"
                    acceleration = {acceleration}
                "#
            );
            assert!(
                Config::parse(&source)
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
    }
}
