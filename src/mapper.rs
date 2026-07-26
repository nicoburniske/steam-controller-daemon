use evdev::KeyCode;
use indexmap::IndexMap;

use crate::{
    Error, Result,
    config::{
        Action, AnalogSource, AnalogTarget, AxisComponent, AxisMapping, Config, Curve,
        GamepadButton, MouseButton,
    },
    protocol::{Button, Buttons, ControllerState, StateFormat, TouchpadState, Trackpad},
};

const TRACKPAD_HAPTIC_MIN_TRAVEL: f32 = 45.0 / 32767.0;
const TRACKPAD_HAPTIC_MAX_TRAVEL: f32 = 4000.0 / 32767.0;
const TRACKPAD_HAPTIC_TICK_TRAVEL: f32 = 3200.0 / 32767.0;
const TRACKPAD_HAPTIC_MIN_INTERVAL_US: u32 = 25_000;
const TRACKPAD_SCROLL_MIN_RADIUS: f32 = 1.0 / 3.0;
const GAMEPAD_AXES: [GamepadAxis; 6] = [
    GamepadAxis::LeftX,
    GamepadAxis::LeftY,
    GamepadAxis::RightX,
    GamepadAxis::RightY,
    GamepadAxis::LeftTrigger,
    GamepadAxis::RightTrigger,
];

pub struct Mapper {
    config: Config,
    active_mode: usize,
    global: Vec<GlobalState>,
    routes: Vec<ActiveRoute>,
    quarantined: Buttons,
    held: IndexMap<HeldOutput, usize>,
    gamepad_axes: [f32; 6],
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
    KeyboardToggle,
    Event {
        name: String,
    },
    ModeChanged {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum GamepadAxis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftTrigger,
    RightTrigger,
}

impl Mapper {
    pub fn new(config: Config) -> Self {
        let active_mode = config
            .modes
            .get_index_of(&config.default_mode)
            .expect("parsed configuration has a valid default mode");
        let global = vec![GlobalState::default(); config.global.bindings.len()];
        Self {
            config,
            active_mode,
            global,
            routes: Vec::new(),
            quarantined: Buttons::default(),
            held: IndexMap::new(),
            gamepad_axes: [0.0; 6],
            left_pad_haptic: TrackpadHapticState::default(),
            right_pad_haptic: TrackpadHapticState::default(),
            previous: None,
        }
    }

    pub fn active_mode(&self) -> &str {
        self.config
            .modes
            .get_index(self.active_mode)
            .expect("active mode index is valid")
            .0
    }

