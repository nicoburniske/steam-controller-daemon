use evdev::KeyCode;
use indexmap::IndexMap;

use scd::{
    Error, Result,
    config::{
        Action, AxisActivation, AxisComponent, AxisMapping, AxisOptions, Binding, Config, Gamepad,
        GamepadButton, GlobalAction, ModeId, MouseButton, Stick, Trigger, VectorTarget,
    },
    protocol::{Button, Buttons, ControllerState, Haptic, StateFormat, TouchpadState, Trackpad},
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
    active_mode: ModeId,
    global: Vec<GlobalState>,
    routes: Vec<ActiveRoute>,
    quarantined: Buttons,
    osk_active: Buttons,
    held: IndexMap<HeldOutput, usize>,
    gamepad_axes: [f32; 6],
    axis_active: Vec<bool>,
    trackpad_haptics: [TrackpadHapticState; 2],
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
    ModeChanged {
        name: String,
        haptic: Option<Haptic>,
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
        let active_mode = config.default_mode;
        let global = vec![GlobalState::default(); config.global_bindings.len()];
        let axis_active = vec![false; config.mode(active_mode).axes.len()];
        Self {
            config,
            active_mode,
            global,
            routes: Vec::new(),
            quarantined: Buttons::default(),
            osk_active: Buttons::default(),
            held: IndexMap::new(),
            gamepad_axes: [0.0; 6],
            axis_active,
            trackpad_haptics: Default::default(),
            previous: None,
        }
    }

    pub fn active_mode(&self) -> &str {
        self.config.mode_name(self.active_mode)
    }

    pub fn gamepad(&self) -> Gamepad {
        self.mode().gamepad
    }

    pub fn gamepad_touchpad(&self) -> Option<Trackpad> {
        self.mode().gamepad_touchpad
    }

    pub fn process(
        &mut self,
        state: &ControllerState,
        keyboard_visible: bool,
        outputs: &mut Vec<Output>,
    ) {
        let previous = self.previous.replace(*state);
        let previous_buttons = previous.map_or(Buttons::default(), |state| state.buttons);

        // global bindings
        let mut reserved = Buttons::default();
        let mut captured = Buttons::default();
        for index in 0..self.config.global_bindings.len() {
            let binding = &self.config.global_bindings[index];
            let runtime = &mut self.global[index];
            let was_active = runtime.active;
            let trigger = binding.trigger;
            let mut prerequisites = binding.inputs;
            prerequisites.remove(trigger);
            let all_active =
                state.buttons.contains(trigger) && state.buttons.contains_all(prerequisites);
            let pressed = all_active
                && !previous_buttons.contains(trigger)
                && previous_buttons.contains_all(prerequisites);
            let active = if was_active { all_active } else { pressed };
            runtime.active = active;
            if pressed {
                runtime.captured = true;
            } else if runtime.captured && !state.buttons.contains(trigger) {
                runtime.captured = false;
            }

            for button in Button::ALL {
                if !binding.inputs.contains(button) {
                    continue;
                }
                if runtime.captured {
                    captured.insert(button);
                    reserved.insert(button);
                } else if (prerequisites.contains(button) && state.buttons.contains(button))
                    || (button == trigger && all_active)
                {
                    reserved.insert(button);
                }
            }

            let action = binding.action;
            let should_apply = pressed
                || (was_active
                    && !active
                    && matches!(action, GlobalAction::Action(action) if HeldOutput::from_action(action).is_some()));
            if should_apply {
                let interrupted = match action {
                    GlobalAction::Action(action) => self.apply_action(action, active, outputs).0,
                    GlobalAction::KeyboardToggle => {
                        outputs.push(Output::KeyboardToggle);
                        true
                    }
                };
                if interrupted {
                    return;
                }
            }
        }

        // osk bindings
        if keyboard_visible {
            for (&input, &key) in &self.config.osk_bindings {
                let was_active = self.osk_active.contains(input);
                let active = state.buttons.contains(input) && !reserved.contains(input);
                if active == was_active {
                    continue;
                }
                if active {
                    self.osk_active.insert(input);
                } else {
                    self.osk_active.remove(input);
                }
                set_held(&mut self.held, HeldOutput::Key(key), active, outputs);
            }
            self.process_trackpad_haptics(state, previous_buttons, outputs);
            return;
        }

        self.quarantined.remove_inactive(state.buttons);

        // route releases
        let mut index = self.routes.len();
        while index != 0 {
            index -= 1;
            let route = &self.routes[index];
            let released = !state.buttons.contains_all(route.inputs);
            let hold_lost = route
                .hold
                .is_some_and(|hold| !state.buttons.contains(hold) || reserved.contains(hold));
            let captured = Button::ALL
                .iter()
                .any(|button| route.inputs.contains(*button) && captured.contains(*button));
            if !released && !hold_lost && !captured {
                continue;
            }

            let route = self.routes.remove(index);
            if state.buttons.contains(route.trigger) {
                self.quarantined.insert(route.trigger);
            }
            if let Some(held) = route.held {
                set_held(&mut self.held, held, false, outputs);
            }
        }

        // layer overrides
        let mut overridden = Buttons::default();
        for layer in &self.mode().layers {
            if state.buttons.contains(layer.hold) && !reserved.contains(layer.hold) {
                for binding in &layer.bindings {
                    overridden.insert(binding.trigger);
                }
            }
        }

        let mut chord_reserved = reserved;
        for binding in &self.mode().bindings {
            if !overridden.contains(binding.trigger) {
                reserve_chord_inputs(*binding, state.buttons, &mut chord_reserved);
            }
        }
        for layer in &self.mode().layers {
            if !state.buttons.contains(layer.hold) || reserved.contains(layer.hold) {
                continue;
            }
            for binding in &layer.bindings {
                reserve_chord_inputs(*binding, state.buttons, &mut chord_reserved);
            }
        }

        let binding_count = self.mode().bindings.len();
        let layer_count = self.mode().layers.len();
        // chords consume buttons before scalar bindings
        for chord in [true, false] {
            let unavailable = if chord { reserved } else { chord_reserved };

            for index in 0..binding_count {
                let binding = self.mode().bindings[index];
                let mut prerequisites = binding.inputs;
                prerequisites.remove(binding.trigger);
                if (prerequisites != Buttons::default()) != chord
                    || overridden.contains(binding.trigger)
                {
                    continue;
                }
                if self.route_binding(
                    binding,
                    None,
                    state.buttons,
                    previous_buttons,
                    unavailable,
                    outputs,
                ) {
                    return;
                }
            }

            for layer_index in 0..layer_count {
                let hold = self.mode().layers[layer_index].hold;
                if !state.buttons.contains(hold) || reserved.contains(hold) {
                    continue;
                }
                let binding_count = self.mode().layers[layer_index].bindings.len();
                for binding_index in 0..binding_count {
                    let binding = self.mode().layers[layer_index].bindings[binding_index];
                    let mut prerequisites = binding.inputs;
                    prerequisites.remove(binding.trigger);
                    if (prerequisites != Buttons::default()) != chord {
                        continue;
                    }
                    if self.route_binding(
                        binding,
                        Some(hold),
                        state.buttons,
                        previous_buttons,
                        unavailable,
                        outputs,
                    ) {
                        return;
                    }
                }
            }
        }

        // axes
        let mut gamepad = [0.0; 6];
        let mapping_count = self.mode().axes.len();
        for index in 0..mapping_count {
            let was_active = self.axis_active[index];
            let mapping = &self.config.mode(self.active_mode).axes[index];
            let options = match mapping {
                AxisMapping::Trigger { options, .. } => *options,
                AxisMapping::Stick { options, .. }
                | AxisMapping::PadPosition { options, .. }
                | AxisMapping::PadMotion { options, .. }
                | AxisMapping::CircularScroll { options, .. }
                | AxisMapping::Gyro { options, .. } => options.axis,
            };
            // activation
            let active = options
                .activation
                .is_none_or(|activation| match activation {
                    AxisActivation::Trigger {
                        source,
                        engage,
                        release,
                    } => {
                        let value = trigger_value(source, state);
                        if was_active {
                            value > release
                        } else {
                            value >= engage
                        }
                    }
                    AxisActivation::Buttons(buttons) => state.buttons.contains_all(buttons),
                });
            self.axis_active[index] = active;
            if !active {
                continue;
            }

            // source
            let (value, source_scale, acceleration, target, vector_options) = match mapping {
                AxisMapping::Trigger {
                    source,
                    target,
                    options,
                } => {
                    let value = scale_axis(trigger_value(*source, state), *options);
                    gamepad[match target {
                        Trigger::Left => GamepadAxis::LeftTrigger,
                        Trigger::Right => GamepadAxis::RightTrigger,
                    } as usize] = value;
                    continue;
                }
                AxisMapping::Stick {
                    source,
                    target,
                    options,
                } => (
                    match source {
                        Stick::Left => state.left_stick,
                        Stick::Right => state.right_stick,
                    },
                    1.0,
                    0.0,
                    *target,
                    *options,
                ),
                AxisMapping::PadPosition {
                    pad,
                    target,
                    options,
                } => {
                    let pad = touchpad(*pad, state);
                    (
                        if pad.touched {
                            pad.position
                        } else {
                            [0.0, 0.0]
                        },
                        1.0,
                        0.0,
                        match target {
                            Stick::Left => VectorTarget::GamepadLeftStick,
                            Stick::Right => VectorTarget::GamepadRightStick,
                        },
                        *options,
                    )
                }
                AxisMapping::PadMotion {
                    pad,
                    options,
                    acceleration,
                } => {
                    let trackpad = *pad;
                    let pad = touchpad(trackpad, state);
                    let value = if !pad.touched {
                        [0.0, 0.0]
                    } else {
                        previous
                            .as_ref()
                            .map(|state| touchpad(trackpad, state))
                            .filter(|pad| pad.touched)
                            .map_or([0.0, 0.0], |previous_pad| {
                                [
                                    pad.position[0] - previous_pad.position[0],
                                    pad.position[1] - previous_pad.position[1],
                                ]
                            })
                    };
                    (
                        value,
                        1.0,
                        *acceleration,
                        VectorTarget::MouseMotion,
                        *options,
                    )
                }
                AxisMapping::CircularScroll { pad, options } => {
                    let trackpad = *pad;
                    let pad = touchpad(trackpad, state);
                    let value = previous
                        .as_ref()
                        .map(|state| touchpad(trackpad, state))
                        .filter(|previous_pad| {
                            pad.touched
                                && previous_pad.touched
                                && pad.position[0].hypot(pad.position[1])
                                    >= TRACKPAD_SCROLL_MIN_RADIUS
                                && previous_pad.position[0].hypot(previous_pad.position[1])
                                    >= TRACKPAD_SCROLL_MIN_RADIUS
                        })
                        .map_or([0.0, 0.0], |previous_pad| {
                            let cross = previous_pad.position[0] * pad.position[1]
                                - previous_pad.position[1] * pad.position[0];
                            let dot = previous_pad.position[0] * pad.position[0]
                                + previous_pad.position[1] * pad.position[1];
                            [0.0, cross.atan2(dot)]
                        });
                    (value, 1.0, 0.0, VectorTarget::Scroll, *options)
                }
                AxisMapping::Gyro {
                    components,
                    target,
                    options,
                } => {
                    let value = components.map(|component| match component {
                        AxisComponent::X => state.gyro[0],
                        AxisComponent::Y => state.gyro[1],
                        AxisComponent::Z => state.gyro[2],
                    });
                    let source_scale =
                        if matches!(target, VectorTarget::MouseMotion | VectorTarget::Scroll) {
                            frame_seconds(state, previous.as_ref(), false).unwrap_or(0.0)
                        } else {
                            1.0
                        };
                    (value, source_scale, 0.0, *target, *options)
                }
            };

            // transform
            let mut value = value;
            if vector_options.swap_xy {
                value.swap(0, 1);
            }
            if vector_options.invert_x {
                value[0] = -value[0];
            }
            if vector_options.invert_y {
                value[1] = -value[1];
            }
            let magnitude = value[0].hypot(value[1]);
            let value = if magnitude <= options.deadzone {
                [0.0, 0.0]
            } else {
                let acceleration_gain = if acceleration > 0.0 {
                    frame_seconds(state, previous.as_ref(), true).map_or(1.0, |seconds| {
                        let speed = magnitude / seconds;
                        1.0 + acceleration * (1.0 - (-(speed / 4.0).powi(2)).exp())
                    })
                } else {
                    1.0
                };
                let scaled_magnitude =
                    scale_axis(magnitude, options) * acceleration_gain * source_scale;
                [
                    value[0] / magnitude * scaled_magnitude,
                    value[1] / magnitude * scaled_magnitude,
                ]
            };

            // target
            let [x, y] = value;
            match target {
                VectorTarget::GamepadLeftStick => {
                    gamepad[GamepadAxis::LeftX as usize] += x;
                    gamepad[GamepadAxis::LeftY as usize] += y;
                }
                VectorTarget::GamepadRightStick => {
                    gamepad[GamepadAxis::RightX as usize] += x;
                    gamepad[GamepadAxis::RightY as usize] += y;
                }
                VectorTarget::MouseMotion if x != 0.0 || y != 0.0 => {
                    outputs.push(Output::MouseMotion { x, y });
                }
                VectorTarget::Scroll if x != 0.0 || y != 0.0 => {
                    outputs.push(Output::Scroll { x, y });
                }
                VectorTarget::MouseMotion | VectorTarget::Scroll => {}
            }
        }
        for axis in GAMEPAD_AXES {
            let value = gamepad[axis as usize];
            let value = if matches!(axis, GamepadAxis::LeftTrigger | GamepadAxis::RightTrigger) {
                value.clamp(0.0, 1.0)
            } else {
                value.clamp(-1.0, 1.0)
            };
            self.set_gamepad_axis(axis, value, outputs);
        }

        // trackpad haptics
        self.process_trackpad_haptics(state, previous_buttons, outputs);
    }

    pub fn set_mode(&mut self, name: &str, outputs: &mut Vec<Output>) -> Result<()> {
        let Some(mode) = self.config.mode_id(name) else {
            return Err(Error::message(format!("unknown mode {name:?}")));
        };
        self.switch_mode(mode, outputs);
        Ok(())
    }

    pub fn next_mode(&mut self, outputs: &mut Vec<Output>) {
        let next = self.config.next_mode(self.active_mode);
        self.switch_mode(next, outputs);
    }

    pub fn reload(&mut self, config: Config, outputs: &mut Vec<Output>) {
        let active_mode = config
            .mode_id(self.active_mode())
            .unwrap_or(config.default_mode);
        self.release_outputs(outputs);
        self.active_mode = active_mode;
        self.config = config;
        self.reset_runtime();
        outputs.push(self.mode_changed());
    }

    pub fn release_all(&mut self, outputs: &mut Vec<Output>) {
        self.release_outputs(outputs);
        self.reset_runtime();
        self.previous = None;
    }

    pub fn suspend(&mut self, outputs: &mut Vec<Output>) {
        self.release_outputs(outputs);
    }

    pub fn keyboard_shifted(&self) -> bool {
        self.config.osk_bindings.iter().any(|(input, key)| {
            self.osk_active.contains(*input)
                && matches!(*key, KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT)
        })
    }

    pub fn osk_bindings(&self) -> impl Iterator<Item = (Button, KeyCode)> + '_ {
        self.config
            .osk_bindings
            .iter()
            .map(|(input, key)| (*input, *key))
    }

    pub fn active_osk_bindings(&self) -> impl Iterator<Item = Button> + '_ {
        self.config
            .osk_bindings
            .keys()
            .copied()
            .filter(|input| self.osk_active.contains(*input))
    }

    fn mode(&self) -> &scd::config::Mode {
        self.config.mode(self.active_mode)
    }

    fn process_trackpad_haptics(
        &mut self,
        state: &ControllerState,
        previous_buttons: Buttons,
        outputs: &mut Vec<Output>,
    ) {
        let timestamp_us = state
            .trackpad_timestamp_us
            .unwrap_or(state.imu_timestamp_us);
        for (pad, pad_state, click) in [
            (Trackpad::Left, state.left_pad, Button::LeftPadClick),
            (Trackpad::Right, state.right_pad, Button::RightPadClick),
        ] {
            let click = button_pressed(click, state.buttons, previous_buttons);
            if click
                || trackpad_haptic(
                    &mut self.trackpad_haptics[pad as usize],
                    pad_state,
                    state.format,
                    timestamp_us,
                )
            {
                outputs.push(Output::TrackpadHaptic { pad });
            }
        }
    }

    fn route_binding(
        &mut self,
        binding: Binding,
        hold: Option<Button>,
        buttons: Buttons,
        previous_buttons: Buttons,
        reserved: Buttons,
        outputs: &mut Vec<Output>,
    ) -> bool {
        let mut prerequisites = binding.inputs;
        prerequisites.remove(binding.trigger);
        let unavailable = Button::ALL
            .iter()
            .any(|button| binding.inputs.contains(*button) && reserved.contains(*button));
        if !button_pressed(binding.trigger, buttons, previous_buttons)
            || !buttons.contains_all(binding.inputs)
            || !previous_buttons.contains_all(prerequisites)
            || self
                .routes
                .iter()
                .any(|route| route.trigger == binding.trigger)
            || self.quarantined.contains(binding.trigger)
            || unavailable
        {
            return false;
        }

        let (interrupted, held) = self.apply_action(binding.action, true, outputs);
        if !interrupted {
            self.routes.push(ActiveRoute {
                inputs: binding.inputs,
                trigger: binding.trigger,
                hold,
                held,
            });
        }
        interrupted
    }

    fn apply_action(
        &mut self,
        action: Action,
        active: bool,
        outputs: &mut Vec<Output>,
    ) -> (bool, Option<HeldOutput>) {
        if let Some(held) = HeldOutput::from_action(action) {
            set_held(&mut self.held, held, active, outputs);
            return (false, Some(held));
        }
        if !active {
            return (false, None);
        }
        let mode = match action {
            Action::ModeSet(mode) => mode,
            Action::ModeNext => self.config.next_mode(self.active_mode),
            _ => unreachable!("held actions returned above"),
        };
        if mode == self.active_mode {
            return (false, None);
        }
        self.switch_mode(mode, outputs);
        (true, None)
    }

    fn switch_mode(&mut self, mode: ModeId, outputs: &mut Vec<Output>) {
        if self.active_mode == mode {
            return;
        }
        self.release_outputs(outputs);
        self.active_mode = mode;
        self.reset_runtime();
        outputs.push(self.mode_changed());
    }

    fn reset_runtime(&mut self) {
        self.global
            .resize(self.config.global_bindings.len(), GlobalState::default());
        self.global.fill(GlobalState::default());
        self.axis_active.resize(self.mode().axes.len(), false);
        self.axis_active.fill(false);
        self.trackpad_haptics = Default::default();
    }

    fn set_gamepad_axis(&mut self, axis: GamepadAxis, value: f32, outputs: &mut Vec<Output>) {
        if self.gamepad_axes[axis as usize] == value {
            return;
        }
        self.gamepad_axes[axis as usize] = value;
        outputs.push(Output::GamepadAxis { axis, value });
    }

    fn mode_changed(&self) -> Output {
        Output::ModeChanged {
            name: self.active_mode().to_owned(),
            haptic: self.config.mode_switch_haptic,
        }
    }

    fn release_outputs(&mut self, outputs: &mut Vec<Output>) {
        self.routes.clear();
        self.quarantined = Buttons::default();
        self.osk_active = Buttons::default();
        self.axis_active.fill(false);
        for (held, _) in self.held.drain(..).rev() {
            outputs.push(held.output(false));
        }
        for axis in GAMEPAD_AXES {
            self.set_gamepad_axis(axis, 0.0, outputs);
        }
    }
}

