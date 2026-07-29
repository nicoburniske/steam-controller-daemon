mod raw;

use std::{error, fmt, str::FromStr};

use evdev::KeyCode;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};

use crate::protocol::{Button, Buttons, Haptic, Trackpad};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub default_mode: ModeId,
    pub mode_switch_haptic: Option<Haptic>,
    pub click_pressure: u16,
    pub osk_bindings: IndexMap<Button, KeyCode>,
    pub global_bindings: Vec<GlobalBinding>,
    mode_names: IndexMap<String, ModeId>,
    pub modes: Vec<Mode>,
}

impl Config {
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let raw: raw::Config =
            toml::from_str(source).map_err(|error| ConfigError(vec![error.to_string()]))?;
        Self::try_from(raw)
    }

    pub fn mode_id(&self, name: &str) -> Option<ModeId> {
        self.mode_names.get(name).copied()
    }

    pub fn mode_name(&self, mode: ModeId) -> &str {
        self.mode_names
            .get_index(mode.0)
            .expect("ModeId belongs to this Config")
            .0
    }

    pub fn mode(&self, mode: ModeId) -> &Mode {
        &self.modes[mode.0]
    }

    pub fn next_mode(&self, mode: ModeId) -> ModeId {
        ModeId((mode.0 + 1) % self.modes.len())
    }
}

impl TryFrom<raw::Config> for Config {
    type Error = ConfigError;

