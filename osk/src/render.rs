use crate::keyboard::{Half, Keyboard};
use crate::theme::{Theme, ThemeColors};
use blit::{
    Easing, Frame, FrameInfo, Input, LogicalPoint, LogicalRect, Sides, Size, Widget, WidgetId,
};
use blit_cpu::{Argb8888, Renderer, RendererConfig, Scanline, VecBuffer};
use blit_gui::{
    FontData, FontFamily, GuiContext, TextConfig, TextSystem, Ui,
    atom::{Rectangle, Shadow},
    color::Color,
    layout::{Sizing, absolute, absolute::Anchor, flex, rect, single},
    style::{Border, BorderRadius},
    text::{HorizontalAlign, TextOptions, TextStyle, VerticalAlign},
    widget::Text,
};
use evdev::KeyCode;
use scd::{ControllerButton, Error, Result};
use std::time::{Duration, Instant};

const KEY_RADIUS: f32 = 1.0;
const BORDER_WIDTH: f32 = 2.0;

pub struct KeyboardRenderer {
    renderer: Renderer<VecBuffer<Argb8888>, Scanline>,
    frame: Frame<GuiContext>,
    context: GuiContext,
    scale: u32,
    theme: Theme,
    started_at: Instant,
}

#[derive(Clone, Copy)]
enum ControllerHint {
    Face(&'static str),
    Trigger(&'static str),
    Paddle(&'static str),
    Control(&'static str),
}

impl KeyboardRenderer {
    pub fn new(font: Box<[u8]>, theme: Theme, width: u32, height: u32, scale: u32) -> Result<Self> {
        let [physical_width, physical_height] = scaled_size(width, height, scale)?;
        let renderer = Renderer::new(
            VecBuffer::new(physical_width as usize, physical_height as usize),
            RendererConfig::default(),
        )
        .strategy(Scanline::default());
        let text = TextSystem::new(
            TextConfig {
                fonts: vec![FontFamily {
                    id: Default::default(),
                    fonts: vec![FontData::Shared(font.into())],
                }],
                text_cache_capacity: 1024 * 1024,
                layout_cache_capacity: 1024 * 1024,
            },
            blit_text_fontdue::Backend::new(),
        )
        .map_err(|error| Error::message(format_args!("could not load keyboard font: {error}")))?;
        let mut context = GuiContext::new(text);
        context.set_scale(scale as f32);
        Ok(Self {
            renderer,
            frame: Frame::default(),
            context,
            scale,
            theme,
            started_at: Instant::now(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32, scale: u32) -> Result<()> {
        let [physical_width, physical_height] = scaled_size(width, height, scale)?;
        let screen = self.renderer.screen();
        if [screen.width as u32, screen.height as u32] == [physical_width, physical_height]
            && self.scale == scale
        {
            return Ok(());
        }
        self.renderer
            .buffer_mut()
            .resize(physical_width as usize, physical_height as usize);
        self.scale = scale;
        self.context.set_scale(scale as f32);
        self.renderer.invalidate_all();
        Ok(())
    }

    pub fn render(&mut self, keyboard: &Keyboard) {
        let time = self.started_at.elapsed();
        let colors = &self.theme.colors;
        let [width, height] = self.physical_size();
        self.frame.build(
            &mut self.context,
            FrameInfo::new(Size::new(
                width as f32 / self.scale as f32,
                height as f32 / self.scale as f32,
            )),
            time,
            Input::None,
            |ui: Ui<'_>| {
                let screen = ui.screen();
                let mut root = ui.layout(rect::new());
                root.insert(
                    Rectangle::new()
                        .background(colors.background.color())
                        .border(Border::solid(BORDER_WIDTH, colors.border.color()))
                        .radius(BorderRadius::uniform(KEY_RADIUS)),
                );

                let bindings = keyboard.bindings();
                let grid = screen.inset(Sides::all(BORDER_WIDTH));
                let pointers = [Half::Left, Half::Right].map(|half| {
                    let inset_x = 19.0_f32.min(grid.width * 0.5);
                    let inset_y = 19.0_f32.min(grid.height * 0.5);
                    (
                        half,
                        keyboard
                            .pointer(half)
                            .and_then(|position| half.project(position))
                            .map(|[x, y]| LogicalPoint {
                                x: (grid.x + grid.width * x)
                                    .clamp(grid.x + inset_x, grid.x + grid.width - inset_x),
                                y: (grid.y + grid.height * y)
                                    .clamp(grid.y + inset_y, grid.y + grid.height - inset_y),
                            }),
                    )
                });

                keyboard.for_each_key(
                    grid.width,
                    grid.height,
                    |slot, primary, secondary, target, [x, y, width, height], active, special| {
                        let cell = LogicalRect {
                            x: grid.x + x,
                            y: grid.y + y,
                            width,
                            height,
                        };
                        let hovered = pointers.iter().any(|(_, pointer)| {
                            pointer.is_some_and(|pointer| cell.contains(pointer))
                        });
                        let pressed = pointers.iter().any(|(half, pointer)| {
                            pointer.is_some_and(|pointer| cell.contains(pointer))
                                && keyboard.pressed(*half)
                        });
                        let hint = target.and_then(|target| {
                            bindings
                                .iter()
                                .filter(|(_, configured)| {
                                    *configured == target
                                        || target == KeyCode::KEY_LEFTSHIFT
                                            && matches!(
                                                *configured,
                                                KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT
                                            )
                                })
                                .filter_map(|(input, _)| ControllerHint::from(input))
                                .min_by_key(|hint| match hint {
                                    ControllerHint::Face(_) => 0,
                                    ControllerHint::Trigger(_) => 1,
                                    ControllerHint::Paddle(_) => 2,
                                    ControllerHint::Control(_) => 3,
                                })
                        });
                        root.child()
                            .item(cell.inset(Sides::all(2.0)))
                            .widget_id(WidgetId::new(("key", slot)))
                            .build(KeyView {
                                primary,
                                secondary,
                                colors,
                                hint,
                                special,
                                hovered,
                                pressed,
                                active,
                                left_side: cell.x + cell.width * 0.5 < grid.x + grid.width * 0.5,
                            });
                    },
                );

                for (_, pointer) in pointers {
                    let Some(pointer) = pointer else {
                        continue;
                    };
                    for (diameter, color, opacity) in [
                        (35.0, colors.hint_control.color(), 0.84),
                        (24.0, colors.pressed.color(), 1.0),
                    ] {
                        root.child()
                            .item(LogicalRect::new(
                                pointer.x - diameter / 2.0,
                                pointer.y - diameter / 2.0,
                                diameter,
                                diameter,
                            ))
                            .insert(
                                Rectangle::new()
                                    .background(color)
                                    .radius(BorderRadius::uniform(diameter / 2.0))
                                    .opacity(opacity),
                            );
                    }
                }
            },
        );
        self.frame.layout(&mut self.context);
        self.frame.paint(&mut self.context);
        self.renderer.render(self.context.render_input());
        self.context.finish_frame();
    }

    pub fn animation_pending(&self) -> bool {
        self.frame.has_pending_redraw()
    }

    pub fn pixels(&self) -> &[Argb8888] {
        self.renderer.buffer().pixels()
    }

    pub fn physical_size(&self) -> [u32; 2] {
        let screen = self.renderer.screen();
        [screen.width as u32, screen.height as u32]
    }
}

fn scaled_size(width: u32, height: u32, scale: u32) -> Result<[u32; 2]> {
    if width == 0 || height == 0 || scale == 0 {
        return Err(Error::message("keyboard dimensions must be nonzero"));
    }
    Ok([
        width
            .checked_mul(scale)
            .ok_or_else(|| Error::message("keyboard width is too large"))?,
        height
            .checked_mul(scale)
            .ok_or_else(|| Error::message("keyboard height is too large"))?,
    ])
}

impl ControllerHint {
    fn from(input: ControllerButton) -> Option<Self> {
        Some(match input {
            ControllerButton::A => Self::Face("A"),
            ControllerButton::B => Self::Face("B"),
            ControllerButton::X => Self::Face("X"),
            ControllerButton::Y => Self::Face("Y"),
            ControllerButton::LeftTriggerClick => Self::Trigger("LT"),
            ControllerButton::RightTriggerClick => Self::Trigger("RT"),
            ControllerButton::L4 => Self::Paddle("L4"),
            ControllerButton::L5 => Self::Paddle("L5"),
            ControllerButton::R4 => Self::Paddle("R4"),
            ControllerButton::R5 => Self::Paddle("R5"),
            ControllerButton::DpadUp
            | ControllerButton::DpadDown
            | ControllerButton::DpadLeft
            | ControllerButton::DpadRight => return None,
            ControllerButton::Qam => Self::Control("QAM"),
            ControllerButton::R3 => Self::Control("R3"),
            ControllerButton::View => Self::Control("View"),
            ControllerButton::Rb => Self::Control("RB"),
            ControllerButton::Menu => Self::Control("Menu"),
            ControllerButton::L3 => Self::Control("L3"),
            ControllerButton::Steam => Self::Control("Steam"),
            ControllerButton::Lb => Self::Control("LB"),
            ControllerButton::RightStickTouch => Self::Control("RST"),
            ControllerButton::RightPadTouch => Self::Control("RPT"),
            ControllerButton::RightPadClick => Self::Control("RPC"),
            ControllerButton::LeftStickTouch => Self::Control("LST"),
            ControllerButton::LeftPadTouch => Self::Control("LPT"),
            ControllerButton::LeftPadClick => Self::Control("LPC"),
            ControllerButton::RightGripTouch => Self::Control("RGT"),
            ControllerButton::LeftGripTouch => Self::Control("LGT"),
        })
    }
}

struct KeyView<'a> {
    primary: &'static str,
    secondary: Option<&'static str>,
    colors: &'a ThemeColors,
    hint: Option<ControllerHint>,
    special: bool,
    hovered: bool,
    pressed: bool,
    active: bool,
    left_side: bool,
}

impl Widget<GuiContext> for KeyView<'_> {
    type Response = ();

    fn build(self, mut ui: Ui<'_>) {
        let colors = self.colors;
        let press = ui
            .animate(
                ui.current_widget_id(),
                f32::from(self.pressed || self.active),
                Duration::from_millis(if self.pressed || self.active { 45 } else { 110 }),
                Easing::EaseOutQuad,
            )
            .clamp(0.0, 1.0);
        let background = if self.hovered {
            colors.hover.color()
        } else if self.special && self.primary != "Space" {
            colors.special.color()
        } else {
            colors.key.color()
        };
        let pressed = colors.pressed.color();
        let [r, g, b, a] = [
            (background.red, pressed.red),
            (background.green, pressed.green),
            (background.blue, pressed.blue),
            (background.alpha, pressed.alpha),
        ]
        .map(|(from, to)| (from as f32 * (1.0 - press) + to as f32 * press).round() as u8);
        let text = if self.primary == "Space" {
            Color::TRANSPARENT
        } else if self.hovered && press < 0.35 {
            colors.key.color()
        } else if press >= 0.35 {
            colors.pressed_foreground.color()
        } else {
            colors.foreground.color()
        };
        let radius = BorderRadius::uniform(KEY_RADIUS);
        let centered = TextOptions::new()
            .horizontal_align(HorizontalAlign::Center)
            .vertical_align(VerticalAlign::Center);
        let mut key = ui.layout(flex::column());
        key.insert(
            Rectangle::new()
                .background(Color::rgba(r, g, b, a))
                .radius(radius),
        );
        if press > 0.0 {
            let mut shadow = colors.shadow.color();
            shadow.alpha = (shadow.alpha as f32 * press * (230.0 / 255.0)).round() as u8;
            key.insert(
                Shadow::new(shadow)
                    .radius(radius)
                    .offset(0.0, 8.0)
                    .blur(10.0)
                    .inset(true),
            );
        }
        if let Some(secondary) = self.secondary {
            key.child()
                .item(
                    flex::item()
                        .width(Sizing::grow())
                        .height(Sizing::percent(0.42)),
                )
                .insert(
                    Text::new(secondary)
                        .color(if self.hovered && !self.pressed && !self.active {
                            colors.dim.color()
                        } else {
                            colors.muted.color()
                        })
                        .style(TextStyle::new().size(14.0))
                        .options(centered),
                );
        }
        let mut label = key
            .child()
            .item(flex::item().grow())
            .layout(single::new().grow().padding(Sides::xy(
                if self.special { 10.0 } else { 2.0 },
                if self.secondary.is_some() { 7.0 } else { 2.0 },
            )));
        label.child().insert(
            Text::new(self.primary)
                .color(text)
                .style(TextStyle::new().size(if self.special { 17.0 } else { 22.0 }))
                .options(
                    centered
                        .horizontal_align(if !self.special || self.primary == "Space" {
                            HorizontalAlign::Center
                        } else if self.left_side {
                            HorizontalAlign::Left
                        } else {
                            HorizontalAlign::Right
                        })
                        .vertical_align(if self.secondary.is_some() {
                            VerticalAlign::Bottom
                        } else {
                            VerticalAlign::Center
                        }),
                ),
        );
        drop(label);
        if let Some(hint) = self.hint {
            let (label, background, text, width) = match hint {
                ControllerHint::Face(label) => (
                    label,
                    colors.pressed.color(),
                    colors.pressed_foreground.color(),
                    24.0,
                ),
                ControllerHint::Trigger(label) => {
                    (label, colors.hover.color(), colors.key.color(), 36.0)
                }
                ControllerHint::Paddle(label) => (
                    label,
                    colors.hint_paddle.color(),
                    colors.foreground.color(),
                    36.0,
                ),
                ControllerHint::Control(label) => (
                    label,
                    colors.hint_control.color(),
                    colors.foreground.color(),
                    44.0,
                ),
            };
            let face = matches!(hint, ControllerHint::Face(_));
            let left = self.primary == "Space" || !self.left_side;
            let anchor = if left { Anchor::Left } else { Anchor::Right };
            let mut badge = key.child().layout(
                absolute::place(single::new().grow())
                    .target_anchor(anchor)
                    .child_anchor(anchor)
                    .x(if left { 8.0 } else { -8.0 })
                    .width(absolute::Sizing::fixed(width))
                    .height(absolute::Sizing::fixed(24.0)),
            );
            badge.insert(
                Rectangle::new()
                    .background(background)
                    .border(Border::solid(
                        if face { 2.0 } else { 0.0 },
                        colors.key.color(),
                    ))
                    .radius(BorderRadius::uniform(if face { 12.0 } else { 4.0 })),
            );
            badge.child().insert(
                Text::new(label)
                    .color(text)
                    .style(TextStyle::new().size(11.0))
                    .options(centered),
            );
        }
    }
}
