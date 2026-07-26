use std::{error::Error, fmt};

use evdev::KeyCode;
use indexmap::IndexMap;

use crate::{
    config::{
        Action, Activation, AnalogSource, AnalogTarget, Axis, AxisComponent, AxisMapping, Binding,
        Button, Config, ConfigError, Curve, DigitalInput, Direction, GamepadButton, MouseButton,
    },
    protocol::{Button as ProtocolButton, ControllerState, StateFormat, TouchpadState, Trackpad},
};

const TRACKPAD_HAPTIC_MIN_TRAVEL: f32 = 45.0 / 32767.0;
const TRACKPAD_HAPTIC_MAX_TRAVEL: f32 = 4000.0 / 32767.0;
const TRACKPAD_HAPTIC_TICK_TRAVEL: f32 = 3200.0 / 32767.0;
const TRACKPAD_HAPTIC_MIN_INTERVAL_US: u32 = 25_000;
const TRACKPAD_SCROLL_MIN_RADIUS: f32 = 1.0 / 3.0;

pub struct Mapper {
    config: Config,
    active_mode: String,
    global_active: Vec<bool>,
    global_capture: Vec<bool>,
    mode_active: Vec<bool>,
    mode_capture: Vec<bool>,
    layer_active: Vec<Vec<bool>>,
    layer_capture: Vec<Vec<bool>>,
    held: IndexMap<HeldOutput, usize>,
    gamepad_axes: IndexMap<GamepadAxis, f32>,
    left_pad_haptic: TrackpadHapticState,
    right_pad_haptic: TrackpadHapticState,
    previous: Option<ControllerState>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Key {
        key: KeyCode,
        pressed: bool,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    GamepadButton {
        button: GamepadButton,
        pressed: bool,
    },
    GamepadAxis {
        axis: GamepadAxis,
        value: f32,
    },
    MouseMotion {
        x: f32,
        y: f32,
    },
    Scroll {
        x: f32,
        y: f32,
    },
    TrackpadHaptic {
        pad: Trackpad,
    },
    Event {
        name: String,
    },
    ModeChanged {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GamepadAxis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftTrigger,
    RightTrigger,
}

#[derive(Debug)]
pub enum MapperError {
    Config(ConfigError),
    UnknownMode(String),
}

impl Mapper {
    pub fn new(config: Config) -> Result<Self, MapperError> {
        config.validate().map_err(MapperError::Config)?;
        let active_mode = config.default_mode.clone();
        let mode_active = vec![false; config.modes[&active_mode].bindings.len()];
        let mode_capture = vec![false; config.modes[&active_mode].bindings.len()];
        let layer_active = config.modes[&active_mode]
            .layers
            .values()
            .map(|layer| vec![false; layer.bindings.len()])
            .collect();
        let layer_capture = config.modes[&active_mode]
            .layers
            .values()
            .map(|layer| vec![false; layer.bindings.len()])
            .collect();
        let global_active = vec![false; config.global.bindings.len()];
        let global_capture = vec![false; config.global.bindings.len()];

        Ok(Self {
            config,
            active_mode,
            global_active,
            global_capture,
            mode_active,
            mode_capture,
            layer_active,
            layer_capture,
            held: IndexMap::new(),
            gamepad_axes: IndexMap::new(),
            left_pad_haptic: TrackpadHapticState::default(),
            right_pad_haptic: TrackpadHapticState::default(),
            previous: None,
        })
    }

    pub fn active_mode(&self) -> &str {
        &self.active_mode
    }

    pub fn process(&mut self, state: &ControllerState) -> Vec<Output> {
        let mut outputs = Vec::new();
        let mut mode_changed = false;

        for index in 0..self.config.global.bindings.len() {
            let binding = &self.config.global.bindings[index];
            let was_active = self.global_active[index];
            let (active, pressed, released) =
                binding_transition(binding, was_active, state, self.previous.as_ref());
            self.global_active[index] = active;
            if binding.consume {
                if pressed {
                    self.global_capture[index] = true;
                } else if self.global_capture[index] && binding_trigger_released(binding, state) {
                    self.global_capture[index] = false;
                }
            }

            let should_apply = match binding.action {
                Action::Key { .. } | Action::Mouse { .. } | Action::Gamepad { .. } => {
                    pressed || (was_active && !active)
                }
                _ => match binding.activation {
                    Activation::Press => pressed,
                    Activation::Release => released,
                },
            };
            let action = should_apply.then(|| binding.action.clone());

            if let Some(action) = action {
                mode_changed |= self.apply_action(&action, active, was_active, &mut outputs);
            }
        }

        if !mode_changed {
            let binding_count = self.config.modes[&self.active_mode].bindings.len();
            for index in 0..binding_count {
                let binding = &self.config.modes[&self.active_mode].bindings[index];
                let was_active = self.mode_active[index];
                let layer_overridden = self.config.modes[&self.active_mode]
                    .layers
                    .values()
                    .enumerate()
                    .any(|(layer_index, layer)| {
                        let held = digital_active(&layer.hold, state);
                        (held
                            && !binding.consume
                            && binding
                                .input
                                .iter()
                                .chain(binding.chord.iter().flatten())
                                .any(|input| inputs_conflict(input, &layer.hold)))
                            || layer.bindings.iter().enumerate().any(
                                |(binding_index, layer_binding)| {
                                    bindings_match(binding, layer_binding)
                                        && (held || self.layer_capture[layer_index][binding_index])
                                },
                            )
                    });
                let globally_consumed = binding
                    .input
                    .iter()
                    .chain(binding.chord.iter().flatten())
                    .any(|input| {
                        input_is_consumed(
                            input,
                            binding,
                            &self.config.global.bindings,
                            &self.global_capture,
                            state,
                            true,
                        )
                    });
                let mode_consumed = binding
                    .input
                    .iter()
                    .chain(binding.chord.iter().flatten())
                    .any(|input| {
                        input_is_consumed(
                            input,
                            binding,
                            &self.config.modes[&self.active_mode].bindings[..index],
                            &self.mode_capture[..index],
                            state,
                            true,
                        )
                    });
                let consumed =
                    (layer_overridden && !was_active) || globally_consumed || mode_consumed;
                let (active, pressed, released) = if consumed {
                    (false, false, false)
                } else {
                    binding_transition(binding, was_active, state, self.previous.as_ref())
                };
                self.mode_active[index] = active;
                if binding.consume {
                    if pressed {
                        self.mode_capture[index] = true;
                    } else if self.mode_capture[index] && binding_trigger_released(binding, state) {
                        self.mode_capture[index] = false;
                    }
                }

                let should_apply = match binding.action {
                    Action::Key { .. } | Action::Mouse { .. } | Action::Gamepad { .. } => {
                        pressed || (was_active && !active)
                    }
                    _ => match binding.activation {
                        Activation::Press => pressed,
                        Activation::Release => released,
                    },
                };
                let action = should_apply.then(|| binding.action.clone());

                if let Some(action) = action {
                    mode_changed |= self.apply_action(&action, active, was_active, &mut outputs);
                    if mode_changed {
                        break;
                    }
                }
            }
        }

        if !mode_changed {
            let layer_count = self.config.modes[&self.active_mode].layers.len();
            for layer_index in 0..layer_count {
                let layer = self.config.modes[&self.active_mode]
                    .layers
                    .get_index(layer_index)
                    .unwrap()
                    .1;
                let layer_hold = layer.hold.clone();
                let layer_held = digital_active(&layer_hold, state);
                let binding_count = layer.bindings.len();

                for binding_index in 0..binding_count {
                    let binding = &self.config.modes[&self.active_mode]
                        .layers
                        .get_index(layer_index)
                        .unwrap()
                        .1
                        .bindings[binding_index];
                    let was_active = self.layer_active[layer_index][binding_index];
                    let base_latched = self.config.modes[&self.active_mode]
                        .bindings
                        .iter()
                        .enumerate()
                        .any(|(base_index, base)| {
                            self.mode_active[base_index] && bindings_match(base, binding)
                        });
                    let other_layer_latched = self.config.modes[&self.active_mode]
                        .layers
                        .values()
                        .enumerate()
                        .any(|(other_layer_index, other_layer)| {
                            other_layer_index != layer_index
                                && other_layer.bindings.iter().enumerate().any(
                                    |(other_binding_index, other_binding)| {
                                        self.layer_active[other_layer_index][other_binding_index]
                                            && bindings_match(binding, other_binding)
                                    },
                                )
                        });
                    let earlier_layer_override = self.config.modes[&self.active_mode]
                        .layers
                        .values()
                        .take(layer_index)
                        .enumerate()
                        .any(|(earlier_layer_index, earlier_layer)| {
                            let earlier_held = digital_active(&earlier_layer.hold, state);
                            earlier_layer.bindings.iter().enumerate().any(
                                |(earlier_binding_index, earlier_binding)| {
                                    bindings_match(binding, earlier_binding)
                                        && (earlier_held
                                            || self.layer_capture[earlier_layer_index]
                                                [earlier_binding_index])
                                },
                            )
                        });
                    let globally_consumed = binding
                        .input
                        .iter()
                        .chain(binding.chord.iter().flatten())
                        .any(|input| {
                            input_is_consumed(
                                input,
                                binding,
                                &self.config.global.bindings,
                                &self.global_capture,
                                state,
                                true,
                            )
                        });
                    let hold_globally_consumed = input_is_consumed(
                        &layer_hold,
                        binding,
                        &self.config.global.bindings,
                        &self.global_capture,
                        state,
                        true,
                    );
                    let mode_consumed = binding
                        .input
                        .iter()
                        .chain(binding.chord.iter().flatten())
                        .any(|input| {
                            input_is_consumed(
                                input,
                                binding,
                                &self.config.modes[&self.active_mode].bindings,
                                &self.mode_capture,
                                state,
                                true,
                            )
                        });
                    let hold_mode_consumed = input_is_consumed(
                        &layer_hold,
                        binding,
                        &self.config.modes[&self.active_mode].bindings,
                        &self.mode_capture,
                        state,
                        true,
                    );
                    let layer_consumed = binding
                        .input
                        .iter()
                        .chain(binding.chord.iter().flatten())
                        .any(|input| {
                            (0..=layer_index).any(|consumer_layer_index| {
                                let consumer_layer = self.config.modes[&self.active_mode]
                                    .layers
                                    .get_index(consumer_layer_index)
                                    .unwrap()
                                    .1;
                                let end = if consumer_layer_index == layer_index {
                                    binding_index
                                } else {
                                    consumer_layer.bindings.len()
                                };
                                input_is_consumed(
                                    input,
                                    binding,
                                    &consumer_layer.bindings[..end],
                                    &self.layer_capture[consumer_layer_index][..end],
                                    state,
                                    digital_active(&consumer_layer.hold, state),
                                )
                            })
                        });
                    let suppressed = base_latched
                        || other_layer_latched
                        || (earlier_layer_override && !was_active)
                        || globally_consumed
                        || hold_globally_consumed
                        || mode_consumed
                        || hold_mode_consumed
                        || layer_consumed
                        || (self.layer_capture[layer_index][binding_index] && !was_active);
                    let (active, pressed, released) = if !layer_held {
                        (false, false, was_active)
                    } else if suppressed {
                        (false, false, false)
                    } else {
                        binding_transition(binding, was_active, state, self.previous.as_ref())
                    };
                    self.layer_active[layer_index][binding_index] = active;
                    if active {
                        self.layer_capture[layer_index][binding_index] = true;
                    } else if self.layer_capture[layer_index][binding_index]
                        && binding_trigger_released(binding, state)
                    {
                        self.layer_capture[layer_index][binding_index] = false;
                    }

                    let should_apply = match binding.action {
                        Action::Key { .. } | Action::Mouse { .. } | Action::Gamepad { .. } => {
                            pressed || (was_active && !active)
                        }
                        _ => match binding.activation {
                            Activation::Press => pressed,
                            Activation::Release => released,
                        },
                    };
                    let action = should_apply.then(|| binding.action.clone());

                    if let Some(action) = action {
                        mode_changed |=
                            self.apply_action(&action, active, was_active, &mut outputs);
                        if mode_changed {
                            break;
                        }
                    }
                }

                if mode_changed {
                    break;
                }
            }
        }

        if !mode_changed {
            let mapping_count = self.config.modes[&self.active_mode].axes.len();
            for index in 0..mapping_count {
                let mapping = &self.config.modes[&self.active_mode].axes[index];
                let target = mapping.target;
                let value = analog_value(mapping, state, self.previous.as_ref());

                match (target, value) {
                    (AnalogTarget::GamepadLeftStick, AnalogValue::Vector([x, y])) => {
                        self.set_gamepad_axis(GamepadAxis::LeftX, x, &mut outputs);
                        self.set_gamepad_axis(GamepadAxis::LeftY, y, &mut outputs);
                    }
                    (AnalogTarget::GamepadRightStick, AnalogValue::Vector([x, y])) => {
                        self.set_gamepad_axis(GamepadAxis::RightX, x, &mut outputs);
                        self.set_gamepad_axis(GamepadAxis::RightY, y, &mut outputs);
                    }
                    (AnalogTarget::GamepadLeftTrigger, AnalogValue::Scalar(value)) => {
                        self.set_gamepad_axis(GamepadAxis::LeftTrigger, value, &mut outputs);
                    }
                    (AnalogTarget::GamepadRightTrigger, AnalogValue::Scalar(value)) => {
                        self.set_gamepad_axis(GamepadAxis::RightTrigger, value, &mut outputs);
                    }
                    (AnalogTarget::MouseMotion, AnalogValue::Vector([x, y])) => {
                        if x != 0.0 || y != 0.0 {
                            outputs.push(Output::MouseMotion { x, y });
                        }
                    }
                    (AnalogTarget::Scroll, AnalogValue::Vector([x, y])) => {
                        if x != 0.0 || y != 0.0 {
                            outputs.push(Output::Scroll { x, y });
                        }
                    }
                    _ => unreachable!("configuration validation rejects mismatched analog shapes"),
                }
            }

            let trackpad_timestamp_us = state
                .trackpad_timestamp_us
                .unwrap_or(state.imu_timestamp_us);
            if trackpad_haptic(
                &mut self.left_pad_haptic,
                state.left_pad,
                state.format,
                trackpad_timestamp_us,
            ) {
                outputs.push(Output::TrackpadHaptic {
                    pad: Trackpad::Left,
                });
            }
            if trackpad_haptic(
                &mut self.right_pad_haptic,
                state.right_pad,
                state.format,
                trackpad_timestamp_us,
            ) {
                outputs.push(Output::TrackpadHaptic {
                    pad: Trackpad::Right,
                });
            }
        }

        self.previous = Some(*state);
        outputs
    }

    pub fn set_mode(&mut self, name: &str) -> Result<Vec<Output>, MapperError> {
        if !self.config.modes.contains_key(name) {
            return Err(MapperError::UnknownMode(name.to_owned()));
        }
        if self.active_mode == name {
            return Ok(Vec::new());
        }

        let mut outputs = Vec::new();
        self.switch_mode(name, &mut outputs);
        Ok(outputs)
    }

    pub fn next_mode(&mut self) -> Vec<Output> {
        let mut outputs = Vec::new();
        self.switch_to_next_mode(&mut outputs);
        outputs
    }

    pub fn reload(&mut self, config: Config) -> Result<Vec<Output>, ConfigError> {
        config.validate()?;

        let mut outputs = Vec::new();
        self.release_outputs(&mut outputs);
        let active_mode = if config.modes.contains_key(&self.active_mode) {
            self.active_mode.clone()
        } else {
            config.default_mode.clone()
        };
        self.config = config;
        self.active_mode = active_mode;
        self.global_active = vec![false; self.config.global.bindings.len()];
        self.global_capture = vec![false; self.config.global.bindings.len()];
        self.mode_active = vec![false; self.config.modes[&self.active_mode].bindings.len()];
        self.mode_capture = vec![false; self.config.modes[&self.active_mode].bindings.len()];
        self.layer_active = self.config.modes[&self.active_mode]
            .layers
            .values()
            .map(|layer| vec![false; layer.bindings.len()])
            .collect();
        self.layer_capture = self.config.modes[&self.active_mode]
            .layers
            .values()
            .map(|layer| vec![false; layer.bindings.len()])
            .collect();
        self.left_pad_haptic = TrackpadHapticState::default();
        self.right_pad_haptic = TrackpadHapticState::default();
        outputs.push(Output::ModeChanged {
            name: self.active_mode.clone(),
        });
        Ok(outputs)
    }

    pub fn release_all(&mut self) -> Vec<Output> {
        let mut outputs = Vec::new();
        self.release_outputs(&mut outputs);
        self.global_active.fill(false);
        self.global_capture.fill(false);
        self.mode_active.fill(false);
        self.mode_capture.fill(false);
        for active in &mut self.layer_active {
            active.fill(false);
        }
        for capture in &mut self.layer_capture {
            capture.fill(false);
        }
        self.left_pad_haptic = TrackpadHapticState::default();
        self.right_pad_haptic = TrackpadHapticState::default();
        self.previous = None;
        outputs
    }

    fn apply_action(
        &mut self,
        action: &Action,
        active: bool,
        was_active: bool,
        outputs: &mut Vec<Output>,
    ) -> bool {
        match action {
            Action::Key { key } => {
                self.set_held(HeldOutput::Key(*key), active, outputs);
            }
            Action::Mouse { button } => {
                self.set_held(HeldOutput::Mouse(*button), active, outputs);
            }
            Action::Gamepad { button } => {
                self.set_held(HeldOutput::Gamepad(*button), active, outputs);
            }
            Action::ModeSet { name } => {
                self.switch_mode(name, outputs);
                return true;
            }
            Action::ModeNext => {
                self.switch_to_next_mode(outputs);
                return true;
            }
            Action::Event { name } => outputs.push(Output::Event { name: name.clone() }),
        }

        debug_assert!(active != was_active);
        false
    }

    fn switch_mode(&mut self, name: &str, outputs: &mut Vec<Output>) {
        if self.active_mode == name {
            return;
        }

        self.release_outputs(outputs);
        self.active_mode.clear();
        self.active_mode.push_str(name);
        self.global_active.fill(false);
        self.global_capture.fill(false);
        self.mode_active = vec![false; self.config.modes[name].bindings.len()];
        self.mode_capture = vec![false; self.config.modes[name].bindings.len()];
        self.layer_active = self.config.modes[name]
            .layers
            .values()
            .map(|layer| vec![false; layer.bindings.len()])
            .collect();
        self.layer_capture = self.config.modes[name]
            .layers
            .values()
            .map(|layer| vec![false; layer.bindings.len()])
            .collect();
        self.left_pad_haptic = TrackpadHapticState::default();
        self.right_pad_haptic = TrackpadHapticState::default();
        outputs.push(Output::ModeChanged {
            name: name.to_owned(),
        });
    }

    fn switch_to_next_mode(&mut self, outputs: &mut Vec<Output>) {
        let current = self.config.modes.get_index_of(&self.active_mode).unwrap();
        let next = (current + 1) % self.config.modes.len();
        let name = self.config.modes.get_index(next).unwrap().0.clone();
        self.switch_mode(&name, outputs);
    }

    fn set_held(&mut self, held: HeldOutput, active: bool, outputs: &mut Vec<Output>) {
        if active {
            if let Some(count) = self.held.get_mut(&held) {
                *count += 1;
                return;
            }
            match &held {
                HeldOutput::Key(key) => outputs.push(Output::Key {
                    key: *key,
                    pressed: true,
                }),
                HeldOutput::Mouse(button) => outputs.push(Output::MouseButton {
                    button: *button,
                    pressed: true,
                }),
                HeldOutput::Gamepad(button) => outputs.push(Output::GamepadButton {
                    button: *button,
                    pressed: true,
                }),
            }
            self.held.insert(held, 1);
            return;
        }

        let Some(count) = self.held.get_mut(&held) else {
            return;
        };
        *count -= 1;
        if *count != 0 {
            return;
        }
        self.held.shift_remove(&held);
        match held {
            HeldOutput::Key(key) => outputs.push(Output::Key {
                key,
                pressed: false,
            }),
            HeldOutput::Mouse(button) => outputs.push(Output::MouseButton {
                button,
                pressed: false,
            }),
            HeldOutput::Gamepad(button) => outputs.push(Output::GamepadButton {
                button,
                pressed: false,
            }),
        }
    }

    fn set_gamepad_axis(&mut self, axis: GamepadAxis, value: f32, outputs: &mut Vec<Output>) {
        if self.gamepad_axes.get(&axis).copied() == Some(value) {
            return;
        }
        self.gamepad_axes.insert(axis, value);
        outputs.push(Output::GamepadAxis { axis, value });
    }

    fn release_outputs(&mut self, outputs: &mut Vec<Output>) {
        for (held, _) in self.held.drain(..).rev() {
            match held {
                HeldOutput::Key(key) => outputs.push(Output::Key {
                    key,
                    pressed: false,
                }),
                HeldOutput::Mouse(button) => outputs.push(Output::MouseButton {
                    button,
                    pressed: false,
                }),
                HeldOutput::Gamepad(button) => outputs.push(Output::GamepadButton {
                    button,
                    pressed: false,
                }),
            }
        }
        for (axis, value) in self.gamepad_axes.drain(..) {
            if value != 0.0 {
                outputs.push(Output::GamepadAxis { axis, value: 0.0 });
            }
        }
    }
}

impl fmt::Display for MapperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::UnknownMode(name) => write!(formatter, "unknown mode {name:?}"),
        }
    }
}