fn set_held(
    held_outputs: &mut IndexMap<HeldOutput, usize>,
    held: HeldOutput,
    active: bool,
    outputs: &mut Vec<Output>,
) {
    if active {
        *held_outputs.entry(held).or_insert_with(|| {
            outputs.push(held.output(true));
            0
        }) += 1;
    } else {
        let released = held_outputs.get_mut(&held).is_some_and(|count| {
            *count -= 1;
            *count == 0
        });
        if released {
            held_outputs.shift_remove(&held);
            outputs.push(held.output(false));
        }
    }
}

fn trigger_value(trigger: Trigger, state: &ControllerState) -> f32 {
    match trigger {
        Trigger::Left => state.triggers[0],
        Trigger::Right => state.triggers[1],
    }
}

fn scale_axis(value: f32, options: AxisOptions) -> f32 {
    if value <= options.deadzone {
        0.0
    } else {
        ((value - options.deadzone) / (1.0 - options.deadzone)).powf(options.exponent)
            * options.sensitivity
    }
}

fn touchpad(trackpad: Trackpad, state: &ControllerState) -> TouchpadState {
    match trackpad {
        Trackpad::Left => state.left_pad,
        Trackpad::Right => state.right_pad,
    }
}

fn frame_seconds(
    current: &ControllerState,
    previous: Option<&ControllerState>,
    prefer_trackpad: bool,
) -> Option<f32> {
    let previous = previous.filter(|previous| previous.format == current.format)?;
    let (current_timestamp_us, previous_timestamp_us) = if prefer_trackpad {
        match (
            current.trackpad_timestamp_us,
            previous.trackpad_timestamp_us,
        ) {
            (Some(current), Some(previous)) => (current, previous),
            _ => (current.imu_timestamp_us, previous.imu_timestamp_us),
        }
    } else {
        (current.imu_timestamp_us, previous.imu_timestamp_us)
    };
    let delta_us = timestamp_delta_us(current.format, current_timestamp_us, previous_timestamp_us);
    (1..=100_000)
        .contains(&delta_us)
        .then_some(delta_us as f32 / 1_000_000.0)
}