    pub fn process(
        &mut self,
        state: &ControllerState,
        keyboard_visible: bool,
        outputs: &mut Vec<Output>,
    ) {
        let mut mode_changed = false;

        for index in 0..self.config.global.bindings.len() {
            let was_active = self.global[index].active;
            let chord = &self.config.global.bindings[index].chord;
            let all_active = chord.iter().all(|button| button_active(*button, state));
            let trigger = *chord.last().expect("validated chord is nonempty");
            let pressed = all_active
                && !self
                    .previous
                    .as_ref()
                    .is_some_and(|previous| button_active(trigger, previous))
                && chord[..chord.len() - 1].iter().all(|button| {
                    self.previous
                        .as_ref()
                        .is_some_and(|previous| button_active(*button, previous))
                });
            let active = if was_active { all_active } else { pressed };
            self.global[index].active = active;
            if pressed {
                self.global[index].captured = true;
            } else if self.global[index].captured
                && !button_active(
                    *self.config.global.bindings[index]
                        .chord
                        .last()
                        .expect("validated chord is nonempty"),
                    state,
                )
            {
                self.global[index].captured = false;
            }

            let action = &self.config.global.bindings[index].action;
            let should_apply = pressed
                || (was_active
                    && !active
                    && matches!(
                        action,
                        Action::Key { .. } | Action::Mouse { .. } | Action::Gamepad { .. }
                    ));
            let action = should_apply.then(|| action.clone());
            if let Some(action) = action {
                mode_changed |= self.apply_action(&action, active, outputs);
                if mode_changed {
                    break;
                }
            }
        }

        if !keyboard_visible && !mode_changed {
            self.quarantined.remove_inactive(state.buttons);

            let mut index = self.routes.len();
            while index != 0 {
                index -= 1;
                let route = &self.routes[index];
                let released = !button_active(route.input, state);
                let hold_lost = route.hold.is_some_and(|hold| {
                    !button_active(hold, state) || self.global_reserves(hold, state)
                });
                let captured = self.config.global.bindings.iter().zip(&self.global).any(
                    |(binding, runtime)| runtime.captured && binding.chord.contains(&route.input),
                );
                if !released && !hold_lost && !captured {
                    continue;
                }

                let route = self.routes.remove(index);
                if button_active(route.input, state) && !self.quarantined.contains(route.input) {
                    self.quarantined.insert(route.input);
                }
                self.apply_action(&route.action, false, outputs);
            }

            let binding_count = self.mode().bindings.len();
            for index in 0..binding_count {
                let input = self.mode().bindings[index].input;
                if !button_pressed(input, state, self.previous.as_ref())
                    || self.routes.iter().any(|route| route.input == input)
                    || self.quarantined.contains(input)
                    || self.global_reserves(input, state)
                {
                    continue;
                }
                let overridden = self.mode().layers.values().any(|layer| {
                    button_active(layer.hold, state)
                        && !self.global_reserves(layer.hold, state)
                        && layer.bindings.iter().any(|binding| binding.input == input)
                });
                if overridden {
                    continue;
                }

                let action = self.mode().bindings[index].action.clone();
                mode_changed |= self.apply_action(&action, true, outputs);
                if mode_changed {
                    break;
                }
                self.routes.push(ActiveRoute {
                    input,
                    hold: None,
                    action,
                });
            }
        }

        if !keyboard_visible && !mode_changed {
            let layer_count = self.mode().layers.len();
            for layer_index in 0..layer_count {
                let hold = self
                    .mode()
                    .layers
                    .get_index(layer_index)
                    .expect("layer index is valid")
                    .1
                    .hold;
                if !button_active(hold, state) || self.global_reserves(hold, state) {
                    continue;
                }
                let binding_count = self
                    .mode()
                    .layers
                    .get_index(layer_index)
                    .expect("layer index is valid")
                    .1
                    .bindings
                    .len();
                for binding_index in 0..binding_count {
                    let input = self
                        .mode()
                        .layers
                        .get_index(layer_index)
                        .expect("layer index is valid")
                        .1
                        .bindings[binding_index]
                        .input;
                    if !button_pressed(input, state, self.previous.as_ref())
                        || self.routes.iter().any(|route| route.input == input)
                        || self.quarantined.contains(input)
                        || self.global_reserves(input, state)
                    {
                        continue;
                    }

                    let action = self
                        .mode()
                        .layers
                        .get_index(layer_index)
                        .expect("layer index is valid")
                        .1
                        .bindings[binding_index]
                        .action
                        .clone();
                    mode_changed |= self.apply_action(&action, true, outputs);
                    if mode_changed {
                        break;
                    }
                    self.routes.push(ActiveRoute {
                        input,
                        hold: Some(hold),
                        action,
                    });
                }
                if mode_changed {
                    break;
                }
            }
        }

        if !keyboard_visible && !mode_changed {
            let mapping_count = self.mode().axes.len();
            for index in 0..mapping_count {
                let mapping = self.mode().axes[index];
                let value = analog_value(&mapping, state, self.previous.as_ref());
                match (mapping.target, value) {
                    (AnalogTarget::GamepadLeftStick, AnalogValue::Vector([x, y])) => {
                        self.set_gamepad_axis(GamepadAxis::LeftX, x, outputs);
                        self.set_gamepad_axis(GamepadAxis::LeftY, y, outputs);
                    }
                    (AnalogTarget::GamepadRightStick, AnalogValue::Vector([x, y])) => {
                        self.set_gamepad_axis(GamepadAxis::RightX, x, outputs);
                        self.set_gamepad_axis(GamepadAxis::RightY, y, outputs);
                    }
                    (AnalogTarget::GamepadLeftTrigger, AnalogValue::Scalar(value)) => {
                        self.set_gamepad_axis(GamepadAxis::LeftTrigger, value, outputs);
                    }
                    (AnalogTarget::GamepadRightTrigger, AnalogValue::Scalar(value)) => {
                        self.set_gamepad_axis(GamepadAxis::RightTrigger, value, outputs);
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
        }

        if !mode_changed {
            let timestamp_us = state
                .trackpad_timestamp_us
                .unwrap_or(state.imu_timestamp_us);
            if trackpad_haptic(
                &mut self.left_pad_haptic,
                state.left_pad,
                state.format,
                timestamp_us,
            ) {
                outputs.push(Output::TrackpadHaptic {
                    pad: Trackpad::Left,
                });
            }
            if trackpad_haptic(
                &mut self.right_pad_haptic,
                state.right_pad,
                state.format,
                timestamp_us,
            ) {
                outputs.push(Output::TrackpadHaptic {
                    pad: Trackpad::Right,
                });
            }
        }

        self.previous = Some(*state);
    }

    pub fn set_mode(&mut self, name: &str, outputs: &mut Vec<Output>) -> Result<()> {
        let Some(index) = self.config.modes.get_index_of(name) else {
            return Err(Error::message(format!("unknown mode {name:?}")));
        };
        self.switch_mode(index, outputs);
        Ok(())
    }

    pub fn next_mode(&mut self, outputs: &mut Vec<Output>) {
        let next = (self.active_mode + 1) % self.config.modes.len();
        self.switch_mode(next, outputs);
    }

    pub fn reload(&mut self, config: Config, outputs: &mut Vec<Output>) -> Result<()> {
        config.validate()?;
        let current = self.active_mode().to_owned();
        self.release_outputs(outputs);
        self.active_mode = config
            .modes
            .get_index_of(&current)
            .or_else(|| config.modes.get_index_of(&config.default_mode))
            .expect("validated configuration has a default mode");
        self.config = config;
        self.global = vec![GlobalState::default(); self.config.global.bindings.len()];
        self.left_pad_haptic = TrackpadHapticState::default();
        self.right_pad_haptic = TrackpadHapticState::default();
        outputs.push(Output::ModeChanged {
            name: self.active_mode().to_owned(),
        });
        Ok(())
    }

    pub fn release_all(&mut self, outputs: &mut Vec<Output>) {
        self.release_outputs(outputs);
        self.global.fill(GlobalState::default());
        self.left_pad_haptic = TrackpadHapticState::default();
        self.right_pad_haptic = TrackpadHapticState::default();
        self.previous = None;
    }

    pub fn suspend(&mut self, outputs: &mut Vec<Output>) {
        self.release_outputs(outputs);
    }

    fn mode(&self) -> &crate::config::Mode {
        self.config
            .modes
            .get_index(self.active_mode)
            .expect("active mode index is valid")
            .1
    }

    fn global_reserves(&self, input: Button, state: &ControllerState) -> bool {
        self.config
            .global
            .bindings
            .iter()
            .zip(&self.global)
            .any(|(binding, runtime)| {
                (runtime.captured && binding.chord.contains(&input))
                    || binding
                        .chord
                        .iter()
                        .take_while(|button| button_active(**button, state))
                        .any(|button| *button == input)
            })
    }

    fn apply_action(&mut self, action: &Action, active: bool, outputs: &mut Vec<Output>) -> bool {
        match action {
            Action::Key { key } => self.set_held(HeldOutput::Key(*key), active, outputs),
            Action::Mouse { button } => self.set_held(HeldOutput::Mouse(*button), active, outputs),
            Action::Gamepad { button } => {
                self.set_held(HeldOutput::Gamepad(*button), active, outputs)
            }
            Action::ModeSet { name } if active => {
                let index = self
                    .config
                    .modes
                    .get_index_of(name)
                    .expect("validated mode target exists");
                if index != self.active_mode {
                    self.switch_mode(index, outputs);
                    return true;
                }
            }
            Action::ModeNext if active => {
                let next = (self.active_mode + 1) % self.config.modes.len();
                if next != self.active_mode {
                    self.switch_mode(next, outputs);
                    return true;
                }
            }
            Action::KeyboardToggle if active => {
                outputs.push(Output::KeyboardToggle);
                return true;
            }
            Action::Event { name } if active => {
                outputs.push(Output::Event { name: name.clone() });
            }
            Action::ModeSet { .. }
            | Action::ModeNext
            | Action::KeyboardToggle
            | Action::Event { .. } => {}
        }
        false
    }

    fn switch_mode(&mut self, index: usize, outputs: &mut Vec<Output>) {
        if self.active_mode == index {
            return;
        }
        self.release_outputs(outputs);
        self.active_mode = index;
        self.global.fill(GlobalState::default());
        self.left_pad_haptic = TrackpadHapticState::default();
        self.right_pad_haptic = TrackpadHapticState::default();
        outputs.push(Output::ModeChanged {
            name: self.active_mode().to_owned(),
        });
    }

    fn set_held(&mut self, held: HeldOutput, active: bool, outputs: &mut Vec<Output>) {
        if active {
            if let Some(count) = self.held.get_mut(&held) {
                *count += 1;
                return;
            }
            outputs.push(held.output(true));
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
        outputs.push(held.output(false));
    }

    fn set_gamepad_axis(&mut self, axis: GamepadAxis, value: f32, outputs: &mut Vec<Output>) {
        if self.gamepad_axes[axis as usize] == value {
            return;
        }
        self.gamepad_axes[axis as usize] = value;
        outputs.push(Output::GamepadAxis { axis, value });
    }

    fn release_outputs(&mut self, outputs: &mut Vec<Output>) {
        self.routes.clear();
        self.quarantined = Buttons::default();
        for (held, _) in self.held.drain(..).rev() {
            outputs.push(held.output(false));
        }
        for axis in GAMEPAD_AXES {
            if self.gamepad_axes[axis as usize] != 0.0 {
                self.gamepad_axes[axis as usize] = 0.0;
                outputs.push(Output::GamepadAxis { axis, value: 0.0 });
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
struct GlobalState {
    active: bool,
    captured: bool,
}

struct ActiveRoute {
    input: Button,
    hold: Option<Button>,
    action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum HeldOutput {
    Key(KeyCode),
    Mouse(MouseButton),
    Gamepad(GamepadButton),
}

impl HeldOutput {
    fn output(self, pressed: bool) -> Output {
        match self {
            Self::Key(key) => Output::Key { key, pressed },
            Self::Mouse(button) => Output::MouseButton { button, pressed },
            Self::Gamepad(button) => Output::GamepadButton { button, pressed },
        }
    }
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

fn button_pressed(
    button: Button,
    current: &ControllerState,
    previous: Option<&ControllerState>,
) -> bool {
    button_active(button, current) && !previous.is_some_and(|state| button_active(button, state))
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

fn button_active(button: Button, state: &ControllerState) -> bool {
    state.buttons.contains(button)
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
    let relative_gyro_seconds = if mapping.source == AnalogSource::Gyro
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
    use crate::protocol::Button as ProtocolButton;

    #[test]
    fn global_chord_is_ordered_consuming_and_edge_triggered() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [[global.bindings]]
                chord = ["steam", "x"]
                action = { type = "keyboard-toggle" }
                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "steam"
                action = { type = "key", key = "super" }
                [[modes.desktop.bindings]]
                input = "x"
                action = { type = "key", key = "x" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone());

        assert_eq!(
            mapped(&mut mapper, &state_with(&[ProtocolButton::Steam])),
            []
        );
        let chord = state_with(&[ProtocolButton::Steam, ProtocolButton::X]);
        assert_eq!(mapped(&mut mapper, &chord), [Output::KeyboardToggle]);
        assert_eq!(mapped(&mut mapper, &chord), []);
        assert_eq!(mapped(&mut mapper, &state_with(&[ProtocolButton::X])), []);
        assert_eq!(mapped(&mut mapper, &ControllerState::default()), []);
        assert_eq!(
            mapped(&mut mapper, &state_with(&[ProtocolButton::X])),
            [Output::Key {
                key: KeyCode::KEY_X,
                pressed: true
            }]
        );

        let mut mapper = Mapper::new(config);
        assert_eq!(
            mapped(&mut mapper, &state_with(&[ProtocolButton::X])),
            [Output::Key {
                key: KeyCode::KEY_X,
                pressed: true
            }]
        );
        assert!(
            !mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::X, ProtocolButton::Steam])
            )
            .iter()
            .any(|output| matches!(output, Output::KeyboardToggle))
        );
    }

    #[test]
    fn visible_keyboard_suppresses_mode_outputs_but_keeps_global_toggle() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [[global.bindings]]
                chord = ["steam", "x"]
                action = { type = "keyboard-toggle" }
                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "a"
                action = { type = "key", key = "enter" }
                [[modes.desktop.axes]]
                source = "right-pad"
                target = "mouse-motion"
                sensitivity = 100.0
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config);

        assert_eq!(
            mapped(&mut mapper, &state_with(&[ProtocolButton::Steam])),
            []
        );
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Steam, ProtocolButton::X])
            ),
            [Output::KeyboardToggle]
        );

        let mut state = state_with(&[ProtocolButton::A]);
        state.right_pad.touched = true;
        state.right_pad.position = [0.1, 0.0];
        assert_eq!(keyboard_mapped(&mut mapper, &state), []);
        state.right_pad.position = [0.11, 0.0];
        assert_eq!(keyboard_mapped(&mut mapper, &state), []);
        assert_eq!(
            keyboard_mapped(&mut mapper, &state_with(&[ProtocolButton::Steam])),
            []
        );
        assert_eq!(
            keyboard_mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Steam, ProtocolButton::X])
            ),
            [Output::KeyboardToggle]
        );
    }

    #[test]
    fn keyboard_capture_does_not_repress_held_desktop_controls() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [[global.bindings]]
                chord = ["steam", "x"]
                action = { type = "keyboard-toggle" }
                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "l4"
                action = { type = "key", key = "super" }
                [[modes.desktop.bindings]]
                input = "a"
                action = { type = "key", key = "enter" }
                [[modes.desktop.bindings]]
                input = "left-pad-click"
                action = { type = "mouse", button = "left" }
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config);
        let held = [
            ProtocolButton::L4,
            ProtocolButton::A,
            ProtocolButton::LeftPadClick,
        ];

        assert_eq!(
            mapped(&mut mapper, &state_with(&held)),
            [
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: true,
                },
                Output::Key {
                    key: KeyCode::KEY_ENTER,
                    pressed: true,
                },
                Output::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
            ]
        );

        let mut with_steam = held.to_vec();
        with_steam.push(ProtocolButton::Steam);
        assert_eq!(mapped(&mut mapper, &state_with(&with_steam)), []);
        let mut with_toggle = with_steam.clone();
        with_toggle.push(ProtocolButton::X);
        assert_eq!(
            mapped(&mut mapper, &state_with(&with_toggle)),
            [Output::KeyboardToggle]
        );

        let mut outputs = Vec::new();
        mapper.suspend(&mut outputs);
        assert_eq!(
            outputs,
            [
                Output::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                },
                Output::Key {
                    key: KeyCode::KEY_ENTER,
                    pressed: false,
                },
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: false,
                },
            ]
        );

        assert_eq!(keyboard_mapped(&mut mapper, &state_with(&held)), []);
        assert_eq!(keyboard_mapped(&mut mapper, &state_with(&with_steam)), []);
        assert_eq!(
            keyboard_mapped(&mut mapper, &state_with(&with_toggle)),
            [Output::KeyboardToggle]
        );
        assert_eq!(mapped(&mut mapper, &state_with(&with_toggle)), []);
        assert_eq!(mapped(&mut mapper, &state_with(&held)), []);
        assert_eq!(mapped(&mut mapper, &ControllerState::default()), []);
        assert_eq!(
            mapped(&mut mapper, &state_with(&held)),
            [
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: true,
                },
                Output::Key {
                    key: KeyCode::KEY_ENTER,
                    pressed: true,
                },
                Output::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
            ]
        );
    }

    #[test]
    fn layer_overrides_faces_inherits_and_orders_modifiers_first() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [modes.desktop]
                [[modes.desktop.bindings]]
                input = "l4"
                action = { type = "key", key = "super" }
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
        let mut mapper = Mapper::new(config.clone());

        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::L4, ProtocolButton::Lb, ProtocolButton::B])
            ),
            [
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: true
                },
                Output::Key {
                    key: KeyCode::KEY_Q,
                    pressed: true
                }
            ]
        );
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[
                    ProtocolButton::L4,
                    ProtocolButton::Lb,
                    ProtocolButton::B,
                    ProtocolButton::Y,
                ])
            ),
            [Output::Key {
                key: KeyCode::KEY_O,
                pressed: true
            }]
        );

        let mut mapper = Mapper::new(config);
        assert_eq!(mapped(&mut mapper, &state_with(&[ProtocolButton::Lb])), []);
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Lb, ProtocolButton::A])
            ),
            [Output::Key {
                key: KeyCode::KEY_ENTER,
                pressed: true
            }]
        );
    }

    #[test]
    fn routes_latch_and_layer_loss_quarantines_until_release() {
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
        let mut mapper = Mapper::new(config.clone());

        assert_eq!(
            mapped(&mut mapper, &state_with(&[ProtocolButton::B])),
            [Output::Key {
                key: KeyCode::KEY_ESC,
                pressed: true
            }]
        );
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Lb, ProtocolButton::B])
            ),
            []
        );
        assert_eq!(mapped(&mut mapper, &state_with(&[ProtocolButton::B])), []);
        assert_eq!(
            mapped(&mut mapper, &ControllerState::default()),
            [Output::Key {
                key: KeyCode::KEY_ESC,
                pressed: false
            }]
        );

        let mut mapper = Mapper::new(config);
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Lb, ProtocolButton::Rb, ProtocolButton::B,])
            ),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: true
            }]
        );
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Rb, ProtocolButton::B])
            ),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: false
            }]
        );
        assert_eq!(mapped(&mut mapper, &state_with(&[ProtocolButton::Rb])), []);
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Rb, ProtocolButton::B])
            ),
            [Output::Key {
                key: KeyCode::KEY_O,
                pressed: true
            }]
        );
    }

    #[test]
    fn global_chord_has_priority_over_layers() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [[global.bindings]]
                chord = ["steam", "x"]
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
        let mut mapper = Mapper::new(config);

        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Steam, ProtocolButton::Lb])
            ),
            []
        );
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Steam, ProtocolButton::Lb, ProtocolButton::X,])
            ),
            [Output::Event {
                name: "keyboard.toggle".to_owned()
            }]
        );
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Lb, ProtocolButton::X])
            ),
            []
        );
        assert_eq!(mapped(&mut mapper, &state_with(&[ProtocolButton::Lb])), []);
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::Lb, ProtocolButton::X])
            ),
            [Output::Key {
                key: KeyCode::KEY_T,
                pressed: true
            }]
        );
    }

    #[test]
    fn lifecycle_releases_in_reverse_order_and_reference_counts() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.bindings]]
                input = "l4"
                action = { type = "key", key = "super" }
                [[modes.one.bindings]]
                input = "a"
                action = { type = "key", key = "enter" }
                [[modes.one.bindings]]
                input = "b"
                action = { type = "key", key = "enter" }
                [modes.two]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config);
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::L4, ProtocolButton::A])
            ),
            [
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: true
                },
                Output::Key {
                    key: KeyCode::KEY_ENTER,
                    pressed: true
                }
            ]
        );
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[ProtocolButton::L4, ProtocolButton::A, ProtocolButton::B])
            ),
            []
        );

        let mut outputs = Vec::new();
        mapper.release_all(&mut outputs);
        assert_eq!(
            outputs,
            [
                Output::Key {
                    key: KeyCode::KEY_ENTER,
                    pressed: false
                },
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: false
                }
            ]
        );

        mapped(&mut mapper, &state_with(&[ProtocolButton::A]));
        let mut outputs = Vec::new();
        mapper.next_mode(&mut outputs);
        assert_eq!(
            outputs,
            [
                Output::Key {
                    key: KeyCode::KEY_ENTER,
                    pressed: false
                },
                Output::ModeChanged {
                    name: "two".to_owned()
                }
            ]
        );
    }

    #[test]
    fn touchpad_motion_and_acceleration_are_report_rate_independent() {
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
        let mut slow = Mapper::new(config.clone());
        let mut fast = Mapper::new(config);
        let mut state = ControllerState::default();
        state.right_pad.touched = true;
        state.trackpad_timestamp_us = Some(0);
        assert_eq!(mapped(&mut slow, &state), []);

        state.right_pad.position[0] = 0.08;
        state.trackpad_timestamp_us = Some(20_000);
        let slow_x = mouse_x(mapped(&mut slow, &state));

        state.right_pad.position[0] = 0.0;
        state.trackpad_timestamp_us = Some(0);
        assert_eq!(mapped(&mut fast, &state), []);
        state.right_pad.position[0] = 0.04;
        state.trackpad_timestamp_us = Some(10_000);
        let fast_x_1 = mouse_x(mapped(&mut fast, &state));
        state.right_pad.position[0] = 0.08;
        state.trackpad_timestamp_us = Some(20_000);
        let fast_x_2 = mouse_x(mapped(&mut fast, &state));

        assert!((slow_x - fast_x_1 - fast_x_2).abs() < 1e-6);
        assert!(slow_x > 0.16);
    }

    #[test]
    fn circular_scroll_keeps_direction_across_wrap_and_ignores_center() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
                [[modes.one.axes]]
                source = "left-pad"
                target = "scroll"
                sensitivity = 2.0
                invert_y = true
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone());
        let mut state = ControllerState::default();
        state.left_pad.touched = true;
        let radians = 177.0_f32.to_radians();
        state.left_pad.position = [radians.cos(), radians.sin()];
        assert_eq!(mapped(&mut mapper, &state), []);
        let radians = -178.0_f32.to_radians();
        state.left_pad.position = [radians.cos(), radians.sin()];
        let scroll = mapped(&mut mapper, &state)
            .into_iter()
            .find_map(|output| match output {
                Output::Scroll { x: 0.0, y } => Some(y),
                _ => None,
            })
            .unwrap();
        assert!((scroll + 10.0_f32.to_radians()).abs() < 1e-6);

        let mut mapper = Mapper::new(config);
        state.left_pad.position = [0.1, 0.0];
        mapped(&mut mapper, &state);
        state.left_pad.position = [0.0, 0.5];
        assert!(
            !mapped(&mut mapper, &state)
                .iter()
                .any(|output| matches!(output, Output::Scroll { .. }))
        );
    }

    #[test]
    fn haptics_accumulate_rate_limit_wrap_and_work_without_mappings() {
        let mut haptic = TrackpadHapticState::default();
        let mut pad = TouchpadState {
            touched: true,
            ..TouchpadState::default()
        };
        assert!(!trackpad_haptic(&mut haptic, pad, StateFormat::Standard, 0));
        let mut ticked = false;
        for timestamp in 1..=74 {
            pad.position[0] += 44.0 / 32767.0;
            ticked |= trackpad_haptic(&mut haptic, pad, StateFormat::Standard, timestamp * 1_000);
        }
        assert!(ticked);

        let mut haptic = TrackpadHapticState::default();
        pad.position = [0.0, 0.0];
        trackpad_haptic(
            &mut haptic,
            pad,
            StateFormat::Timestamp32Us,
            u32::from(u16::MAX - 1000) * 32,
        );
        pad.position[0] += 3300.0 / 32767.0;
        assert!(trackpad_haptic(
            &mut haptic,
            pad,
            StateFormat::Timestamp32Us,
            u32::from(u16::MAX - 500) * 32,
        ));
        pad.position[0] += 3300.0 / 32767.0;
        assert!(!trackpad_haptic(
            &mut haptic,
            pad,
            StateFormat::Timestamp32Us,
            0,
        ));
        pad.position[0] += 100.0 / 32767.0;
        assert!(trackpad_haptic(
            &mut haptic,
            pad,
            StateFormat::Timestamp32Us,
            282 * 32,
        ));

        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [modes.one]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config);
        let mut state = ControllerState::default();
        state.right_pad.touched = true;
        mapped(&mut mapper, &state);
        state.right_pad.position[0] += 1700.0 / 32767.0;
        state.imu_timestamp_us = 30_000;
        mapped(&mut mapper, &state);
        state.right_pad.position[0] += 1700.0 / 32767.0;
        state.imu_timestamp_us = 60_000;
        assert!(
            mapped(&mut mapper, &state).contains(&Output::TrackpadHaptic {
                pad: Trackpad::Right
            })
        );
    }

    #[test]
    fn gyro_motion_uses_time_and_handles_timestamp_rollover() {
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
        let mut mapper = Mapper::new(config.clone());
        let mut state = ControllerState {
            gyro: [0.0, 0.0, 1.0],
            imu_timestamp_us: u32::MAX - 4_999,
            ..ControllerState::default()
        };
        assert_eq!(mapped(&mut mapper, &state), []);
        state.imu_timestamp_us = 5_000;
        assert!((mouse_x(mapped(&mut mapper, &state)) - 1.0).abs() < f32::EPSILON);

        let mut mapper = Mapper::new(config);
        state.format = StateFormat::Timestamp32Us;
        state.imu_timestamp_us = u32::from(u16::MAX - 4) * 32;
        assert_eq!(mapped(&mut mapper, &state), []);
        state.imu_timestamp_us = 5 * 32;
        assert!((mouse_x(mapped(&mut mapper, &state)) - 0.032).abs() < f32::EPSILON);
    }

    fn mapped(mapper: &mut Mapper, state: &ControllerState) -> Vec<Output> {
        let mut outputs = Vec::new();
        mapper.process(state, false, &mut outputs);
        outputs
    }

    fn keyboard_mapped(mapper: &mut Mapper, state: &ControllerState) -> Vec<Output> {
        let mut outputs = Vec::new();
        mapper.process(state, true, &mut outputs);
        outputs
    }

    fn mouse_x(outputs: Vec<Output>) -> f32 {
        outputs
            .into_iter()
            .find_map(|output| match output {
                Output::MouseMotion { x, y: 0.0 } => Some(x),
                _ => None,
            })
            .expect("mouse motion output")
    }

    fn state_with(buttons: &[ProtocolButton]) -> ControllerState {
        let mut state = ControllerState::default();
        for button in buttons {
            state.buttons.insert(*button);
        }
        state
    }
}
