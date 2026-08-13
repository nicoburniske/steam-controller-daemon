use std::{error, fmt, str::FromStr};

use evdev::KeyCode;
use indexmap::IndexMap;
use miette::{Diagnostic, LabeledSpan, NamedSource, SourceCode};

use crate::{
    kdl::{
        Issue as ConfigIssue, KdlDocument, KdlNode, Spanned, argument, arguments, children, issue,
        nodes, one_of, properties, props,
    },
    protocol::{Button, Buttons, Haptic, Trackpad},
};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub default_mode: ModeId,
    pub mode_switch_haptic: Option<Haptic>,
    pub click_pressure: u16,
    pub osk_bindings: IndexMap<Button, KeyCode>,
    pub global_bindings: Vec<GlobalBinding>,
    pub mode_names: IndexMap<String, ModeId>,
    pub modes: Vec<Mode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModeId(pub usize);

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Gamepad {
    #[default]
    None,
    Xbox,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisComponent {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Default)]
struct ConfigError {
    source: Option<NamedSource<String>>,
    issues: Vec<ConfigIssue>,
}

impl Config {
    pub fn parse(source: &str) -> miette::Result<Self> {
        let document = KdlDocument::parse(source)?;
        Self::from_document(&document).map_err(|mut error| {
            error.source = Some(NamedSource::new("config.kdl", source.to_owned()));
            miette::Report::new(error)
        })
    }