#[derive(Clone, Copy, Default)]
struct GlobalState {
    active: bool,
    captured: bool,
}

struct ActiveRoute {
    inputs: Buttons,
    trigger: Button,
    hold: Option<Button>,
    held: Option<HeldOutput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum HeldOutput {
    Key(KeyCode),
    Mouse(MouseButton),
    Gamepad(GamepadButton),
}

impl HeldOutput {
    fn from_action(action: Action) -> Option<Self> {
        match action {
            Action::Key(key) => Some(Self::Key(key)),
            Action::Mouse(button) => Some(Self::Mouse(button)),
            Action::Gamepad(button) => Some(Self::Gamepad(button)),
            Action::ModeSet(_) | Action::ModeNext => None,
        }
    }

    fn output(self, pressed: bool) -> Output {
        match self {
            Self::Key(key) => Output::Key { key, pressed },
            Self::Mouse(button) => Output::MouseButton { button, pressed },
            Self::Gamepad(button) => Output::GamepadButton { button, pressed },
        }
    }
}

#[derive(Default)]
struct TrackpadHapticState {
    previous_position: Option<[f32; 2]>,
    progress: f32,
    last_tick: Option<(StateFormat, u32)>,
}

fn reserve_chord_inputs(binding: Binding, buttons: Buttons, reserved: &mut Buttons) {
    let mut prerequisites = binding.inputs;
    prerequisites.remove(binding.trigger);
    if prerequisites == Buttons::default() {
        return;
    }
    for button in Button::ALL {
        if prerequisites.contains(button) && buttons.contains(button) {
            reserved.insert(button);
        }
    }
    if buttons.contains_all(prerequisites) && buttons.contains(binding.trigger) {
        reserved.insert(binding.trigger);
    }
}

fn button_pressed(button: Button, current: Buttons, previous: Buttons) -> bool {
    current.contains(button) && !previous.contains(button)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_chord_is_ordered_consuming_and_edge_triggered() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [global]
                bind = [
                    { input = ["steam", "x"], type = "keyboard-toggle" },
                ]
                [mode.desktop]
                bind = [
                    { input = "steam", type = "key", key = "super" },
                    { input = "x", type = "key", key = "x" },
                ]
                [mode.desktop.layer.apps]
                hold = "left-bumper"
                bind = [
                    { input = "x", type = "key", key = "t" },
                ]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone());

        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::Steam, Button::Lb])),
            []
        );
        let chord = state_with(&[Button::Steam, Button::Lb, Button::X]);
        assert_eq!(mapped(&mut mapper, &chord), [Output::KeyboardToggle]);
        assert_eq!(mapped(&mut mapper, &chord), []);
        assert_eq!(mapped(&mut mapper, &state_with(&[Button::X])), []);
        assert_eq!(mapped(&mut mapper, &ControllerState::default()), []);
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::X])),
            [Output::Key {
                key: KeyCode::KEY_X,
                pressed: true
            }]
        );

        let mut mapper = Mapper::new(config);
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::X])),
            [Output::Key {
                key: KeyCode::KEY_X,
                pressed: true
            }]
        );
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::X, Button::Steam])),
            []
        );
    }

    #[test]
    fn mode_chord_consumes_fallback_and_releases_with_its_prerequisite() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [mode.desktop]
                bind = [
                    { input = ["left-bumper", "x"], type = "key", key = "q" },
                    { input = "x", type = "key", key = "x" },
                ]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config);

        assert_eq!(mapped(&mut mapper, &state_with(&[Button::Lb])), []);
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::Lb, Button::X])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: true,
            }]
        );
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::X])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: false,
            }]
        );
        assert_eq!(mapped(&mut mapper, &ControllerState::default()), []);
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::X])),
            [Output::Key {
                key: KeyCode::KEY_X,
                pressed: true,
            }]
        );
    }

    #[test]
    fn keyboard_capture_suppresses_mode_outputs_without_repressing_held_controls() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [osk.bind]
                x = "backspace"
                y = "space"
                [global]
                bind = [
                    { input = ["steam", "x"], type = "keyboard-toggle" },
                ]
                [mode.desktop]
                bind = [
                    { input = "a", type = "key", key = "enter" },
                ]
                [[mode.desktop.axis]]
                source = "right-pad"
                target = "mouse-motion"
                sensitivity = 100.0
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config);

        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::A])),
            [Output::Key {
                key: KeyCode::KEY_ENTER,
                pressed: true,
            }]
        );
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::A, Button::Steam])),
            []
        );
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[Button::A, Button::Steam, Button::X,])
            ),
            [Output::KeyboardToggle]
        );
        let mut outputs = Vec::new();
        mapper.suspend(&mut outputs);
        assert_eq!(
            outputs,
            [Output::Key {
                key: KeyCode::KEY_ENTER,
                pressed: false,
            }]
        );

        let mut state = state_with(&[Button::A]);
        state.right_pad.touched = true;
        state.right_pad.position = [0.1, 0.0];
        assert_eq!(keyboard_mapped(&mut mapper, &state), []);
        state.right_pad.position = [0.11, 0.0];
        assert_eq!(keyboard_mapped(&mut mapper, &state), []);
        assert_eq!(
            keyboard_mapped(&mut mapper, &state_with(&[Button::A, Button::Steam])),
            []
        );
        assert_eq!(
            keyboard_mapped(
                &mut mapper,
                &state_with(&[Button::A, Button::Steam, Button::X, Button::Y,])
            ),
            [Output::KeyboardToggle]
        );
        outputs.clear();
        mapper.suspend(&mut outputs);
        assert_eq!(outputs, []);
        assert_eq!(mapped(&mut mapper, &state_with(&[Button::A])), []);
        assert_eq!(mapped(&mut mapper, &ControllerState::default()), []);
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::A])),
            [Output::Key {
                key: KeyCode::KEY_ENTER,
                pressed: true,
            }]
        );
    }

    #[test]
    fn keyboard_controls_are_held_repeated_and_released_together() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [osk.bind]
                l4 = "shift"
                left-trigger-click = "shift"
                x = "backspace"
                [mode.desktop]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config);
        let held = state_with(&[Button::L4, Button::LeftTriggerClick, Button::X]);

        assert_eq!(
            keyboard_mapped(&mut mapper, &held),
            [
                Output::Key {
                    key: KeyCode::KEY_LEFTSHIFT,
                    pressed: true,
                },
                Output::Key {
                    key: KeyCode::KEY_BACKSPACE,
                    pressed: true,
                },
            ]
        );
        assert!(mapper.keyboard_shifted());
        assert!(
            mapper
                .active_osk_bindings()
                .any(|input| input == Button::L4)
        );
        assert_eq!(
            keyboard_mapped(
                &mut mapper,
                &state_with(&[Button::LeftTriggerClick, Button::X])
            ),
            []
        );

        let mut outputs = Vec::new();
        mapper.suspend(&mut outputs);
        assert_eq!(
            outputs,
            [
                Output::Key {
                    key: KeyCode::KEY_BACKSPACE,
                    pressed: false,
                },
                Output::Key {
                    key: KeyCode::KEY_LEFTSHIFT,
                    pressed: false,
                },
            ]
        );
        assert!(!mapper.keyboard_shifted());
        assert_eq!(mapper.active_osk_bindings().count(), 0);
    }

    #[test]
    fn layer_overrides_faces_inherits_and_orders_modifiers_first() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "desktop"
                [mode.desktop]
                bind = [
                    { input = "l4", type = "key", key = "super" },
                    { input = "a", type = "key", key = "enter" },
                    { input = "b", type = "key", key = "escape" },
                ]
                [mode.desktop.layer.apps]
                hold = "left-bumper"
                bind = [
                    { input = "b", type = "key", key = "q" },
                ]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone());

        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[Button::L4, Button::Lb, Button::B])
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
        let mut mapper = Mapper::new(config);
        assert_eq!(mapped(&mut mapper, &state_with(&[Button::Lb])), []);
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::Lb, Button::A])),
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
                [mode.desktop]
                bind = [
                    { input = "b", type = "key", key = "escape" },
                ]
                [mode.desktop.layer.apps]
                hold = "left-bumper"
                bind = [
                    { input = "b", type = "key", key = "q" },
                ]
                [mode.desktop.layer.navigation]
                hold = "right-bumper"
                bind = [
                    { input = "b", type = "key", key = "o" },
                ]
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone());

        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::B])),
            [Output::Key {
                key: KeyCode::KEY_ESC,
                pressed: true
            }]
        );
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::Lb, Button::B])),
            []
        );
        assert_eq!(mapped(&mut mapper, &state_with(&[Button::B])), []);
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
                &state_with(&[Button::Lb, Button::Rb, Button::B,])
            ),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: true
            }]
        );
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::Rb, Button::B])),
            [Output::Key {
                key: KeyCode::KEY_Q,
                pressed: false
            }]
        );
        assert_eq!(mapped(&mut mapper, &state_with(&[Button::Rb])), []);
        assert_eq!(
            mapped(&mut mapper, &state_with(&[Button::Rb, Button::B])),
            [Output::Key {
                key: KeyCode::KEY_O,
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
                [mode.one]
                gamepad = "xbox"
                bind = [
                    { input = "l4", type = "key", key = "super" },
                    { input = "a", type = "key", key = "enter" },
                    { input = "b", type = "key", key = "enter" },
                    { input = "x", type = "mouse", button = "left" },
                    { input = "y", type = "gamepad", button = "south" },
                ]
                [mode.two]
                gamepad = "none"
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config);
        assert_eq!(mapper.gamepad(), Gamepad::Xbox);
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[Button::L4, Button::A, Button::X, Button::Y])
            ),
            [
                Output::Key {
                    key: KeyCode::KEY_LEFTMETA,
                    pressed: true
                },
                Output::Key {
                    key: KeyCode::KEY_ENTER,
                    pressed: true
                },
                Output::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
                Output::GamepadButton {
                    button: GamepadButton::South,
                    pressed: true,
                }
            ]
        );
        assert_eq!(
            mapped(
                &mut mapper,
                &state_with(&[Button::L4, Button::A, Button::B, Button::X, Button::Y,])
            ),
            []
        );

        let mut outputs = Vec::new();
        mapper.release_all(&mut outputs);
        assert_eq!(
            outputs,
            [
                Output::GamepadButton {
                    button: GamepadButton::South,
                    pressed: false,
                },
                Output::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                },
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

        mapped(&mut mapper, &state_with(&[Button::A]));
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
                    name: "two".to_owned(),
                    haptic: None,
                }
            ]
        );
        assert_eq!(mapper.gamepad(), Gamepad::None);
    }

    #[test]
    fn touchpad_motion_and_acceleration_are_report_rate_independent() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [mode.one]
                [[mode.one.axis]]
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
    fn gyro_activation_is_hysteretic_and_adds_to_stick() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [mode.one]
                [[mode.one.axis]]
                source = "right-stick"
                target = "gamepad-right-stick"
                [[mode.one.axis]]
                source = "gyro"
                target = "gamepad-right-stick"
                activation = { source = "left-trigger", engage = 0.15, release = 0.10 }
                components = ["z", "x"]
                sensitivity = 0.5
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config);
        let mut state = ControllerState {
            right_stick: [0.25, 0.0],
            gyro: [0.0, 0.0, 0.4],
            ..ControllerState::default()
        };
        let right_x = |outputs: Vec<Output>| {
            outputs.into_iter().find_map(|output| match output {
                Output::GamepadAxis {
                    axis: GamepadAxis::RightX,
                    value,
                } => Some(value),
                _ => None,
            })
        };

        state.triggers[0] = 0.14;
        assert_eq!(right_x(mapped(&mut mapper, &state)), Some(0.25));

        state.triggers[0] = 0.15;
        assert!((right_x(mapped(&mut mapper, &state)).unwrap() - 0.45).abs() < 1e-6);

        state.triggers[0] = 0.11;
        assert_eq!(mapped(&mut mapper, &state), []);

        state.triggers[0] = 0.10;
        assert_eq!(right_x(mapped(&mut mapper, &state)), Some(0.25));
    }

    #[test]
    fn circular_scroll_keeps_direction_across_wrap_and_ignores_center() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [mode.one]
                [[mode.one.axis]]
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
                [mode.one]
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
                pad: Trackpad::Right,
            })
        );
    }

    #[test]
    fn gyro_motion_uses_time_and_handles_timestamp_rollover() {
        let config = Config::parse(
            r#"
                version = 1
                default_mode = "one"
                [mode.one]
                [[mode.one.axis]]
                source = "right-pad"
                target = "mouse-motion"
                [[mode.one.axis]]
                source = "gyro"
                target = "mouse-motion"
                sensitivity = 100.0
            "#,
        )
        .unwrap();
        let mut mapper = Mapper::new(config.clone());
        let mut state = ControllerState {
            gyro: [0.0, 1.0, 0.0],
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

    fn state_with(buttons: &[Button]) -> ControllerState {
        let mut state = ControllerState::default();
        for button in buttons {
            state.buttons.insert(*button);
        }
        state
    }
}
