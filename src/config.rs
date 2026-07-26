use std::{fs, path::Path, str::FromStr};

use evdev::KeyCode;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};

use crate::{Error, Result as ScdResult, ResultExt, protocol::Button};

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
    pub fn load(path: impl AsRef<Path>) -> ScdResult<Self> {
        Self::parse(&fs::read_to_string(path).whence()?)
    }

    pub fn parse(source: &str) -> ScdResult<Self> {
        let config: Self = toml::from_str(source).whence()?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> ScdResult<()> {
        if self.version != 1 {
            return Err(Error::message(format!(
                "invalid configuration: version must be 1, got {}",
                self.version
            )));
        }
        if self.modes.is_empty() {
            return Err(Error::message(
                "invalid configuration: at least one mode is required",
            ));
        }
        if self.default_mode.trim().is_empty() {
            return Err(Error::message(
                "invalid configuration: default_mode must not be empty",
            ));
        }
        if !self.modes.contains_key(&self.default_mode) {
            return Err(Error::message(format!(
                "invalid configuration: default_mode {:?} does not name a configured mode",
                self.default_mode
            )));
        }

        for (index, binding) in self.global.bindings.iter().enumerate() {
            if binding.chord.is_empty() {
                return Err(Error::message(format!(
                    "invalid configuration: global binding {index} chord must not be empty"
                )));
            }
            for (position, button) in binding.chord.iter().enumerate() {
                if binding.chord[..position].contains(button) {
                    return Err(Error::message(format!(
                        "invalid configuration: global binding {index} chord contains duplicate button {button:?}"
                    )));
                }
            }
            if self.global.bindings[..index]
                .iter()
                .any(|earlier| earlier.chord == binding.chord)
            {
                return Err(Error::message(format!(
                    "invalid configuration: global binding {index} duplicates an earlier chord"
                )));
            }
            self.validate_action(&binding.action)?;
        }

        for (name, mode) in &self.modes {
            if name.trim().is_empty() {
                return Err(Error::message(
                    "invalid configuration: mode names must not be empty",
                ));
            }

            for (index, binding) in mode.bindings.iter().enumerate() {
                if mode.bindings[..index]
                    .iter()
                    .any(|earlier| earlier.input == binding.input)
                {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} binding {index} duplicates input {:?}",
                        binding.input
                    )));
                }
                self.validate_action(&binding.action)?;
            }

            for (layer_index, (layer_name, layer)) in mode.layers.iter().enumerate() {
                if layer_name.trim().is_empty() {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} layer names must not be empty"
                    )));
                }
                if mode
                    .layers
                    .values()
                    .take(layer_index)
                    .any(|earlier| earlier.hold == layer.hold)
                {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} layer {layer_name:?} hold conflicts with an earlier layer"
                    )));
                }
                if mode
                    .bindings
                    .iter()
                    .any(|binding| binding.input == layer.hold)
                {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} layer {layer_name:?} hold must not also be a mode binding"
                    )));
                }
                for (binding_index, binding) in layer.bindings.iter().enumerate() {
                    if binding.input == layer.hold {
                        return Err(Error::message(format!(
                            "invalid configuration: mode {name:?} layer {layer_name:?} binding {binding_index} must not use its hold button"
                        )));
                    }
                    if layer.bindings[..binding_index]
                        .iter()
                        .any(|earlier| earlier.input == binding.input)
                    {
                        return Err(Error::message(format!(
                            "invalid configuration: mode {name:?} layer {layer_name:?} binding {binding_index} duplicates input {:?}",
                            binding.input
                        )));
                    }
                    self.validate_action(&binding.action)?;
                }
            }

            for (index, mapping) in mode.axes.iter().enumerate() {
                let scalar_source = matches!(
                    mapping.source,
                    AnalogSource::LeftTrigger | AnalogSource::RightTrigger
                );
                let scalar_target = matches!(
                    mapping.target,
                    AnalogTarget::GamepadLeftTrigger | AnalogTarget::GamepadRightTrigger
                );
                if scalar_source != scalar_target {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} axis mapping {index} must connect a scalar source to a trigger or a vector source to a stick, mouse, or scroll target"
                    )));
                }
                if mapping.components.is_some() && mapping.source != AnalogSource::Gyro {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} axis mapping {index} components is only valid for a gyro source"
                    )));
                }
                if let Some(deadzone) = mapping.deadzone
                    && (!deadzone.is_finite() || !(0.0..1.0).contains(&deadzone))
                {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} axis mapping {index} deadzone must be finite and in [0, 1)"
                    )));
                }
                if let Some(sensitivity) = mapping.sensitivity
                    && (!sensitivity.is_finite() || sensitivity < 0.0)
                {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} axis mapping {index} sensitivity must be finite and non-negative"
                    )));
                }
                if let Some(acceleration) = mapping.acceleration {
                    if !acceleration.is_finite() || acceleration < 0.0 {
                        return Err(Error::message(format!(
                            "invalid configuration: mode {name:?} axis mapping {index} acceleration must be finite and non-negative"
                        )));
                    }
                    if !matches!(
                        mapping.source,
                        AnalogSource::LeftPad | AnalogSource::RightPad
                    ) || mapping.target != AnalogTarget::MouseMotion
                    {
                        return Err(Error::message(format!(
                            "invalid configuration: mode {name:?} axis mapping {index} acceleration is only valid for a trackpad source and mouse-motion target"
                        )));
                    }
                }
                if let Some(exponent) = mapping.exponent
                    && (!exponent.is_finite() || exponent <= 0.0)
                {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} axis mapping {index} exponent must be finite and greater than zero"
                    )));
                }
                if mode.axes[..index]
                    .iter()
                    .any(|earlier| earlier.target == mapping.target)
                {
                    return Err(Error::message(format!(
                        "invalid configuration: mode {name:?} axis mapping {index} duplicates target {:?}",
                        mapping.target
                    )));
                }
            }
        }

        Ok(())
    }

    fn validate_action(&self, action: &Action) -> ScdResult<()> {
        match action {
            Action::ModeSet { name } if !self.modes.contains_key(name) => Err(Error::message(
                format!("invalid configuration: mode-set target {name:?} is not configured"),
            )),
            Action::Event { name } if name.trim().is_empty() => Err(Error::message(
                "invalid configuration: event name must not be empty",
            )),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Global {
    #[serde(default)]
    pub bindings: Vec<GlobalBinding>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GlobalBinding {
    pub chord: Vec<Button>,
    pub action: Action,
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
    pub hold: Button,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub input: Button,
    pub action: Action,
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modes_layers_chords_and_friendly_keys() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [[global.bindings]]
                chord = ["steam", "x"]
                action = { type = "event", name = "keyboard.toggle" }

                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "l4"
                action = { type = "key", key = "super" }

                [modes.desktop.layers.apps]
                hold = "left-bumper"
                [[modes.desktop.layers.apps.bindings]]
                input = "b"
                action = { type = "key", key = "q" }

                [modes."anything at all"]
            "#,
        )
        .unwrap();

        assert_eq!(config.global.bindings[0].chord, [Button::Steam, Button::X]);
        assert_eq!(
            config.modes["desktop"].bindings[0].action,
            Action::Key {
                key: KeyCode::KEY_LEFTMETA
            }
        );
        assert_eq!(
            config.modes["desktop"].layers["apps"].bindings[0].action,
            Action::Key {
                key: KeyCode::KEY_Q
            }
        );
    }

    #[test]
    fn rejects_invalid_digital_configuration() {
        for (body, expected) in [
            (
                r#"
                    [[global.bindings]]
                    chord = ["steam", "steam"]
                    action = { type = "mode-next" }
                    [modes.one]
                "#,
                "duplicate button",
            ),
            (
                r#"
                    [modes.one]
                    [[modes.one.bindings]]
                    input = "left-bumper"
                    action = { type = "key", key = "q" }
                    [modes.one.layers.apps]
                    hold = "left-bumper"
                "#,
                "must not also be a mode binding",
            ),
            (
                r#"
                    [modes.one]
                    [[modes.one.bindings]]
                    input = "a"
                    action = { type = "mode-set", name = "missing" }
                "#,
                "mode-set target",
            ),
        ] {
            let source = format!("version = 1\ndefault_mode = \"one\"\n{body}");
            assert!(
                Config::parse(&source)
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
    }

    #[test]
    fn rejects_invalid_analog_configuration() {
        for (mapping, expected) in [
            (
                r#"source = "right-pad"
                   target = "mouse-motion"
                   acceleration = -1.0"#,
                "acceleration must be finite and non-negative",
            ),
            (
                r#"source = "right-stick"
                   target = "mouse-motion"
                   acceleration = 1.0"#,
                "acceleration is only valid",
            ),
            (
                r#"source = "right-trigger"
                   target = "mouse-motion""#,
                "scalar source",
            ),
        ] {
            let source = format!(
                "version = 1\ndefault_mode = \"one\"\n[modes.one]\n[[modes.one.axes]]\n{mapping}"
            );
            assert!(
                Config::parse(&source)
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
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
}