    fn from_document(document: &KdlDocument) -> Result<Self, ConfigError> {
        let (version, default_mode, mode_switch_haptic_node, trackpad, osk, global, mode_nodes) = nodes!(
            document,
            "version",
            "default-mode",
            optional "mode-switch-haptic",
            optional "trackpad",
            optional "osk",
            optional "global",
            many "mode",
        )?;
        let version: Spanned<u32> = version;
        let default_mode: Spanned<&str> = default_mode;
        let mode_switch_haptic = mode_switch_haptic_node.map(parse_haptic).transpose()?;
        let click_pressure = trackpad.map_or(Ok(25), |node| props!(node, "click-pressure" = 25))?;

        let mut error = ConfigError::default();
        if version.value != 1 {
            error.push(issue(
                &version,
                format!("version must be 1, got {}", version.value),
            ));
        }
        let mode_count = mode_nodes.clone().count();
        if mode_count == 0 {
            error.push(issue(document, "at least one mode is required"));
        }
        if default_mode.value.trim().is_empty() {
            error.push(issue(&default_mode, "default_mode must not be empty"));
        }
        if click_pressure > 100 {
            error.push(issue(
                trackpad.unwrap(),
                "trackpad.click_pressure must be in [0, 100]",
            ));
        }

        let mut mode_names = IndexMap::with_capacity(mode_count);
        for (index, node) in mode_nodes.clone().enumerate() {
            let name: &str = argument(node, 0)?;
            if name.trim().is_empty() {
                error.push(issue(node, "mode names must not be empty"));
            }
            if mode_names.contains_key(name) {
                error.push(issue(node, format!("duplicate mode name {name:?}")));
            } else {
                mode_names.insert(name.to_owned(), ModeId(index));
            }
        }
        let resolved_default_mode = mode_names.get(default_mode.value).copied();
        if resolved_default_mode.is_none() {
            error.push(issue(
                &default_mode,
                format!(
                    "default_mode {:?} does not name a configured mode",
                    default_mode.value
                ),
            ));
        }

        let mut osk_bindings = IndexMap::new();
        if let Some(node) = osk {
            properties(node, &[])?;
            for child in children(node) {
                if child.name().value() != "bind" {
                    return Err(issue(
                        child,
                        format!("expected \"bind\", got {:?}", child.name().value()),
                    )
                    .into());
                }
                properties(child, &[])?;
                let input = parse_button(argument(child, 0)?, child)?;
                let key = parse_key(argument(child, 1)?).map_err(|error| issue(child, error))?;
                if osk_bindings.insert(input, key).is_some() {
                    error.push(issue(
                        child,
                        format!("duplicate OSK binding input {input:?}"),
                    ));
                }
                if matches!(
                    input,
                    Button::LeftPadTouch
                        | Button::LeftPadClick
                        | Button::RightPadTouch
                        | Button::RightPadClick
                ) {
                    error.push(issue(
                        child,
                        format!("OSK binding input {input:?} is reserved for keyboard pointing"),
                    ));
                }
            }
        }

        let mut global_bindings = Vec::new();
        if let Some(node) = global {
            properties(node, &[])?;
            for (index, child) in children(node).iter().enumerate() {
                let Some((inputs, trigger, action)) =
                    error.append(parse_binding(child, &mode_names, true))
                else {
                    continue;
                };
                if global_bindings.iter().any(|binding: &GlobalBinding| {
                    binding.inputs == inputs && binding.trigger == trigger
                }) {
                    error.push(issue(
                        child,
                        format!("global binding {index} duplicates an earlier input"),
                    ));
                }
                global_bindings.push(GlobalBinding {
                    inputs,
                    trigger,
                    action,
                });
            }
        }

        let mut modes = Vec::with_capacity(mode_count);
        for node in mode_nodes {
            modes.push(parse_mode(node, &mode_names, &mut error)?);
        }

        if !error.issues.is_empty() {
            return Err(error);
        }

        Ok(Self {
            default_mode: resolved_default_mode.expect("default mode exists"),
            mode_switch_haptic,
            click_pressure,
            osk_bindings,
            global_bindings,
            mode_names,
            modes,
        })
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

fn parse_mode(
    node: &KdlNode,
    mode_names: &IndexMap<String, ModeId>,
    error: &mut ConfigError,
) -> Result<Mode, ConfigIssue> {
    let name: &str = argument(node, 0)?;
    let (gamepad, touchpad): (&str, Option<&str>) =
        props!(node, "gamepad" = "none", optional "touchpad")?;
    let gamepad = one_of!(node, gamepad, "gamepad", {
        "none" => Gamepad::None,
        "xbox" => Gamepad::Xbox,
        "dualshock4" => Gamepad::DualShock4,
    })?;
    let gamepad_touchpad = touchpad
        .map(|value| parse_trackpad(value, node))
        .transpose()?;
    let mut bindings = Vec::new();
    let mut axes = Vec::new();
    let mut layers = Vec::new();
    let mut layer_holds = Buttons::default();
    let mode_children = children(node);

    for (child_position, child) in mode_children.iter().enumerate() {
        one_of!(child, child.name().value(), "mode child", {
            "bind" => {
                let index = bindings.len();
                let Some((inputs, trigger, action)) =
                    error.append(parse_binding(child, mode_names, false))
                else {
                    continue;
                };
                if bindings.iter().any(|binding: &Binding| {
                    binding.inputs == inputs && binding.trigger == trigger
                }) {
                    error.push(issue(
                        child,
                        format!("mode {name:?} binding {index} duplicates an earlier input"),
                    ));
                }
                bindings.push(Binding {
                    inputs,
                    trigger,
                    action: match action {
                        GlobalAction::Action(action) => action,
                        GlobalAction::KeyboardToggle => unreachable!(),
                    },
                });
            },
            "axis" => {
                let index = axes.len();
                let Some(axis) = error.append(parse_axis(child)) else {
                    continue;
                };
                if axes
                    .iter()
                    .any(|earlier| duplicate_axis_target(*earlier, axis))
                {
                    error.push(issue(
                        child,
                        format!("mode {name:?} axis mapping {index} duplicates target"),
                    ));
                }
                axes.push(axis);
            },
            "layer" => {
                let layer_name: &str = argument(child, 0)?;
                let hold: &str = props!(child, "hold")?;
                let hold = parse_button(hold, child)?;
                if layer_name.trim().is_empty() {
                    error.push(issue(
                        child,
                        format!("mode {name:?} layer names must not be empty"),
                    ));
                }
                if mode_children[..child_position].iter().any(|earlier| {
                    earlier.name().value() == "layer"
                        && earlier.entry(0).and_then(|entry| entry.value().as_string())
                            == Some(layer_name)
                }) {
                    error.push(issue(
                        child,
                        format!("mode {name:?} layer {layer_name:?} duplicates an earlier layer"),
                    ));
                }
                if layer_holds.contains(hold) {
                    error.push(issue(
                        child,
                        format!(
                            "mode {name:?} layer {layer_name:?} hold conflicts with an earlier layer"
                        ),
                    ));
                }
                layer_holds.insert(hold);
                let mut layer_bindings = Vec::new();
                for (index, binding) in children(child).iter().enumerate() {
                    let Some((inputs, trigger, action)) =
                        error.append(parse_binding(binding, mode_names, false))
                    else {
                        continue;
                    };
                    if inputs.contains(hold) {
                        error.push(issue(
                            binding,
                            format!(
                                "mode {name:?} layer {layer_name:?} binding {index} must not use its hold button"
                            ),
                        ));
                    }
                    if layer_bindings.iter().any(|binding: &Binding| {
                        binding.inputs == inputs && binding.trigger == trigger
                    }) {
                        error.push(issue(
                            binding,
                            format!(
                                "mode {name:?} layer {layer_name:?} binding {index} duplicates an earlier input"
                            ),
                        ));
                    }
                    layer_bindings.push(Binding {
                        inputs,
                        trigger,
                        action: match action {
                            GlobalAction::Action(action) => action,
                            GlobalAction::KeyboardToggle => unreachable!(),
                        },
                    });
                }
                layers.push(Layer {
                    hold,
                    bindings: layer_bindings,
                });
            },
        })?;
    }
    for (layer_node, layer) in mode_children
        .iter()
        .filter(|node| node.name().value() == "layer")
        .zip(&layers)
    {
        if bindings
            .iter()
            .any(|binding| binding.inputs.contains(layer.hold))
        {
            let layer_name: &str = argument(layer_node, 0)?;
            error.push(issue(
                layer_node,
                format!("mode {name:?} layer {layer_name:?} hold must not also be a mode binding"),
            ));
        }
    }
    Ok(Mode {
        gamepad,
        gamepad_touchpad,
        bindings,
        axes,
        layers,
    })
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid configuration")
    }
}

impl error::Error for ConfigError {}

impl ConfigError {
    fn push(&mut self, issue: ConfigIssue) {
        self.issues.push(issue);
    }