impl Error for MapperError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::UnknownMode(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum HeldOutput {
    Key(KeyCode),
    Mouse(MouseButton),
    Gamepad(GamepadButton),
}

#[derive(Clone, Copy)]
enum AnalogValue {
    Scalar(f32),
    Vector([f32; 2]),
}

#[derive(Default)]
struct TrackpadHapticState {
    previous_position: Option<[f32; 2]>,
    progress: f32,
    last_tick: Option<(StateFormat, u32)>,
}

fn binding_transition(
    binding: &Binding,
    was_active: bool,
    current: &ControllerState,
    previous: Option<&ControllerState>,
) -> (bool, bool, bool) {
    if let Some(input) = &binding.input {
        let active = digital_active(input, current);
        let previous = previous.is_some_and(|state| digital_active(input, state));
        return (active, active && !previous, was_active && !active);
    }

    let chord = binding.chord.as_ref().unwrap();
    let all_active = chord.iter().all(|input| digital_active(input, current));
    let ordered_press = all_active
        && !previous.is_some_and(|state| digital_active(chord.last().unwrap(), state))
        && chord[..chord.len() - 1]
            .iter()
            .all(|input| previous.is_some_and(|state| digital_active(input, state)));
    let active = if was_active {
        all_active
    } else {
        ordered_press
    };
    (active, ordered_press, was_active && !all_active)
}

fn binding_trigger_released(binding: &Binding, state: &ControllerState) -> bool {
    binding
        .input
        .as_ref()
        .or_else(|| binding.chord.as_ref().unwrap().last())
        .is_some_and(|input| !digital_active(input, state))
}

fn input_is_consumed(
    input: &DigitalInput,
    candidate: &Binding,
    consumers: &[Binding],
    captures: &[bool],
    state: &ControllerState,
    include_active_prefix: bool,
) -> bool {
    for (index, binding) in consumers
        .iter()
        .enumerate()
        .filter(|(_, binding)| binding.consume)
    {
        if captures[index]
            && binding
                .input
                .iter()
                .chain(binding.chord.iter().flatten())
                .any(|global| inputs_conflict(input, global))
        {
            return true;
        }
        if !include_active_prefix {
            continue;
        }
        if let Some(global) = &binding.input {
            if digital_active(global, state) && inputs_conflict(input, global) {
                return true;
            }
            continue;
        }

        let chord = binding.chord.as_ref().unwrap();
        for (position, global) in chord.iter().enumerate() {
            if !digital_active(global, state) {
                break;
            }
            if inputs_conflict(input, global) {
                if candidate.consume
                    && candidate.chord.as_ref().is_some_and(|candidate| {
                        candidate.get(position) == Some(global)
                            && candidate[..=position] == chord[..=position]
                    })
                {
                    continue;
                }
                return true;
            }
        }
    }
    false
}

fn bindings_match(left: &Binding, right: &Binding) -> bool {
    left.input == right.input && left.chord == right.chord
}

fn inputs_conflict(left: &DigitalInput, right: &DigitalInput) -> bool {
    match (left, right) {
        (DigitalInput::Button(left), DigitalInput::Button(right)) => left == right,
        (DigitalInput::Axis(left), DigitalInput::Axis(right)) => left.axis == right.axis,
        _ => false,
    }
}

fn trackpad_haptic(
    haptic: &mut TrackpadHapticState,
    pad: TouchpadState,
    format: StateFormat,
    timestamp_us: u32,
) -> bool {
    if !pad.touched {
        *haptic = TrackpadHapticState::default();
        return false;
    }
    if haptic
        .last_tick
        .is_some_and(|(previous_format, _)| previous_format != format)
    {
        *haptic = TrackpadHapticState {
            previous_position: Some(pad.position),
            ..TrackpadHapticState::default()
        };
        return false;
    }

    let Some(previous_position) = haptic.previous_position else {
        haptic.previous_position = Some(pad.position);
        return false;
    };
    let travel =
        (pad.position[0] - previous_position[0]).hypot(pad.position[1] - previous_position[1]);
    if travel < TRACKPAD_HAPTIC_MIN_TRAVEL {
        return false;
    }
    haptic.previous_position = Some(pad.position);
    if travel > TRACKPAD_HAPTIC_MAX_TRAVEL {
        haptic.progress = 0.0;
        return false;
    }

    haptic.progress += travel;
    if haptic.progress < TRACKPAD_HAPTIC_TICK_TRAVEL {
        return false;
    }
    if haptic.last_tick.is_some_and(|(_, previous_timestamp_us)| {
        timestamp_delta_us(format, timestamp_us, previous_timestamp_us)
            < TRACKPAD_HAPTIC_MIN_INTERVAL_US
    }) {
        haptic.progress = TRACKPAD_HAPTIC_TICK_TRAVEL;
        return false;
    }

    haptic.progress %= TRACKPAD_HAPTIC_TICK_TRAVEL;
    haptic.last_tick = Some((format, timestamp_us));
    true
}

fn timestamp_delta_us(format: StateFormat, current: u32, previous: u32) -> u32 {
    if format == StateFormat::Timestamp32Us {
        u32::from(((current / 32) as u16).wrapping_sub((previous / 32) as u16)) * 32
    } else {
        current.wrapping_sub(previous)
    }
}

fn digital_active(input: &DigitalInput, state: &ControllerState) -> bool {
    match input {
        DigitalInput::Button(button) => state.buttons.contains(match button {
            Button::A => ProtocolButton::A,
            Button::B => ProtocolButton::B,
            Button::X => ProtocolButton::X,
            Button::Y => ProtocolButton::Y,
            Button::QuickAccess => ProtocolButton::Qam,
            Button::RightStickClick => ProtocolButton::R3,
            Button::View => ProtocolButton::View,
            Button::R4 => ProtocolButton::R4,
            Button::R5 => ProtocolButton::R5,
            Button::RightBumper => ProtocolButton::Rb,
            Button::DpadDown => ProtocolButton::DpadDown,
            Button::DpadRight => ProtocolButton::DpadRight,
            Button::DpadLeft => ProtocolButton::DpadLeft,
            Button::DpadUp => ProtocolButton::DpadUp,
            Button::Menu => ProtocolButton::Menu,
            Button::LeftStickClick => ProtocolButton::L3,
            Button::Steam => ProtocolButton::Steam,
            Button::L4 => ProtocolButton::L4,
            Button::L5 => ProtocolButton::L5,
            Button::LeftBumper => ProtocolButton::Lb,
            Button::RightStickTouch => ProtocolButton::RightStickTouch,
            Button::RightPadTouch => ProtocolButton::RightPadTouch,
            Button::RightPadClick => ProtocolButton::RightPadClick,
            Button::RightTriggerClick => ProtocolButton::RightTriggerClick,
            Button::LeftStickTouch => ProtocolButton::LeftStickTouch,
            Button::LeftPadTouch => ProtocolButton::LeftPadTouch,
            Button::LeftPadClick => ProtocolButton::LeftPadClick,
            Button::LeftTriggerClick => ProtocolButton::LeftTriggerClick,
            Button::RightGripTouch => ProtocolButton::RightGripTouch,
            Button::LeftGripTouch => ProtocolButton::LeftGripTouch,
        }),
        DigitalInput::Axis(threshold) => {
            let value = match threshold.axis {
                Axis::LeftStickX => state.left_stick[0],
                Axis::LeftStickY => state.left_stick[1],
                Axis::RightStickX => state.right_stick[0],
                Axis::RightStickY => state.right_stick[1],
                Axis::LeftTrigger => state.triggers[0],
                Axis::RightTrigger => state.triggers[1],
                Axis::LeftPadX if !state.left_pad.touched => return false,
                Axis::LeftPadY if !state.left_pad.touched => return false,
                Axis::RightPadX if !state.right_pad.touched => return false,
                Axis::RightPadY if !state.right_pad.touched => return false,
                Axis::LeftPadX => state.left_pad.position[0],
                Axis::LeftPadY => state.left_pad.position[1],
                Axis::RightPadX => state.right_pad.position[0],
                Axis::RightPadY => state.right_pad.position[1],
                Axis::GyroX => state.gyro[0],
                Axis::GyroY => state.gyro[1],
                Axis::GyroZ => state.gyro[2],
            };
            match threshold.direction {
                Direction::Positive => value >= threshold.threshold,
                Direction::Negative => value <= -threshold.threshold,
            }
        }
    }
}

fn analog_value(
    mapping: &AxisMapping,
    current: &ControllerState,
    previous: Option<&ControllerState>,
) -> AnalogValue {
    let sensitivity = mapping.sensitivity.unwrap_or(1.0);

    if mapping.target == AnalogTarget::Scroll {
        let pads = match mapping.source {
            AnalogSource::LeftPad => Some((current.left_pad, previous.map(|state| state.left_pad))),
            AnalogSource::RightPad => {
                Some((current.right_pad, previous.map(|state| state.right_pad)))
            }
            _ => None,
        };
        if let Some((pad, previous_pad)) = pads {
            if !pad.touched {
                return AnalogValue::Vector([0.0, 0.0]);
            }
            let Some(previous_pad) = previous_pad.filter(|pad| pad.touched) else {
                return AnalogValue::Vector([0.0, 0.0]);
            };
            if pad.position[0].hypot(pad.position[1]) < TRACKPAD_SCROLL_MIN_RADIUS
                || previous_pad.position[0].hypot(previous_pad.position[1])
                    < TRACKPAD_SCROLL_MIN_RADIUS
            {
                return AnalogValue::Vector([0.0, 0.0]);
            }

            let cross = previous_pad.position[0] * pad.position[1]
                - previous_pad.position[1] * pad.position[0];
            let dot = previous_pad.position[0] * pad.position[0]
                + previous_pad.position[1] * pad.position[1];
            let mut value = [0.0, cross.atan2(dot) * sensitivity];
            if mapping.swap_xy {
                value.swap(0, 1);
            }
            if mapping.invert_x {
                value[0] = -value[0];
            }
            if mapping.invert_y {
                value[1] = -value[1];
            }
            return AnalogValue::Vector(value);
        }
    }

    let deadzone = mapping.deadzone.unwrap_or(0.0);
    let exponent = match mapping.curve {
        Curve::Linear => 1.0,
        Curve::Exponential => mapping.exponent.unwrap_or(2.0),
    };
    let relative_gyro_seconds = if matches!(mapping.source, AnalogSource::Gyro)
        && matches!(
            mapping.target,
            AnalogTarget::MouseMotion | AnalogTarget::Scroll
        ) {
        previous
            .filter(|previous| previous.format == current.format)
            .and_then(|previous| {
                let delta_us = timestamp_delta_us(
                    current.format,
                    current.imu_timestamp_us,
                    previous.imu_timestamp_us,
                );
                (1..=100_000)
                    .contains(&delta_us)
                    .then_some(delta_us as f32 / 1_000_000.0)
            })
            .unwrap_or(0.0)
    } else {
        1.0
    };

    if matches!(
        mapping.source,
        AnalogSource::LeftTrigger | AnalogSource::RightTrigger
    ) {
        let mut value = match mapping.source {
            AnalogSource::LeftTrigger => current.triggers[0],
            AnalogSource::RightTrigger => current.triggers[1],
            _ => unreachable!(),
        };
        value = if value <= deadzone {
            0.0
        } else {
            (value - deadzone) / (1.0 - deadzone)
        };
        return AnalogValue::Scalar(value.powf(exponent) * sensitivity);
    }

    let mut value = match mapping.source {
        AnalogSource::LeftStick => current.left_stick,
        AnalogSource::RightStick => current.right_stick,
        AnalogSource::LeftPad => {
            if !current.left_pad.touched {
                [0.0, 0.0]
            } else if matches!(
                mapping.target,
                AnalogTarget::MouseMotion | AnalogTarget::Scroll
            ) {
                previous
                    .filter(|state| state.left_pad.touched)
                    .map_or([0.0, 0.0], |state| {
                        [
                            current.left_pad.position[0] - state.left_pad.position[0],
                            current.left_pad.position[1] - state.left_pad.position[1],
                        ]
                    })
            } else {
                current.left_pad.position
            }
        }
        AnalogSource::RightPad => {
            if !current.right_pad.touched {
                [0.0, 0.0]
            } else if matches!(
                mapping.target,
                AnalogTarget::MouseMotion | AnalogTarget::Scroll
            ) {
                previous
                    .filter(|state| state.right_pad.touched)
                    .map_or([0.0, 0.0], |state| {
                        [
                            current.right_pad.position[0] - state.right_pad.position[0],
                            current.right_pad.position[1] - state.right_pad.position[1],
                        ]
                    })
            } else {
                current.right_pad.position
            }
        }
        AnalogSource::Gyro => {
            let components = mapping
                .components
                .unwrap_or([AxisComponent::Z, AxisComponent::X]);
            components.map(|component| match component {
                AxisComponent::X => current.gyro[0],
                AxisComponent::Y => current.gyro[1],
                AxisComponent::Z => current.gyro[2],
            })
        }
        AnalogSource::LeftTrigger | AnalogSource::RightTrigger => unreachable!(),
    };

    if mapping.swap_xy {
        value.swap(0, 1);
    }
    if mapping.invert_x {
        value[0] = -value[0];
    }
    if mapping.invert_y {
        value[1] = -value[1];
    }

    let magnitude = value[0].hypot(value[1]);
    if magnitude <= deadzone {
        return AnalogValue::Vector([0.0, 0.0]);
    }
    let acceleration_gain =
        mapping
            .acceleration
            .filter(|value| *value > 0.0)
            .map_or(1.0, |acceleration| {
                previous
                    .filter(|previous| previous.format == current.format)
                    .and_then(|previous| {
                        let (current_timestamp_us, previous_timestamp_us) = match (
                            current.trackpad_timestamp_us,
                            previous.trackpad_timestamp_us,
                        ) {
                            (Some(current), Some(previous)) => (current, previous),
                            _ => (current.imu_timestamp_us, previous.imu_timestamp_us),
                        };
                        let delta_us = timestamp_delta_us(
                            current.format,
                            current_timestamp_us,
                            previous_timestamp_us,
                        );
                        (1..=100_000)
                            .contains(&delta_us)
                            .then_some(delta_us as f32 / 1_000_000.0)
                    })
                    .map_or(1.0, |seconds| {
                        let speed = magnitude / seconds;
                        1.0 + acceleration * (1.0 - (-(speed / 4.0).powi(2)).exp())
                    })
            });
    let scaled_magnitude = ((magnitude - deadzone) / (1.0 - deadzone)).powf(exponent)
        * sensitivity
        * acceleration_gain
        * relative_gyro_seconds;
    AnalogValue::Vector([
        value[0] / magnitude * scaled_magnitude,
        value[1] / magnitude * scaled_magnitude,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_x_is_global_ordered_consumed_and_edge_triggered() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "whatever"

                [[global.bindings]]
                chord = ["steam", "x"]
                activation = "press"
                consume = true
                action = { type = "event", name = "keyboard.toggle" }

                [modes.whatever]
                [[modes.whatever.bindings]]
                input = "steam"
                action = { type = "key", key = "super" }
                [[modes.whatever.bindings]]
                input = "x"
                action = { type = "key", key = "x" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        let steam = state_with(&[ProtocolButton::Steam]);
        assert_eq!(mapper.process(&steam), []);

        let steam_x = state_with(&[ProtocolButton::Steam, ProtocolButton::X]);
        assert_eq!(
            mapper.process(&steam_x),
            [Output::Event {
                name: "keyboard.toggle".to_owned()
            }]
        );
        assert_eq!(mapper.process(&steam_x), []);

        assert_eq!(mapper.process(&state_with(&[ProtocolButton::X])), []);
        assert_eq!(mapper.process(&ControllerState::default()), []);
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::X])),
            [Output::Key {
                key: KeyCode::KEY_X,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&ControllerState::default()),
            [Output::Key {
                key: KeyCode::KEY_X,
                pressed: false,
            }]
        );

        let x_first = state_with(&[ProtocolButton::X]);
        mapper.process(&x_first);
        let x_then_steam = state_with(&[ProtocolButton::X, ProtocolButton::Steam]);
        assert!(
            !mapper
                .process(&x_then_steam)
                .iter()
                .any(|output| matches!(output, Output::Event { .. }))
        );
    }

    #[test]
    fn mode_chord_consumes_later_base_binding() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [modes.desktop]
                [[modes.desktop.bindings]]
                chord = ["left-bumper", "dpad-up"]
                consume = true
                action = { type = "key", key = "page-up" }
                [[modes.desktop.bindings]]
                input = "dpad-up"
                action = { type = "key", key = "up" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        assert_eq!(mapper.process(&state_with(&[ProtocolButton::Lb])), []);
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::DpadUp])),
            [Output::Key {
                key: KeyCode::KEY_PAGEUP,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::DpadUp])),
            []
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb])),
            [Output::Key {
                key: KeyCode::KEY_PAGEUP,
                pressed: false,
            }]
        );
    }

    #[test]
    fn mode_capture_prevents_late_base_press_when_modifier_releases_first() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [modes.desktop]
                [[modes.desktop.bindings]]
                chord = ["left-bumper", "dpad-up"]
                consume = true
                action = { type = "key", key = "page-up" }
                [[modes.desktop.bindings]]
                input = "dpad-up"
                action = { type = "key", key = "up" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        mapper.process(&state_with(&[ProtocolButton::Lb]));
        mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::DpadUp]));
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::DpadUp])),
            [Output::Key {
                key: KeyCode::KEY_PAGEUP,
                pressed: false,
            }]
        );
        assert_eq!(mapper.process(&state_with(&[ProtocolButton::DpadUp])), []);
        assert_eq!(mapper.process(&ControllerState::default()), []);
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::DpadUp])),
            [Output::Key {
                key: KeyCode::KEY_UP,
                pressed: true,
            }]
        );
    }

    #[test]
    fn layer_overrides_faces_inherits_unspecified_inputs_and_orders_modifiers_first() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "l4"
                action = { type = "key", key = "super" }
                [[modes.desktop.bindings]]
                input = "left-bumper"
                action = { type = "key", key = "tab" }
                [[modes.desktop.bindings]]
                input = "a"
                action = { type = "key", key = "enter" }
                [[modes.desktop.bindings]]
                input = "b"
                action = { type = "key", key = "escape" }
                [[modes.desktop.bindings]]
                input = "y"
                action = { type = "key", key = "space" }

                [modes.desktop.layers.apps]
                hold = "left-bumper"
                [[modes.desktop.layers.apps.bindings]]
                input = "b"
                action = { type = "key", key = "q" }
                [[modes.desktop.layers.apps.bindings]]
                input = "y"
                action = { type = "key", key = "o" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone()).unwrap();

        assert_eq!(
            mapper.process(&state_with(&[
                ProtocolButton::L4,
                ProtocolButton::Lb,
                ProtocolButton::B,
            ])),
            [
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: true,
                },
                Output::Key {
                    key: KeyCode::KEY_Q,
                    pressed: true,
                },
            ]
        );
        assert_eq!(
            mapper.process(&state_with(&[
                ProtocolButton::L4,
                ProtocolButton::Lb,
                ProtocolButton::B,
                ProtocolButton::Y,
            ])),
            [Output::Key {
                key: KeyCode::KEY_O,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[
                ProtocolButton::L4,
                ProtocolButton::Lb,
                ProtocolButton::Y,
            ])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: false,
            }]
        );

        let mut mapper = Mapper::new(config).unwrap();
        assert_eq!(mapper.process(&state_with(&[ProtocolButton::Lb])), []);
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::A])),
            [Output::Key {
                key: KeyCode::KEY_ENTER,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb])),
            [Output::Key {
                key: KeyCode::KEY_ENTER,
                pressed: false,
            }]
        );
    }

    #[test]
    fn layer_route_is_latched_until_the_face_releases() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "b"
                action = { type = "key", key = "escape" }

                [modes.desktop.layers.apps]
                hold = "left-bumper"
                [[modes.desktop.layers.apps.bindings]]
                input = "b"
                action = { type = "key", key = "q" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::B])),
            [Output::Key {
                key: KeyCode::KEY_ESC,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::B])),
            []
        );
        assert_eq!(mapper.process(&state_with(&[ProtocolButton::B])), []);
        assert_eq!(
            mapper.process(&ControllerState::default()),
            [Output::Key {
                key: KeyCode::KEY_ESC,
                pressed: false,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::B])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: true,
            }]
        );
    }

    #[test]
    fn releasing_layer_hold_quarantines_the_face_until_release() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "b"
                action = { type = "key", key = "escape" }

                [modes.desktop.layers.apps]
                hold = "left-bumper"
                [[modes.desktop.layers.apps.bindings]]
                input = "b"
                action = { type = "key", key = "q" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::B])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::B])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: false,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::B])),
            []
        );
        assert_eq!(mapper.process(&state_with(&[ProtocolButton::B])), []);
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::B])),
            []
        );
        assert_eq!(mapper.process(&state_with(&[ProtocolButton::Lb])), []);
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::B])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: true,
            }]
        );
    }

    #[test]
    fn global_consuming_chord_has_priority_over_a_layer() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [[global.bindings]]
                chord = ["steam", "x"]
                consume = true
                action = { type = "event", name = "keyboard.toggle" }

                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "x"
                action = { type = "key", key = "g" }

                [modes.desktop.layers.apps]
                hold = "left-bumper"
                [[modes.desktop.layers.apps.bindings]]
                input = "x"
                action = { type = "key", key = "t" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        mapper.process(&state_with(&[ProtocolButton::Steam, ProtocolButton::Lb]));
        assert_eq!(
            mapper.process(&state_with(&[
                ProtocolButton::Steam,
                ProtocolButton::Lb,
                ProtocolButton::X,
            ])),
            [Output::Event {
                name: "keyboard.toggle".to_owned(),
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::X])),
            []
        );
        assert_eq!(mapper.process(&state_with(&[ProtocolButton::Lb])), []);
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::X])),
            [Output::Key {
                key: KeyCode::KEY_T,
                pressed: true,
            }]
        );
    }

    #[test]
    fn consuming_chord_can_reserve_a_layer_hold() {
        for scope in ["global", "modes.desktop"] {
            let config = Config::parse(&format!(
                r#"
                    version = 1
                    default_mode = "desktop"

                    [[{scope}.bindings]]
                    chord = ["steam", "left-bumper"]
                    consume = true
                    action = {{ type = "event", name = "hold.chord" }}

                    [modes.desktop.layers.apps]
                    hold = "left-bumper"
                    [[modes.desktop.layers.apps.bindings]]
                    input = "b"
                    action = {{ type = "key", key = "q" }}
                "#,
            ))
            .unwrap();
            let mut mapper = Mapper::new(config).unwrap();

            assert_eq!(mapper.process(&state_with(&[ProtocolButton::Steam])), []);
            assert_eq!(
                mapper.process(&state_with(&[
                    ProtocolButton::Steam,
                    ProtocolButton::Lb,
                    ProtocolButton::B,
                ])),
                [Output::Event {
                    name: "hold.chord".to_owned(),
                }]
            );
        }
    }

    #[test]
    fn simultaneous_layers_use_declaration_order_without_fallthrough() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "b"
                action = { type = "key", key = "escape" }

                [modes.desktop.layers.apps]
                hold = "left-bumper"
                [[modes.desktop.layers.apps.bindings]]
                input = "b"
                action = { type = "key", key = "q" }

                [modes.desktop.layers.navigation]
                hold = "right-bumper"
                [[modes.desktop.layers.navigation.bindings]]
                input = "b"
                action = { type = "key", key = "o" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        assert_eq!(
            mapper.process(&state_with(&[
                ProtocolButton::Lb,
                ProtocolButton::Rb,
                ProtocolButton::B,
            ])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Rb, ProtocolButton::B])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: false,
            }]
        );
        assert_eq!(mapper.process(&state_with(&[ProtocolButton::Rb])), []);
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Rb, ProtocolButton::B])),
            [Output::Key {
                key: KeyCode::KEY_O,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[
                ProtocolButton::Lb,
                ProtocolButton::Rb,
                ProtocolButton::B,
            ])),
            []
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::Rb])),
            [Output::Key {
                key: KeyCode::KEY_O,
                pressed: false,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[
                ProtocolButton::Lb,
                ProtocolButton::Rb,
                ProtocolButton::B,
            ])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: true,
            }]
        );
    }

    #[test]
    fn release_all_uses_reverse_press_order() {
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
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();
        let pressed = state_with(&[ProtocolButton::L4, ProtocolButton::Lb, ProtocolButton::B]);
        assert_eq!(
            mapper.process(&pressed),
            [
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: true,
                },
                Output::Key {
                    key: KeyCode::KEY_Q,
                    pressed: true,
                },
            ]
        );
        assert_eq!(
            mapper.release_all(),
            [
                Output::Key {
                    key: KeyCode::KEY_Q,
                    pressed: false,
                },
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: false,
                },
            ]
        );
    }

    #[test]
    fn sibling_consuming_chords_reuse_a_held_prefix() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"

                [modes.desktop]
                [[modes.desktop.bindings]]
                chord = ["left-bumper", "dpad-up"]
                consume = true
                action = { type = "key", key = "page-up" }
                [[modes.desktop.bindings]]
                chord = ["left-bumper", "dpad-down"]
                consume = true
                action = { type = "key", key = "page-down" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        assert_eq!(mapper.process(&state_with(&[ProtocolButton::Lb])), []);
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::DpadDown])),
            [Output::Key {
                key: KeyCode::KEY_PAGEDOWN,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb])),
            [Output::Key {
                key: KeyCode::KEY_PAGEDOWN,
                pressed: false,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::Lb, ProtocolButton::DpadUp])),
            [Output::Key {
                key: KeyCode::KEY_PAGEUP,
                pressed: true,
            }]
        );
    }

    #[test]
    fn axis_threshold_holds_and_releases_output() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.bindings]]
                input = { axis = "right-trigger", threshold = 0.5 }
                action = { type = "gamepad", button = "south" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();
        let mut state = ControllerState::default();
        state.triggers[1] = 0.75;

        assert_eq!(
            mapper.process(&state),
            [Output::GamepadButton {
                button: GamepadButton::South,
                pressed: true,
            }]
        );
        state.triggers[1] = 0.25;
        assert_eq!(
            mapper.process(&state),
            [Output::GamepadButton {
                button: GamepadButton::South,
                pressed: false,
            }]
        );
    }

    #[test]
    fn switching_modes_releases_outputs_and_uses_declaration_order() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "zebra"
                [modes.zebra]
                [[modes.zebra.bindings]]
                input = "a"
                action = { type = "key", key = "enter" }
                [modes.alpha]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::A])),
            [Output::Key {
                key: KeyCode::KEY_ENTER,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.next_mode(),
            [
                Output::Key {
                    key: KeyCode::KEY_ENTER,
                    pressed: false,
                },
                Output::ModeChanged {
                    name: "alpha".to_owned(),
                }
            ]
        );
        assert_eq!(mapper.active_mode(), "alpha");
    }

    #[test]
    fn reload_releases_outputs_and_falls_back_when_mode_disappears() {
        let first = Config::parse(
            r#"
                version = 1
                default_mode = "old"
                [modes.old]
                [[modes.old.bindings]]
                input = "a"
                action = { type = "mouse", button = "left" }
            "#,
        )
        .unwrap();
        let second = Config::parse(
            r#"
                version = 1
                default_mode = "new"
                [modes.new]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(first).unwrap();
        mapper.process(&state_with(&[ProtocolButton::A]));

        assert_eq!(
            mapper.reload(second).unwrap(),
            [
                Output::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                },
                Output::ModeChanged {
                    name: "new".to_owned(),
                }
            ]
        );
    }

    #[test]
    fn touchpad_mouse_mapping_uses_relative_motion_with_acceleration_off() {
        for acceleration in ["", "acceleration = 0.0"] {
            let config = Config::parse(&format!(
                r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "right-pad"
                target = "mouse-motion"
                sensitivity = 2.0
                {acceleration}
            "#
            ))
            .unwrap();
            let mut mapper = Mapper::new(config).unwrap();
            let mut state = ControllerState::default();
            state.right_pad.touched = true;
            state.right_pad.position = [0.25, 0.5];
            assert_eq!(mapper.process(&state), []);

            state.right_pad.position = [0.5, 0.5];
            assert_eq!(
                mapper.process(&state),
                [Output::MouseMotion { x: 0.5, y: 0.0 }]
            );
        }
    }

    #[test]
    fn touchpad_acceleration_is_report_rate_independent() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "right-pad"
                target = "mouse-motion"
                sensitivity = 2.0
                acceleration = 5.0
            "#,
        )
        .unwrap();
        let mut slow = Mapper::new(config.clone()).unwrap();
        let mut fast = Mapper::new(config).unwrap();
        let mut state = ControllerState::default();
        state.right_pad.touched = true;
        state.trackpad_timestamp_us = Some(0);
        assert_eq!(slow.process(&state), []);

        state.right_pad.position[0] = 0.08;
        state.trackpad_timestamp_us = Some(20_000);
        let slow_x = slow
            .process(&state)
            .into_iter()
            .find_map(|output| match output {
                Output::MouseMotion { x, y: 0.0 } => Some(x),
                _ => None,
            })
            .unwrap();

        state.right_pad.position[0] = 0.0;
        state.trackpad_timestamp_us = Some(0);
        assert_eq!(fast.process(&state), []);
        state.right_pad.position[0] = 0.04;
        state.trackpad_timestamp_us = Some(10_000);
        let fast_x_1 = fast
            .process(&state)
            .into_iter()
            .find_map(|output| match output {
                Output::MouseMotion { x, y: 0.0 } => Some(x),
                _ => None,
            })
            .unwrap();
        state.right_pad.position[0] = 0.08;
        state.trackpad_timestamp_us = Some(20_000);
        let fast_x_2 = fast
            .process(&state)
            .into_iter()
            .find_map(|output| match output {
                Output::MouseMotion { x, y: 0.0 } => Some(x),
                _ => None,
            })
            .unwrap();

        assert!((slow_x - fast_x_1 - fast_x_2).abs() < 1e-6);
        assert!(slow_x > 0.16);
    }

    #[test]
    fn touchpad_acceleration_handles_wrapping_and_invalid_time() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "right-pad"
                target = "mouse-motion"
                sensitivity = 2.0
                acceleration = 5.0
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone()).unwrap();
        let mut state = ControllerState::default();
        state.right_pad.touched = true;
        state.imu_timestamp_us = u32::MAX - 9_999;
        assert_eq!(mapper.process(&state), []);
        state.right_pad.position[0] = 0.04;
        state.imu_timestamp_us = 10_000;
        let x = mapper
            .process(&state)
            .into_iter()
            .find_map(|output| match output {
                Output::MouseMotion { x, y: 0.0 } => Some(x),
                _ => None,
            })
            .unwrap();
        let expected = 0.08 * (1.0 + 5.0 * (1.0 - (-0.25_f32).exp()));
        assert!((x - expected).abs() < 1e-6);

        let mut mapper = Mapper::new(config).unwrap();
        state = ControllerState::default();
        state.right_pad.touched = true;
        assert_eq!(mapper.process(&state), []);
        state.right_pad.position[0] = 0.04;
        state.imu_timestamp_us = 100_001;
        let x = mapper
            .process(&state)
            .into_iter()
            .find_map(|output| match output {
                Output::MouseMotion { x, y: 0.0 } => Some(x),
                _ => None,
            })
            .unwrap();
        assert!((x - 0.08).abs() < f32::EPSILON);
    }

    #[test]
    fn touchpad_scroll_keeps_direction_across_angle_wrap() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "left-pad"
                target = "scroll"
                sensitivity = 2.0
                deadzone = 0.95
                curve = "exponential"
                exponent = 3.0
                invert_y = true
            "#,
        )
        .unwrap();
        let mut counterclockwise = Mapper::new(config.clone()).unwrap();
        let mut state = ControllerState::default();
        state.left_pad.touched = true;
        let radians = 177.0_f32.to_radians();
        state.left_pad.position = [radians.cos(), radians.sin()];
        assert_eq!(counterclockwise.process(&state), []);

        for degrees in [-178.0_f32, -173.0] {
            let radians = degrees.to_radians();
            state.left_pad.position = [radians.cos(), radians.sin()];
            let outputs = counterclockwise.process(&state);
            let Some(Output::Scroll { x: 0.0, y }) = outputs
                .iter()
                .find(|output| matches!(output, Output::Scroll { .. }))
            else {
                panic!("expected vertical scroll, got {outputs:?}");
            };
            assert!((*y + 10.0_f32.to_radians()).abs() < 1e-6);
        }

        let mut clockwise = Mapper::new(config).unwrap();
        let radians = -177.0_f32.to_radians();
        state.left_pad.position = [radians.cos(), radians.sin()];
        assert_eq!(clockwise.process(&state), []);

        for degrees in [178.0_f32, 173.0] {
            let radians = degrees.to_radians();
            state.left_pad.position = [radians.cos(), radians.sin()];
            let outputs = clockwise.process(&state);
            let Some(Output::Scroll { x: 0.0, y }) = outputs
                .iter()
                .find(|output| matches!(output, Output::Scroll { .. }))
            else {
                panic!("expected vertical scroll, got {outputs:?}");
            };
            assert!((*y - 10.0_f32.to_radians()).abs() < 1e-6);
        }
    }

    #[test]
    fn touchpad_scroll_ignores_rotation_near_center() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "left-pad"
                target = "scroll"
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();
        let mut state = ControllerState::default();
        state.left_pad.touched = true;
        state.left_pad.position = [0.1, 0.0];
        assert_eq!(mapper.process(&state), []);

        state.left_pad.position = [0.0, 0.1];
        assert!(
            !mapper
                .process(&state)
                .iter()
                .any(|output| matches!(output, Output::Scroll { .. }))
        );
        state.left_pad.position = [0.0, 0.5];
        assert!(
            !mapper
                .process(&state)
                .iter()
                .any(|output| matches!(output, Output::Scroll { .. }))
        );

        let radians = 100.0_f32.to_radians();
        state.left_pad.position = [radians.cos() * 0.5, radians.sin() * 0.5];
        assert!(
            mapper
                .process(&state)
                .iter()
                .any(|output| matches!(output, Output::Scroll { y, .. } if *y > 0.0))
        );
    }

    #[test]
    fn trackpad_haptic_accumulates_slow_travel_and_resets_on_teleport() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "right-pad"
                target = "mouse-motion"
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();
        let mut state = ControllerState::default();
        state.right_pad.touched = true;
        mapper.process(&state);

        let mut ticked = false;
        for _ in 0..74 {
            state.right_pad.position[0] += 44.0 / 32767.0;
            state.imu_timestamp_us += 1_000;
            ticked |= mapper.process(&state).contains(&Output::TrackpadHaptic {
                pad: Trackpad::Right,
            });
        }
        assert!(ticked);

        state.right_pad.position[0] += 4001.0 / 32767.0;
        state.imu_timestamp_us += 30_000;
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
        state.right_pad.position[0] += 1700.0 / 32767.0;
        state.imu_timestamp_us += 30_000;
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
        state.right_pad.position[0] += 1700.0 / 32767.0;
        state.imu_timestamp_us += 30_000;
        assert!(mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
    }

    #[test]
    fn trackpad_haptic_rate_cap_uses_wrapping_controller_time() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "right-pad"
                target = "mouse-motion"
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone()).unwrap();
        let mut state = ControllerState::default();
        state.right_pad.touched = true;
        state.trackpad_timestamp_us = Some(u32::MAX - 10_000);
        mapper.process(&state);

        state.right_pad.position[0] += 3300.0 / 32767.0;
        state.trackpad_timestamp_us = Some(u32::MAX - 5_000);
        assert!(mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));

        state.right_pad.position[0] += 3300.0 / 32767.0;
        state.trackpad_timestamp_us = Some(5_000);
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));

        state.right_pad.position[0] += 100.0 / 32767.0;
        state.trackpad_timestamp_us = Some(20_000);
        assert!(mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
        state.right_pad.position[0] += 3000.0 / 32767.0;
        state.trackpad_timestamp_us = Some(20_001);
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));

        let mut mapper = Mapper::new(config).unwrap();
        state = ControllerState::default();
        state.format = StateFormat::Timestamp32Us;
        state.right_pad.touched = true;
        state.trackpad_timestamp_us = Some(u32::from(u16::MAX - 1000) * 32);
        mapper.process(&state);
        state.right_pad.position[0] += 3300.0 / 32767.0;
        state.trackpad_timestamp_us = Some(u32::from(u16::MAX - 500) * 32);
        assert!(mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
        state.right_pad.position[0] += 3300.0 / 32767.0;
        state.trackpad_timestamp_us = Some(0);
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
        state.right_pad.position[0] += 100.0 / 32767.0;
        state.trackpad_timestamp_us = Some(282 * 32);
        assert!(mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
        state.right_pad.position[0] += 3000.0 / 32767.0;
        state.trackpad_timestamp_us = Some(283 * 32);
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
    }

    #[test]
    fn trackpad_haptics_track_sides_independently() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "left-pad"
                target = "mouse-motion"
                [[modes.one.axes]]
                source = "right-pad"
                target = "scroll"
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();
        let mut state = ControllerState::default();
        state.left_pad.touched = true;
        state.right_pad.touched = true;
        mapper.process(&state);

        let mut left_ticked = false;
        for _ in 0..2 {
            state.left_pad.position[0] += 2000.0 / 32767.0;
            state.imu_timestamp_us += 30_000;
            let outputs = mapper.process(&state);
            left_ticked |= outputs.contains(&Output::TrackpadHaptic {
                pad: Trackpad::Left,
            });
            assert!(!outputs.contains(&Output::TrackpadHaptic {
                pad: Trackpad::Right,
            }));
        }
        assert!(left_ticked);

        let mut right_ticked = false;
        for _ in 0..2 {
            state.right_pad.position[1] += 2000.0 / 32767.0;
            state.imu_timestamp_us += 30_000;
            let outputs = mapper.process(&state);
            assert!(!outputs.contains(&Output::TrackpadHaptic {
                pad: Trackpad::Left,
            }));
            right_ticked |= outputs.contains(&Output::TrackpadHaptic {
                pad: Trackpad::Right,
            });
        }
        assert!(right_ticked);
    }

    #[test]
    fn trackpad_haptic_resets_on_touch_and_stays_on_when_unmapped() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "right-pad"
                target = "mouse-motion"
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();
        let mut state = ControllerState::default();
        state.right_pad.touched = true;
        mapper.process(&state);
        state.right_pad.position[0] += 2000.0 / 32767.0;
        state.imu_timestamp_us = 30_000;
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));

        state.right_pad.touched = false;
        mapper.process(&state);
        state.right_pad.touched = true;
        state.right_pad.position[0] = -0.5;
        state.imu_timestamp_us = 60_000;
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
        state.right_pad.position[0] += 1700.0 / 32767.0;
        state.imu_timestamp_us = 90_000;
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));

        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();
        state = ControllerState::default();
        state.right_pad.touched = true;
        mapper.process(&state);
        state.right_pad.position[0] += 1700.0 / 32767.0;
        state.imu_timestamp_us = 30_000;
        assert!(!mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
        state.right_pad.position[0] += 1700.0 / 32767.0;
        state.imu_timestamp_us = 60_000;
        assert!(mapper.process(&state).contains(&Output::TrackpadHaptic {
            pad: Trackpad::Right,
        }));
    }

    #[test]
    fn gyro_mouse_motion_is_report_rate_independent() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "gyro"
                target = "mouse-motion"
                sensitivity = 100.0
            "#,
        )
        .unwrap();
        let mut slow = Mapper::new(config.clone()).unwrap();
        let mut fast = Mapper::new(config).unwrap();
        let mut state = ControllerState {
            gyro: [0.0, 0.0, 1.0],
            ..ControllerState::default()
        };

        assert_eq!(slow.process(&state), []);
        state.imu_timestamp_us = 10_000;
        let slow_x = match slow.process(&state).as_slice() {
            [Output::MouseMotion { x, y: 0.0 }] => *x,
            _ => panic!("expected mouse motion"),
        };

        state.imu_timestamp_us = 0;
        assert_eq!(fast.process(&state), []);
        state.imu_timestamp_us = 5_000;
        let fast_x_1 = match fast.process(&state).as_slice() {
            [Output::MouseMotion { x, y: 0.0 }] => *x,
            _ => panic!("expected mouse motion"),
        };
        state.imu_timestamp_us = 10_000;
        let fast_x_2 = match fast.process(&state).as_slice() {
            [Output::MouseMotion { x, y: 0.0 }] => *x,
            _ => panic!("expected mouse motion"),
        };

        assert!((slow_x - (fast_x_1 + fast_x_2)).abs() < f32::EPSILON);
        assert!((slow_x - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn gyro_timestamps_wrap_without_a_motion_spike() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "gyro"
                target = "mouse-motion"
                sensitivity = 100.0
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone()).unwrap();
        let mut state = ControllerState {
            gyro: [0.0, 0.0, 1.0],
            imu_timestamp_us: u32::MAX - 4_999,
            ..ControllerState::default()
        };
        assert_eq!(mapper.process(&state), []);
        state.imu_timestamp_us = 5_000;
        let x = match mapper.process(&state).as_slice() {
            [Output::MouseMotion { x, y: 0.0 }] => *x,
            _ => panic!("expected mouse motion"),
        };
        assert!((x - 1.0).abs() < f32::EPSILON);

        let mut mapper = Mapper::new(config).unwrap();
        state.format = StateFormat::Timestamp32Us;
        state.imu_timestamp_us = u32::from(u16::MAX - 4) * 32;
        assert_eq!(mapper.process(&state), []);
        state.imu_timestamp_us = 5 * 32;
        let x = match mapper.process(&state).as_slice() {
            [Output::MouseMotion { x, y: 0.0 }] => *x,
            _ => panic!("expected mouse motion"),
        };
        assert!((x - 0.032).abs() < f32::EPSILON);
    }

    #[test]
    fn shared_outputs_are_reference_counted() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.bindings]]
                input = "a"
                action = { type = "key", key = "enter" }
                [[modes.one.bindings]]
                input = "b"
                action = { type = "key", key = "enter" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config).unwrap();

        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::A])),
            [Output::Key {
                key: KeyCode::KEY_ENTER,
                pressed: true,
            }]
        );
        assert_eq!(
            mapper.process(&state_with(&[ProtocolButton::A, ProtocolButton::B])),
            []
        );
        assert_eq!(mapper.process(&state_with(&[ProtocolButton::B])), []);
        assert_eq!(
            mapper.process(&ControllerState::default()),
            [Output::Key {
                key: KeyCode::KEY_ENTER,
                pressed: false,
            }]
        );
    }

    fn state_with(buttons: &[ProtocolButton]) -> ControllerState {
        let mut state = ControllerState::default();
        for button in buttons {
            state.buttons.insert(*button);
        }
        state
    }
}