    fn try_from(config: raw::Config) -> Result<Self, Self::Error> {
        let raw::Config {
            version,
            default_mode,
            mode_switch_haptic,
            trackpad,
            osk,
            global,
            mode: raw_modes,
        } = config;
        let mut errors = Vec::new();

        if version != 1 {
            errors.push(format!("version must be 1, got {version}"));
        }
        if raw_modes.is_empty() {
            errors.push("at least one mode is required".into());
        }
        if default_mode.trim().is_empty() {
            errors.push("default_mode must not be empty".into());
        }
        if trackpad.click_pressure > 100 {
            errors.push("trackpad.click_pressure must be in [0, 100]".into());
        }

        let mut mode_names = IndexMap::with_capacity(raw_modes.len());
        for (index, name) in raw_modes.keys().enumerate() {
            if name.trim().is_empty() {
                errors.push("mode names must not be empty".into());
            }
            mode_names.insert(name.clone(), ModeId(index));
        }
        let resolved_default_mode = mode_names.get(&default_mode).copied();
        if resolved_default_mode.is_none() {
            errors.push(format!(
                "default_mode {default_mode:?} does not name a configured mode"
            ));
        }

        for input in osk.bind.keys() {
            if matches!(
                input,
                Button::LeftPadTouch
                    | Button::LeftPadClick
                    | Button::RightPadTouch
                    | Button::RightPadClick
            ) {
                errors.push(format!(
                    "OSK binding input {input:?} is reserved for keyboard pointing"
                ));
            }
        }

        validate_binding_inputs(&global.bind, format_args!("global"), &mut errors);

        let osk_bindings = osk
            .bind
            .into_iter()
            .map(|(button, key)| (button, key.0))
            .collect();
        let global_bindings = global
            .bind
            .into_iter()
            .filter_map(|binding| {
                let action = convert_global_action(binding.action, &mode_names, &mut errors)?;
                let (inputs, trigger) = binding.input.into_parts()?;
                Some(GlobalBinding {
                    inputs,
                    trigger,
                    action,
                })
            })
            .collect();

        let mut modes = Vec::with_capacity(raw_modes.len());
        for (name, mode) in raw_modes {
            validate_binding_inputs(&mode.bind, format_args!("mode {name:?}"), &mut errors);
            let mut mode_inputs = Buttons::default();
            for binding in &mode.bind {
                for input in binding.input.buttons() {
                    mode_inputs.insert(*input);
                }
            }

            let mut layer_holds = Buttons::default();
            for (layer_name, layer) in &mode.layer {
                if layer_name.trim().is_empty() {
                    errors.push(format!("mode {name:?} layer names must not be empty"));
                }
                if layer_holds.contains(layer.hold) {
                    errors.push(format!(
                        "mode {name:?} layer {layer_name:?} hold conflicts with an earlier layer"
                    ));
                }
                layer_holds.insert(layer.hold);
                if mode_inputs.contains(layer.hold) {
                    errors.push(format!(
                        "mode {name:?} layer {layer_name:?} hold must not also be a mode binding"
                    ));
                }

                validate_binding_inputs(
                    &layer.bind,
                    format_args!("mode {name:?} layer {layer_name:?}"),
                    &mut errors,
                );
                for (binding_index, binding) in layer.bind.iter().enumerate() {
                    if binding.input.buttons().contains(&layer.hold) {
                        errors.push(format!(
                            "mode {name:?} layer {layer_name:?} binding {binding_index} must not use its hold button"
                        ));
                    }
                }
            }

            for (index, mapping) in mode.axis.iter().enumerate() {
                if mode.axis[..index]
                    .iter()
                    .any(|earlier| earlier.target == mapping.target)
                    && !matches!(
                        mapping.target,
                        raw::AnalogTarget::GamepadLeftStick
                            | raw::AnalogTarget::GamepadRightStick
                            | raw::AnalogTarget::MouseMotion
                    )
                {
                    errors.push(format!(
                        "mode {name:?} axis mapping {index} duplicates target {:?}",
                        mapping.target
                    ));
                }
            }

            let bindings = mode
                .bind
                .into_iter()
                .filter_map(|binding| {
                    let action = convert_action(binding.action, &mode_names, &mut errors)?;
                    let (inputs, trigger) = binding.input.into_parts()?;
                    Some(Binding {
                        inputs,
                        trigger,
                        action,
                    })
                })
                .collect();
            let layers = mode
                .layer
                .into_values()
                .map(|layer| Layer {
                    hold: layer.hold,
                    bindings: layer
                        .bind
                        .into_iter()
                        .filter_map(|binding| {
                            let action = convert_action(binding.action, &mode_names, &mut errors)?;
                            let (inputs, trigger) = binding.input.into_parts()?;
                            Some(Binding {
                                inputs,
                                trigger,
                                action,
                            })
                        })
                        .collect(),
                })
                .collect();
            let axes = mode
                .axis
                .into_iter()
                .enumerate()
                .filter_map(|(index, mapping)| {
                    let error_count = errors.len();
                    let scalar_source = matches!(
                        mapping.source,
                        raw::AnalogSource::LeftTrigger | raw::AnalogSource::RightTrigger
                    );
                    let scalar_target = matches!(
                        mapping.target,
                        raw::AnalogTarget::GamepadLeftTrigger
                            | raw::AnalogTarget::GamepadRightTrigger
                    );
                    if scalar_source != scalar_target {
                        errors.push(format!(
                            "mode {name:?} axis mapping {index} must connect a scalar source to a trigger or a vector source to a stick, mouse, or scroll target"
                        ));
                    }
                    if mapping.components.is_some()
                        && mapping.source != raw::AnalogSource::Gyro
                    {
                        errors.push(format!(
                            "mode {name:?} axis mapping {index} components is only valid for a gyro source"
                        ));
                    }

                    let activation = match mapping.activation {
                        Some(raw::AxisActivation::Trigger(activation)) => {
                            let source = match activation.source {
                                raw::AnalogSource::LeftTrigger => Some(Trigger::Left),
                                raw::AnalogSource::RightTrigger => Some(Trigger::Right),
                                _ => {
                                    errors.push(format!(
                                        "mode {name:?} axis mapping {index} activation source must be a trigger"
                                    ));
                                    None
                                }
                            };
                            if !activation.engage.is_finite()
                                || !activation.release.is_finite()
                                || activation.release < 0.0
                                || activation.release >= activation.engage
                                || activation.engage > 1.0
                            {
                                errors.push(format!(
                                    "mode {name:?} axis mapping {index} activation thresholds must satisfy 0 <= release < engage <= 1"
                                ));
                            }
                            source.map(|source| AxisActivation::Trigger {
                                source,
                                engage: activation.engage,
                                release: activation.release,
                            })
                        }
                        Some(raw::AxisActivation::All { all }) => {
                            if all.is_empty() {
                                errors.push(format!(
                                    "mode {name:?} axis mapping {index} activation must contain at least one button"
                                ));
                            }
                            let mut buttons = Buttons::default();
                            for button in all {
                                buttons.insert(button);
                            }
                            Some(AxisActivation::Buttons(buttons))
                        }
                        None => None,
                    };

                    if let Some(deadzone) = mapping.deadzone
                        && (!deadzone.is_finite() || !(0.0..1.0).contains(&deadzone))
                    {
                        errors.push(format!(
                            "mode {name:?} axis mapping {index} deadzone must be finite and in [0, 1)"
                        ));
                    }
                    if let Some(sensitivity) = mapping.sensitivity
                        && (!sensitivity.is_finite() || sensitivity < 0.0)
                    {
                        errors.push(format!(
                            "mode {name:?} axis mapping {index} sensitivity must be finite and non-negative"
                        ));
                    }
                    if let Some(acceleration) = mapping.acceleration {
                        if !acceleration.is_finite() || acceleration < 0.0 {
                            errors.push(format!(
                                "mode {name:?} axis mapping {index} acceleration must be finite and non-negative"
                            ));
                        }
                        if !matches!(
                            mapping.source,
                            raw::AnalogSource::LeftPad | raw::AnalogSource::RightPad
                        ) || mapping.target != raw::AnalogTarget::MouseMotion
                        {
                            errors.push(format!(
                                "mode {name:?} axis mapping {index} acceleration is only valid for a trackpad source and mouse-motion target"
                            ));
                        }
                    }
                    if let Some(exponent) = mapping.exponent
                        && (!exponent.is_finite() || exponent <= 0.0)
                    {
                        errors.push(format!(
                            "mode {name:?} axis mapping {index} exponent must be finite and greater than zero"
                        ));
                    }
                    if errors.len() != error_count {
                        return None;
                    }

                    let options = AxisOptions {
                        activation,
                        deadzone: mapping.deadzone.unwrap_or(0.0),
                        sensitivity: mapping.sensitivity.unwrap_or(1.0),
                        exponent: match mapping.curve {
                            raw::Curve::Linear => 1.0,
                            raw::Curve::Exponential => mapping.exponent.unwrap_or(2.0),
                        },
                    };
                    if scalar_source {
                        return Some(AxisMapping::Trigger {
                            source: match mapping.source {
                                raw::AnalogSource::LeftTrigger => Trigger::Left,
                                raw::AnalogSource::RightTrigger => Trigger::Right,
                                _ => return None,
                            },
                            target: match mapping.target {
                                raw::AnalogTarget::GamepadLeftTrigger => Trigger::Left,
                                raw::AnalogTarget::GamepadRightTrigger => Trigger::Right,
                                _ => return None,
                            },
                            options,
                        });
                    }

                    let vector_options = VectorOptions {
                        axis: options,
                        invert_x: mapping.invert_x,
                        invert_y: mapping.invert_y,
                        swap_xy: mapping.swap_xy,
                    };
                    let target = match mapping.target {
                        raw::AnalogTarget::GamepadLeftStick => VectorTarget::GamepadLeftStick,
                        raw::AnalogTarget::GamepadRightStick => VectorTarget::GamepadRightStick,
                        raw::AnalogTarget::MouseMotion => VectorTarget::MouseMotion,
                        raw::AnalogTarget::Scroll => VectorTarget::Scroll,
                        raw::AnalogTarget::GamepadLeftTrigger
                        | raw::AnalogTarget::GamepadRightTrigger => return None,
                    };
                    match mapping.source {
                        source @ (raw::AnalogSource::LeftStick | raw::AnalogSource::RightStick) => {
                            Some(AxisMapping::Stick {
                                source: if source == raw::AnalogSource::LeftStick {
                                    Stick::Left
                                } else {
                                    Stick::Right
                                },
                                target,
                                options: vector_options,
                            })
                        }
                        source @ (raw::AnalogSource::LeftPad | raw::AnalogSource::RightPad) => {
                            let pad = if source == raw::AnalogSource::LeftPad {
                                Trackpad::Left
                            } else {
                                Trackpad::Right
                            };
                            match target {
                                VectorTarget::GamepadLeftStick
                                | VectorTarget::GamepadRightStick => {
                                    Some(AxisMapping::PadPosition {
                                        pad,
                                        target: if target == VectorTarget::GamepadLeftStick {
                                            Stick::Left
                                        } else {
                                            Stick::Right
                                        },
                                        options: vector_options,
                                    })
                                }
                                VectorTarget::MouseMotion => Some(AxisMapping::PadMotion {
                                    pad,
                                    options: vector_options,
                                    acceleration: mapping.acceleration.unwrap_or(0.0),
                                }),
                                VectorTarget::Scroll => Some(AxisMapping::CircularScroll {
                                    pad,
                                    options: vector_options,
                                }),
                            }
                        }
                        raw::AnalogSource::Gyro => {
                            let components = mapping
                                .components
                                .unwrap_or([AxisComponent::Y, AxisComponent::X]);
                            Some(AxisMapping::Gyro {
                                components,
                                target,
                                options: vector_options,
                            })
                        }
                        _ => None,
                    }
                })
                .collect();
            modes.push(Mode {
                gamepad: mode.gamepad,
                gamepad_touchpad: mode.gamepad_touchpad,
                bindings,
                axes,
                layers,
            });
        }

        if !errors.is_empty() {
            return Err(ConfigError(errors));
        }

        Ok(Self {
            default_mode: resolved_default_mode.expect("validated default mode exists"),
            mode_switch_haptic,
            click_pressure: trackpad.click_pressure,
            osk_bindings,
            global_bindings,
            mode_names,
            modes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub Vec<String>);

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid configuration")?;
        for error in &self.0 {
            write!(formatter, "\n- {error}")?;
        }
        Ok(())
    }
}

impl error::Error for ConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModeId(usize);

#[derive(Debug, Clone, PartialEq)]
pub struct Mode {
    pub gamepad: Gamepad,
    pub gamepad_touchpad: Option<Trackpad>,
    pub bindings: Vec<Binding>,
    pub axes: Vec<AxisMapping>,
    pub layers: Vec<Layer>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub hold: Button,
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub inputs: Buttons,
    pub trigger: Button,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalBinding {
    pub inputs: Buttons,
    pub trigger: Button,
    pub action: GlobalAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Key(KeyCode),
    Mouse(MouseButton),
    Gamepad(GamepadButton),
    ModeSet(ModeId),
    ModeNext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalAction {
    Action(Action),
    KeyboardToggle,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Gamepad {
    #[default]
    None,
    Xbox,
    #[serde(rename = "dualshock4")]
    DualShock4,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AxisMapping {
    Trigger {
        source: Trigger,
        target: Trigger,
        options: AxisOptions,
    },
    Stick {
        source: Stick,
        target: VectorTarget,
        options: VectorOptions,
    },
    PadPosition {
        pad: Trackpad,
        target: Stick,
        options: VectorOptions,
    },
    PadMotion {
        pad: Trackpad,
        options: VectorOptions,
        acceleration: f32,
    },
    CircularScroll {
        pad: Trackpad,
        options: VectorOptions,
    },
    Gyro {
        components: [AxisComponent; 2],
        target: VectorTarget,
        options: VectorOptions,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisOptions {
    pub activation: Option<AxisActivation>,
    pub deadzone: f32,
    pub sensitivity: f32,
    pub exponent: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorOptions {
    pub axis: AxisOptions,
    pub invert_x: bool,
    pub invert_y: bool,
    pub swap_xy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AxisActivation {
    Trigger {
        source: Trigger,
        engage: f32,
        release: f32,
    },
    Buttons(Buttons),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stick {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorTarget {
    GamepadLeftStick,
    GamepadRightStick,
    MouseMotion,
    Scroll,
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

fn validate_binding_inputs(
    bindings: &[raw::Binding],
    context: fmt::Arguments<'_>,
    errors: &mut Vec<String>,
) {
    for (index, binding) in bindings.iter().enumerate() {
        let buttons = binding.input.buttons();
        if matches!(binding.input, raw::BindingInput::Chord(_)) && buttons.len() < 2 {
            errors.push(format!(
                "{context} binding {index} list input must contain at least two buttons"
            ));
        }
        for (position, button) in buttons.iter().enumerate() {
            if buttons[..position].contains(button) {
                errors.push(format!(
                    "{context} binding {index} input contains duplicate button {button:?}"
                ));
            }
        }
        if bindings[..index].iter().any(|earlier| {
            let earlier = earlier.input.buttons();
            earlier.len() == buttons.len()
                && earlier.last() == buttons.last()
                && earlier[..earlier.len().saturating_sub(1)]
                    .iter()
                    .all(|button| buttons[..buttons.len().saturating_sub(1)].contains(button))
        }) {
            errors.push(format!(
                "{context} binding {index} duplicates an earlier input"
            ));
        }
    }
}

fn convert_global_action(
    action: raw::Action,
    mode_names: &IndexMap<String, ModeId>,
    errors: &mut Vec<String>,
) -> Option<GlobalAction> {
    let action = match action {
        raw::Action::Key { key } => Action::Key(key),
        raw::Action::Mouse { button } => Action::Mouse(button),
        raw::Action::Gamepad { button } => Action::Gamepad(button),
        raw::Action::ModeSet { name } => {
            let Some(mode) = mode_names.get(&name).copied() else {
                errors.push(format!("mode-set target {name:?} is not configured"));
                return None;
            };
            Action::ModeSet(mode)
        }
        raw::Action::ModeNext => Action::ModeNext,
        raw::Action::KeyboardToggle => return Some(GlobalAction::KeyboardToggle),
    };
    Some(GlobalAction::Action(action))
}

fn convert_action(
    action: raw::Action,
    mode_names: &IndexMap<String, ModeId>,
    errors: &mut Vec<String>,
) -> Option<Action> {
    match convert_global_action(action, mode_names, errors)? {
        GlobalAction::Action(action) => Some(action),
        GlobalAction::KeyboardToggle => {
            errors.push("keyboard-toggle is only valid for global bindings".into());
            None
        }
    }
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
    if !is_keyboard_key(key) {
        return Err(serde::de::Error::custom(format!(
            "{name:?} is not a keyboard key"
        )));
    }
    Ok(key)
}

pub const fn is_keyboard_key(key: KeyCode) -> bool {
    let code = key.code();
    code != 0
        && code <= 0x2ff
        && !(code >= 0x100 && code <= 0x15f)
        && !(code >= 0x220 && code <= 0x22f)
        && !(code >= 0x2c0 && code <= 0x2ff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_into_semantic_configuration() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [osk.bind]
                l4 = "super"
                left-trigger-click = "shift"

                [global]
                bind = [
                    { input = ["steam", "x"], type = "keyboard-toggle" },
                ]

                [mode.desktop]
                gamepad = "none"
                bind = [
                    { input = "l4", type = "key", key = "super" },
                    { input = "x", type = "mode-set", name = "anything at all" },
                ]
                [[mode.desktop.axis]]
                source = "gyro"
                target = "mouse-motion"
                curve = "exponential"

                [mode.desktop.layer.apps]
                hold = "left-bumper"
                bind = [
                    { input = "b", type = "key", key = "q" },
                ]

                [mode."anything at all"]
                gamepad = "dualshock4"
                gamepad_touchpad = "right"
            "#,
        )
        .unwrap();

        let desktop = config.mode_id("desktop").unwrap();
        let other = config.mode_id("anything at all").unwrap();
        let mode = config.mode(desktop);
        assert_eq!(config.default_mode, desktop);
        assert_eq!(config.mode_name(other), "anything at all");
        assert_eq!(config.osk_bindings[&Button::L4], KeyCode::KEY_LEFTMETA);
        assert_eq!(
            config.osk_bindings[&Button::LeftTriggerClick],
            KeyCode::KEY_LEFTSHIFT
        );
        assert_eq!(
            config.global_bindings[0].inputs,
            Buttons::from_bits(Button::Steam.mask() | Button::X.mask())
        );
        assert_eq!(config.global_bindings[0].trigger, Button::X);
        assert_eq!(mode.gamepad, Gamepad::None);
        assert_eq!(mode.gamepad_touchpad, None);
        assert_eq!(mode.bindings[0].action, Action::Key(KeyCode::KEY_LEFTMETA));
        assert_eq!(mode.bindings[1].action, Action::ModeSet(other));
        assert_eq!(
            mode.layers[0].bindings[0].action,
            Action::Key(KeyCode::KEY_Q)
        );
        let AxisMapping::Gyro {
            components,
            options,
            ..
        } = mode.axes[0]
        else {
            panic!("expected gyro motion axis");
        };
        assert_eq!(components, [AxisComponent::Y, AxisComponent::X]);
        assert_eq!(options.axis.exponent, 2.0);
        assert_eq!(config.mode(other).gamepad, Gamepad::DualShock4);
        assert_eq!(config.mode(other).gamepad_touchpad, Some(Trackpad::Right));
    }

    #[test]
    fn parses_axis_operations() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [mode.one]
                [[mode.one.axis]]
                source = "left-pad"
                target = "gamepad-left-stick"
                [[mode.one.axis]]
                source = "right-pad"
                target = "mouse-motion"
                [[mode.one.axis]]
                source = "left-pad"
                target = "scroll"
                [[mode.one.axis]]
                source = "gyro"
                target = "gamepad-right-stick"
                [[mode.one.axis]]
                source = "gyro"
                target = "mouse-motion"
            "#,
        )
        .unwrap();
        assert!(matches!(
            config.modes[0].axes.as_slice(),
            [
                AxisMapping::PadPosition {
                    pad: Trackpad::Left,
                    ..
                },
                AxisMapping::PadMotion {
                    pad: Trackpad::Right,
                    ..
                },
                AxisMapping::CircularScroll {
                    pad: Trackpad::Left,
                    ..
                },
                AxisMapping::Gyro {
                    components: [AxisComponent::Y, AxisComponent::X],
                    target: VectorTarget::GamepadRightStick,
                    ..
                },
                AxisMapping::Gyro {
                    components: [AxisComponent::Y, AxisComponent::X],
                    target: VectorTarget::MouseMotion,
                    ..
                },
            ]
        ));
    }

    #[test]
    fn reports_all_validation_errors() {
        let error = Config::parse(
            r#"
                version = 2
                default_mode = "missing"

                [trackpad]
                click_pressure = 101

                [osk.bind]
                left-pad-click = "enter"

                [global]
                bind = [
                    { input = [], type = "mode-set", name = "also missing" },
                ]

                [mode.one]
                bind = [
                    { input = "a", type = "keyboard-toggle" },
                ]
            "#,
        )
        .unwrap_err();

        assert_eq!(error.0.len(), 7);
        let report = error.to_string();
        for expected in [
            "version must be 1",
            "does not name a configured mode",
            "click_pressure must be in [0, 100]",
            "reserved for keyboard pointing",
            "list input must contain at least two buttons",
            "mode-set target \"also missing\" is not configured",
            "keyboard-toggle is only valid for global bindings",
        ] {
            assert!(
                report.contains(expected),
                "missing {expected:?} in {report}"
            );
        }
    }

    #[test]
    fn rejects_invalid_digital_configuration() {
        for (body, expected) in [
            (
                r#"
                    [global]
                    bind = [
                        { input = ["steam", "steam"], type = "mode-next" },
                    ]
                    [mode.one]
                "#,
                "duplicate button",
            ),
            (
                r#"
                    [global]
                    bind = [
                        { input = ["steam"], type = "mode-next" },
                    ]
                    [mode.one]
                "#,
                "list input must contain at least two buttons",
            ),
            (
                r#"
                    [osk.bind]
                    left-pad-click = "enter"
                    [mode.one]
                "#,
                "reserved for keyboard pointing",
            ),
            (
                r#"
                    [mode.one]
                    bind = [
                        { input = "left-bumper", type = "key", key = "q" },
                    ]
                    [mode.one.layer.apps]
                    hold = "left-bumper"
                "#,
                "must not also be a mode binding",
            ),
            (
                r#"
                    [mode.one]
                    bind = [
                        { input = "a", type = "mode-set", name = "missing" },
                    ]
                "#,
                "mode-set target",
            ),
            (
                r#"
                    [mode.one]
                    bind = [
                        { input = "a", type = "keyboard-toggle" },
                    ]
                "#,
                "only valid for global bindings",
            ),
            (
                r#"
                    [mode.one]
                    bind = [
                        { input = "a", type = "key", key = "q" },
                        { input = "a", type = "key", key = "w" },
                    ]
                "#,
                "duplicates an earlier input",
            ),
            (
                r#"
                    [mode.one]
                    bind = [
                        { input = ["steam", "left-bumper", "x"], type = "key", key = "q" },
                        { input = ["left-bumper", "steam", "x"], type = "key", key = "w" },
                    ]
                "#,
                "duplicates an earlier input",
            ),
            (
                r#"
                    [mode.one]
                    bind = [
                        { input = "a", type = "key", key = "q", typo = true },
                    ]
                "#,
                "unknown field",
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
            (
                r#"source = "gyro"
                   target = "gamepad-right-stick"
                   activation = { source = "left-trigger", engage = 0.1, release = 0.2 }"#,
                "activation thresholds",
            ),
        ] {
            let source = format!(
                "version = 1\ndefault_mode = \"one\"\n[mode.one]\n[[mode.one.axis]]\n{mapping}"
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
        for code in [
            "KEY_RESERVED",
            "BTN_LEFT",
            "BTN_DPAD_UP",
            "BTN_TRIGGER_HAPPY1",
        ] {
            let source = format!(
                r#"
                    version = 1
                    default_mode = "one"
                    [mode.one]
                    bind = [
                        {{ input = "a", type = "key", key = "{code}" }},
                    ]
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