    fn append<T>(&mut self, result: Result<T, ConfigIssue>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(issue) => {
                self.push(issue);
                None
            }
        }
    }
}

impl From<ConfigIssue> for ConfigError {
    fn from(issue: ConfigIssue) -> Self {
        Self {
            source: None,
            issues: vec![issue],
        }
    }
}

impl Diagnostic for ConfigError {
    fn source_code(&self) -> Option<&dyn SourceCode> {
        self.source.as_ref().map(|source| source as &dyn SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        Some(Box::new(self.issues.iter().map(|issue| {
            LabeledSpan::new_with_span(Some(issue.message.clone()), issue.span)
        })))
    }
}

fn parse_binding(
    node: &KdlNode,
    mode_names: &IndexMap<String, ModeId>,
    global: bool,
) -> Result<(Buttons, Button, GlobalAction), ConfigIssue> {
    if node.name().value() != "bind" {
        return Err(issue(
            node,
            format!("expected \"bind\", got {:?}", node.name().value()),
        ));
    }
    properties(node, &[])?;
    let mut inputs = Buttons::default();
    let mut trigger = None;
    for (index, _) in arguments(node).enumerate() {
        let button = parse_button(argument(node, index)?, node)?;
        if inputs.contains(button) {
            return Err(issue(
                node,
                format!("binding input contains duplicate button {button:?}"),
            ));
        }
        inputs.insert(button);
        trigger = Some(button);
    }
    let trigger = trigger.ok_or_else(|| issue(node, "binding must contain at least one button"))?;
    let children = children(node);
    if children.len() != 1 {
        return Err(issue(node, "binding must contain exactly one action"));
    }
    let action = &children[0];
    properties(action, &[])?;
    let action = one_of!(action, action.name().value(), "action", {
        "key" => {
            GlobalAction::Action(Action::Key(
                parse_key(argument(action, 0)?).map_err(|error| issue(action, error))?,
            ))
        },
        "mouse" => {
            GlobalAction::Action(Action::Mouse(parse_mouse_button(
                argument(action, 0)?,
                action,
            )?))
        },
        "gamepad" => {
            GlobalAction::Action(Action::Gamepad(parse_gamepad_button(
                argument(action, 0)?,
                action,
            )?))
        },
        "mode-set" => {
            let name: &str = argument(action, 0)?;
            let mode = mode_names.get(name).copied().ok_or_else(|| {
                issue(
                    action,
                    format!("mode-set target {name:?} is not configured"),
                )
            })?;
            GlobalAction::Action(Action::ModeSet(mode))
        },
        "mode-next" => GlobalAction::Action(Action::ModeNext),
        "keyboard-toggle" => {
            if !global {
                return Err(issue(
                    action,
                    "keyboard-toggle is only valid for global bindings",
                ));
            }
            GlobalAction::KeyboardToggle
        }
    })?;
    Ok((inputs, trigger, action))
}

fn parse_axis(node: &KdlNode) -> Result<AxisMapping, ConfigIssue> {
    let source: &str = argument(node, 0)?;
    let target: &str = argument(node, 1)?;
    let scalar_source = matches!(source, "left-trigger" | "right-trigger");
    let scalar_target = matches!(target, "gamepad-left-trigger" | "gamepad-right-trigger");
    if scalar_source != scalar_target {
        return Err(issue(
            node,
            "axis mapping must connect a scalar source to a trigger or a vector source to a stick, mouse, or scroll target",
        ));
    }
    let mut activation = None;
    let mut components = None;
    for child in children(node) {
        one_of!(child, child.name().value(), "axis child", {
                "trigger" => {
                    if activation.is_some() {
                        return Err(issue(
                            child,
                            "axis mapping must contain at most one activation",
                        ));
                    }
                    let trigger = one_of!(child, argument(child, 0)?, "trigger", {
                        "left-trigger" => Trigger::Left,
                        "right-trigger" => Trigger::Right,
                    })?;
                    let (engage, release): (f32, f32) = props!(child, "engage", "release")?;
                    if !engage.is_finite()
                        || !release.is_finite()
                        || release < 0.0
                        || release >= engage
                        || engage > 1.0
                    {
                        return Err(issue(
                            child,
                            "axis mapping activation thresholds must satisfy 0 <= release < engage <= 1",
                        ));
                    }
                    activation = Some(AxisActivation::Trigger {
                        source: trigger,
                        engage,
                        release,
                    });
                },
                "buttons" => {
                    if activation.is_some() {
                        return Err(issue(
                            child,
                            "axis mapping must contain at most one activation",
                        ));
                    }
                    properties(child, &[])?;
                    let mut buttons = Buttons::default();
                    for (position, _) in arguments(child).enumerate() {
                        let button = parse_button(argument(child, position)?, child)?;
                        buttons.insert(button);
                    }
                    if arguments(child).next().is_none() {
                        return Err(issue(
                            child,
                            "axis mapping activation must contain at least one button",
                        ));
                    }
                    activation = Some(AxisActivation::Buttons(buttons));
                },
                "components" => {
                    if components.is_some() {
                        return Err(issue(
                            child,
                            "axis mapping must contain at most one components node",
                        ));
                    }
                    properties(child, &[])?;
                    components = Some([
                        parse_axis_component(argument(child, 0)?, child)?,
                        parse_axis_component(argument(child, 1)?, child)?,
                    ]);
                },
        })?;
    }
    if components.is_some() && source != "gyro" {
        return Err(issue(
            node,
            "axis mapping components is only valid for a gyro source",
        ));
    }
    let (deadzone, sensitivity, acceleration, curve, exponent, invert_x, invert_y, swap_xy) = props!(
        node,
        "deadzone" = 0.0_f32,
        "sensitivity" = 1.0_f32,
        optional "acceleration",
        "curve" = "linear",
        "exponent" = 2.0_f32,
        "invert-x" = false,
        "invert-y" = false,
        "swap-xy" = false,
    )?;
    if !deadzone.is_finite() || !(0.0..1.0).contains(&deadzone) {
        return Err(issue(
            node,
            "axis mapping deadzone must be finite and in [0, 1)",
        ));
    }
    if !sensitivity.is_finite() || sensitivity < 0.0 {
        return Err(issue(
            node,
            "axis mapping sensitivity must be finite and non-negative",
        ));
    }
    if let Some(value) = acceleration {
        let value: f32 = value;
        if !value.is_finite() || value < 0.0 {
            return Err(issue(
                node,
                "axis mapping acceleration must be finite and non-negative",
            ));
        }
        if !matches!(source, "left-pad" | "right-pad") || target != "mouse-motion" {
            return Err(issue(
                node,
                "axis mapping acceleration is only valid for a trackpad source and mouse-motion target",
            ));
        }
    }
    if !exponent.is_finite() || exponent <= 0.0 {
        return Err(issue(
            node,
            "axis mapping exponent must be finite and greater than zero",
        ));
    }
    let exponent = one_of!(
        node,
        curve,
        "axis curve",
        {
            "linear" => 1.0,
            "exponential" => exponent,
        }
    )?;
    let options = AxisOptions {
        activation,
        deadzone,
        sensitivity,
        exponent,
    };
    if scalar_source {
        return Ok(AxisMapping::Trigger {
            source: if source == "left-trigger" {
                Trigger::Left
            } else {
                Trigger::Right
            },
            target: if target == "gamepad-left-trigger" {
                Trigger::Left
            } else {
                Trigger::Right
            },
            options,
        });
    }
    let target = one_of!(node, target, "axis target", {
        "gamepad-left-stick" => VectorTarget::GamepadLeftStick,
        "gamepad-right-stick" => VectorTarget::GamepadRightStick,
        "mouse-motion" => VectorTarget::MouseMotion,
        "scroll" => VectorTarget::Scroll,
    })?;
    let options = VectorOptions {
        axis: options,
        invert_x,
        invert_y,
        swap_xy,
    };
    one_of!(node, source, "axis source", {
        "left-stick" | "right-stick" => AxisMapping::Stick {
            source: if source == "left-stick" {
                Stick::Left
            } else {
                Stick::Right
            },
            target,
            options,
        },
        "left-pad" | "right-pad" => {
            let pad = if source == "left-pad" {
                Trackpad::Left
            } else {
                Trackpad::Right
            };
            match target {
                VectorTarget::GamepadLeftStick | VectorTarget::GamepadRightStick => {
                    AxisMapping::PadPosition {
                        pad,
                        target: if target == VectorTarget::GamepadLeftStick {
                            Stick::Left
                        } else {
                            Stick::Right
                        },
                        options,
                    }
                }
                VectorTarget::MouseMotion => AxisMapping::PadMotion {
                    pad,
                    options,
                    acceleration: acceleration.unwrap_or(0.0),
                },
                VectorTarget::Scroll => AxisMapping::CircularScroll { pad, options },
            }
        },
        "gyro" => AxisMapping::Gyro {
            components: components.unwrap_or([AxisComponent::Y, AxisComponent::X]),
            target,
            options,
        },
    })
}

fn duplicate_axis_target(left: AxisMapping, right: AxisMapping) -> bool {
    fn target(mapping: AxisMapping) -> Option<u8> {
        match mapping {
            AxisMapping::Trigger {
                target: Trigger::Left,
                ..
            } => Some(0),
            AxisMapping::Trigger {
                target: Trigger::Right,
                ..
            } => Some(1),
            AxisMapping::CircularScroll { .. } => Some(2),
            AxisMapping::Stick { target, .. } | AxisMapping::Gyro { target, .. } => match target {
                VectorTarget::Scroll => Some(2),
                _ => None,
            },
            _ => None,
        }
    }
    target(left).is_some() && target(left) == target(right)
}

fn parse_haptic(node: &KdlNode) -> Result<Haptic, ConfigIssue> {
    properties(node, &[])?;
    let children = children(node);
    if children.len() != 1 {
        return Err(issue(
            node,
            "mode-switch-haptic must contain exactly one haptic",
        ));
    }
    let node = &children[0];
    one_of!(node, node.name().value(), "haptic", {
        "rumble" => {
            let (low_frequency, high_frequency) =
                props!(node, "low-frequency", "high-frequency")?;
            Haptic::Rumble {
                low_frequency,
                high_frequency,
            }
        },
        "trackpad-click" => {
            let gain = props!(node, "gain")?;
            Haptic::TrackpadClick {
                trackpad: parse_trackpad(argument(node, 0)?, node)?,
                gain,
            }
        },
        "pulse" => {
            let (on_us, off_us, repeat) = props!(
                node,
                "on-us",
                "off-us",
                "repeat",
            )?;
            Haptic::Pulse {
                on_us,
                off_us,
                repeat,
            }
        },
        "tone" => {
            let (gain, frequency, duration_ms) = props!(
                node,
                "gain",
                "frequency",
                "duration-ms",
            )?;
            Haptic::Tone {
                gain,
                frequency,
                duration_ms,
            }
        },
        "sweep" => {
            let (gain, duration_ms, start_frequency, end_frequency) = props!(
                node,
                "gain",
                "duration-ms",
                "start-frequency",
                "end-frequency",
            )?;
            Haptic::LogSweep {
                gain,
                duration_ms,
                start_frequency,
                end_frequency,
            }
        },
        "script" => {
            let gain = props!(node, "gain")?;
            Haptic::Script {
                script: argument(node, 0)?,
                gain,
            }
        },
    })
}

fn parse_trackpad(value: &str, node: &KdlNode) -> Result<Trackpad, ConfigIssue> {
    one_of!(node, value, "trackpad", {
        "left" => Trackpad::Left,
        "right" => Trackpad::Right,
    })
}

fn parse_button(value: &str, node: &KdlNode) -> Result<Button, ConfigIssue> {
    one_of!(node, value, "controller button", {
        "a" => Button::A,
        "b" => Button::B,
        "x" => Button::X,
        "y" => Button::Y,
        "quick-access" => Button::Qam,
        "right-stick-click" | "r3" => Button::R3,
        "menu" => Button::Menu,
        "r4" => Button::R4,
        "r5" => Button::R5,
        "right-bumper" | "rb" => Button::Rb,
        "dpad-down" => Button::DpadDown,
        "dpad-right" => Button::DpadRight,
        "dpad-left" => Button::DpadLeft,
        "dpad-up" => Button::DpadUp,
        "view" => Button::View,
        "left-stick-click" | "l3" => Button::L3,
        "steam" => Button::Steam,
        "l4" => Button::L4,
        "l5" => Button::L5,
        "left-bumper" | "lb" => Button::Lb,
        "right-stick-touch" => Button::RightStickTouch,
        "right-pad-touch" => Button::RightPadTouch,
        "right-pad-click" => Button::RightPadClick,
        "right-trigger-click" => Button::RightTriggerClick,
        "left-stick-touch" => Button::LeftStickTouch,
        "left-pad-touch" => Button::LeftPadTouch,
        "left-pad-click" => Button::LeftPadClick,
        "left-trigger-click" => Button::LeftTriggerClick,
        "right-grip-touch" => Button::RightGripTouch,
        "left-grip-touch" => Button::LeftGripTouch,
    })
}

fn parse_mouse_button(value: &str, node: &KdlNode) -> Result<MouseButton, ConfigIssue> {
    one_of!(node, value, "mouse button", {
        "left" => MouseButton::Left,
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        "side" => MouseButton::Side,
        "extra" => MouseButton::Extra,
        "forward" => MouseButton::Forward,
        "back" => MouseButton::Back,
        "task" => MouseButton::Task,
    })
}

fn parse_gamepad_button(value: &str, node: &KdlNode) -> Result<GamepadButton, ConfigIssue> {
    one_of!(node, value, "gamepad button", {
        "south" => GamepadButton::South,
        "east" => GamepadButton::East,
        "north" => GamepadButton::North,
        "west" => GamepadButton::West,
        "left-bumper" => GamepadButton::LeftBumper,
        "right-bumper" => GamepadButton::RightBumper,
        "left-trigger" => GamepadButton::LeftTrigger,
        "right-trigger" => GamepadButton::RightTrigger,
        "select" => GamepadButton::Select,
        "start" => GamepadButton::Start,
        "guide" => GamepadButton::Guide,
        "left-stick" => GamepadButton::LeftStick,
        "right-stick" => GamepadButton::RightStick,
        "dpad-up" => GamepadButton::DpadUp,
        "dpad-down" => GamepadButton::DpadDown,
        "dpad-left" => GamepadButton::DpadLeft,
        "dpad-right" => GamepadButton::DpadRight,
        "paddle-left-upper" => GamepadButton::PaddleLeftUpper,
        "paddle-left-lower" => GamepadButton::PaddleLeftLower,
        "paddle-right-upper" => GamepadButton::PaddleRightUpper,
        "paddle-right-lower" => GamepadButton::PaddleRightLower,
    })
}

fn parse_axis_component(value: &str, node: &KdlNode) -> Result<AxisComponent, ConfigIssue> {
    one_of!(node, value, "axis component", {
        "x" => AxisComponent::X,
        "y" => AxisComponent::Y,
        "z" => AxisComponent::Z,
    })
}

fn parse_key(name: &str) -> Result<KeyCode, String> {
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
            KeyCode::from_str(&code).map_err(|_| format!("unknown key {name:?}"))?
        }
    };
    if !is_keyboard_key(key) {
        return Err(format!("{name:?} is not a keyboard key"));
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
            r#"version 1
            default-mode "desktop"
            osk {
                bind "l4" "super"
                bind "left-trigger-click" "shift"
            }
            global {
                bind "steam" "x" {
                    keyboard-toggle
                }
            }
            mode "desktop" gamepad="none" {
                bind "l4" {
                    key "super"
                }
                bind "x" {
                    mode-set "anything at all"
                }
                layer "apps" hold="left-bumper" {
                    bind "b" {
                        key "q"
                    }
                }
                axis "gyro" "mouse-motion" curve="exponential"
            }
            mode "anything at all" gamepad="dualshock4" touchpad="right" {
            }"#,
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
            r#"version 1
            default-mode "one"
            mode "one" {
                axis "left-pad" "gamepad-left-stick"
                axis "right-pad" "mouse-motion"
                axis "left-pad" "scroll"
                axis "gyro" "gamepad-right-stick"
                axis "gyro" "mouse-motion"
            }"#,
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
    fn parses_haptics_and_axis_options() {
        let config = Config::parse(
            r#"version 1
            default-mode "one"
            mode-switch-haptic {
                sweep gain=-2 duration-ms=65 start-frequency=400 end-frequency=850
            }
            mode "one" {
                axis "gyro" "mouse-motion" deadzone=0.1 sensitivity=2.0 curve="exponential" exponent=3.0 invert-x=#true invert-y=#true swap-xy=#true {
                    buttons "left-grip-touch" "right-grip-touch"
                    components "z" "x"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.mode_switch_haptic,
            Some(Haptic::LogSweep {
                gain: -2,
                duration_ms: 65,
                start_frequency: 400,
                end_frequency: 850,
            })
        );
        let AxisMapping::Gyro {
            components,
            options,
            ..
        } = config.modes[0].axes[0]
        else {
            panic!("expected gyro mapping");
        };
        assert_eq!(components, [AxisComponent::Z, AxisComponent::X]);
        assert!(matches!(
            options.axis.activation,
            Some(AxisActivation::Buttons(_))
        ));
        assert_eq!(options.axis.deadzone, 0.1);
        assert_eq!(options.axis.sensitivity, 2.0);
        assert_eq!(options.axis.exponent, 3.0);
        assert!(options.invert_x && options.invert_y && options.swap_xy);
    }

    #[test]
    fn aggregates_independent_errors() {
        let report = Config::parse(
            r#"version 2
            default-mode "missing"
            trackpad click-pressure=101
            osk {
                bind "left-pad-click" "enter"
            }
            global {
                bind  {
                    mode-next
                }
                bind "a" {
                    mode-set "also missing"
                }
            }
            mode "one" {
                bind "a" {
                    keyboard-toggle
                }
            }"#,
        )
        .unwrap_err();

        let error = report.downcast_ref::<ConfigError>().unwrap();
        assert_eq!(error.issues.len(), 7);
        for expected in [
            "version must be 1",
            "does not name a configured mode",
            "click_pressure must be in [0, 100]",
            "reserved for keyboard pointing",
            "must contain at least one button",
            "mode-set target \"also missing\" is not configured",
            "keyboard-toggle is only valid for global bindings",
        ] {
            assert!(
                error
                    .issues
                    .iter()
                    .any(|issue| issue.message.contains(expected))
            );
        }
    }

    #[test]
    fn rejects_invalid_digital_configuration() {
        for (body, expected) in [
            (
                r#"global {
                       bind "steam" "steam" {
                           mode-next
                       }
                   }
                   mode "one" {}"#,
                "duplicate button",
            ),
            (
                r#"global {
                       bind {
                           mode-next
                       }
                   }
                   mode "one" {}"#,
                "must contain at least one button",
            ),
            (
                r#"osk {
                       bind "left-pad-click" "enter"
                   }
                   mode "one" {}"#,
                "reserved for keyboard pointing",
            ),
            (
                r#"mode "one" {
                       bind "left-bumper" {
                           key "q"
                       }
                       layer "apps" hold="left-bumper" {}
                   }"#,
                "must not also be a mode binding",
            ),
            (
                r#"mode "one" {
                       bind "a" {
                           mode-set "missing"
                       }
                   }"#,
                "mode-set target",
            ),
            (
                r#"mode "one" {
                       bind "a" {
                           keyboard-toggle
                       }
                   }"#,
                "only valid for global bindings",
            ),
            (
                r#"mode "one" {
                       bind "a" { key "q"; }
                       bind "a" { key "w"; }
                   }"#,
                "duplicates an earlier input",
            ),
            (
                r#"mode "one" {
                       bind "steam" "left-bumper" "x" { key "q"; }
                       bind "left-bumper" "steam" "x" { key "w"; }
                   }"#,
                "duplicates an earlier input",
            ),
            (
                r#"mode "one" {
                       bind "a" typo=#true { key "q"; }
                   }"#,
                "unexpected property",
            ),
        ] {
            let source = format!("version 1\ndefault-mode \"one\"\n{body}");
            let report = Config::parse(&source).unwrap_err();
            let error = report.downcast_ref::<ConfigError>().unwrap();
            assert!(
                error
                    .issues
                    .iter()
                    .any(|issue| issue.message.contains(expected))
            );
        }
    }

    #[test]
    fn rejects_invalid_analog_configuration() {
        for (mapping, expected) in [
            (
                r#"axis "right-pad" "mouse-motion" acceleration=-1.0"#,
                "acceleration must be finite and non-negative",
            ),
            (
                r#"axis "right-stick" "mouse-motion" acceleration=1.0"#,
                "acceleration is only valid",
            ),
            (r#"axis "right-trigger" "mouse-motion""#, "scalar source"),
            (
                r#"axis "gyro" "gamepad-right-stick" {
                       trigger "left-trigger" engage=0.1 release=0.2
                   }"#,
                "activation thresholds",
            ),
        ] {
            let source = format!("version 1\ndefault-mode \"one\"\nmode \"one\" {{\n{mapping}\n}}");
            let report = Config::parse(&source).unwrap_err();
            let error = report.downcast_ref::<ConfigError>().unwrap();
            assert!(
                error
                    .issues
                    .iter()
                    .any(|issue| issue.message.contains(expected))
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
                    version 1
                    default-mode "one"
                    mode "one" {{
                        bind "a" {{
                            key "{code}"
                        }}
                    }}
                "#
            );
            let report = Config::parse(&source).unwrap_err();
            let error = report.downcast_ref::<ConfigError>().unwrap();
            assert!(
                error
                    .issues
                    .iter()
                    .any(|issue| issue.message.contains("is not a keyboard key"))
            );
        }
    }
}
