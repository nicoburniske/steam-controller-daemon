use crate::keyboard::{Half, Keyboard};
use crate::theme::{Theme, ThemeColors};
use blit::{
    RepaintBuffer, Runtime,
    animation::Easing,
    color::Color,
    geometry::{LogicalInsets, LogicalPoint, LogicalRect, LogicalSize, PhysicalRect},
    input::Input,
    interact::WidgetId,
    keyboard::KeyboardRequest,
    paint::{BoxShadow, HorizontalAlign, TextOptions, TextRequest, VerticalAlign},
    paint_list::PaintList,
    platform::PlatformImpl,
    resource::{ImageData, ImageId, StringData, StringId},
    widget::Button,
};
use blit_cpu::{Argb8888, Font, FontFace, Renderer, RendererConfig, Scanline, VecBuffer};
use evdev::KeyCode;
use scd::{ControllerButton, Error, Result, ResultExt};
use std::hash::Hash;
use std::time::{Duration, Instant};

const KEY_RADIUS: f32 = 1.0;
const BORDER_WIDTH: f32 = 2.0;

pub struct KeyboardRenderer {
    runtime: Runtime<BlitPlatform>,
    theme: Theme,
    started_at: Instant,
}

struct BlitPlatform {
    renderer: Renderer<VecBuffer<Argb8888>, Scanline>,
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
            RendererConfig {
                fonts: vec![FontFace {
                    id: Default::default(),
                    weight: 400,
                    font: Font::from_owned(font).whence()?,
                }],
                font_metric_cache_capacity: 256,
                glyph_cache_capacity: 1024 * 1024,
                paragraph_cache_capacity: 1024 * 1024,
                shadow_cache_capacity: 1024 * 1024,
            },
        )
        .with_scale_factor(scale as f32)
        .strategy(Scanline::default());
        Ok(Self {
            runtime: Runtime::new(BlitPlatform { renderer }),
            theme,
            started_at: Instant::now(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32, scale: u32) -> Result<()> {
        let [physical_width, physical_height] = scaled_size(width, height, scale)?;
        let platform = self.runtime.platform();
        let screen = platform.renderer.screen();
        if [screen.width as u32, screen.height as u32] == [physical_width, physical_height]
            && platform.renderer.scale_factor() == scale as f32
        {
            return Ok(());
        }
        platform
            .renderer
            .buffer_mut()
            .resize(physical_width as usize, physical_height as usize);
        platform.renderer.set_scale_factor(scale as f32);
        self.runtime.refresh_screen();
        self.runtime.invalidate_all();
        Ok(())
    }

    pub fn render(&mut self, keyboard: &Keyboard) {
        let time = self.started_at.elapsed();
        let colors = &self.theme.colors;
        self.runtime.render(time, Input::None, |ui| {
            let screen = ui.screen();
            blit::paint::Rectangle::new(screen)
                .background(colors.background.color())
                .border(BORDER_WIDTH, colors.border.color())
                .uniform_radius(KEY_RADIUS)
                .replace(true)
                .render(ui);

            let bindings = keyboard.bindings();
            let grid = screen.inset(LogicalInsets::uniform(BORDER_WIDTH));
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
                        pointer.is_some_and(|pointer| cell.contains(pointer.x, pointer.y))
                    });
                    let pressed = pointers.iter().any(|(half, pointer)| {
                        pointer.is_some_and(|pointer| cell.contains(pointer.x, pointer.y))
                            && keyboard.pressed(*half)
                    });
                    let background = if hovered {
                        colors.hover.color()
                    } else if special && primary != "Space" {
                        colors.special.color()
                    } else {
                        colors.key.color()
                    };
                    let press = ui
                        .animate(
                            WidgetId::new(("key press", slot)),
                            f32::from(pressed || active),
                            if pressed || active {
                                Duration::from_millis(45)
                            } else {
                                Duration::from_millis(110)
                            },
                            Easing::EaseOutQuad,
                        )
                        .value()
                        .clamp(0.0, 1.0);
                    let pressed_color = colors.pressed.color();
                    let background = Color::from_rgba8(
                        (background.red as f32 * (1.0 - press) + pressed_color.red as f32 * press)
                            .round() as u8,
                        (background.green as f32 * (1.0 - press)
                            + pressed_color.green as f32 * press)
                            .round() as u8,
                        (background.blue as f32 * (1.0 - press) + pressed_color.blue as f32 * press)
                            .round() as u8,
                        (background.alpha as f32 * (1.0 - press)
                            + pressed_color.alpha as f32 * press)
                            .round() as u8,
                    );
                    let text = if hovered && press < 0.35 {
                        colors.key.color()
                    } else if press >= 0.35 {
                        colors.pressed_foreground.color()
                    } else {
                        colors.foreground.color()
                    };
                    let inset = 2.0;
                    let key = LogicalRect {
                        x: cell.x + inset,
                        y: cell.y + inset,
                        width: (cell.width - inset * 2.0).max(0.0),
                        height: (cell.height - inset * 2.0).max(0.0),
                    };
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
                            .filter_map(|(input, _)| {
                                ControllerHint::from(input).map(|hint| (input, hint))
                            })
                            .min_by_key(|(_, hint)| match hint {
                                ControllerHint::Face(_) => 0,
                                ControllerHint::Trigger(_) => 1,
                                ControllerHint::Paddle(_) => 2,
                                ControllerHint::Control(_) => 3,
                            })
                    });
                    let left_side = key.x + key.width * 0.5 < grid.x + grid.width * 0.5;
                    let text_options = TextOptions {
                        horizontal_align: if special && primary != "Space" {
                            if left_side {
                                HorizontalAlign::Left
                            } else {
                                HorizontalAlign::Right
                            }
                        } else {
                            HorizontalAlign::Center
                        },
                        vertical_align: if secondary.is_some() {
                            VerticalAlign::Bottom
                        } else {
                            VerticalAlign::Center
                        },
                        ..Default::default()
                    };
                    Button::new(primary)
                        .id(slot)
                        .background(background)
                        .text_color(if primary == "Space" {
                            Color::TRANSPARENT
                        } else {
                            text
                        })
                        .uniform_radius(KEY_RADIUS)
                        .padding_x(if special { 10.0 } else { 2.0 })
                        .padding_y(if secondary.is_some() { 7.0 } else { 2.0 })
                        .text_size(if special { 17.0 } else { 22.0 })
                        .text_options(text_options)
                        .render(ui, key);
                    if press > 0.0 {
                        let shadow = colors.shadow.color();
                        let mut clip = ui.begin_clip(key);
                        BoxShadow::new(
                            LogicalRect {
                                x: key.x - 10.0,
                                y: key.y - 8.0,
                                width: key.width + 20.0,
                                height: 10.0,
                            },
                            Color::from_rgba8(
                                shadow.red,
                                shadow.green,
                                shadow.blue,
                                (shadow.alpha as f32 * press * (230.0 / 255.0)).round() as u8,
                            ),
                        )
                        .blur(10.0)
                        .render(&mut clip);
                    }
                    if let Some(secondary) = secondary {
                        Button::new(secondary)
                            .id((slot, "secondary"))
                            .background(Color::TRANSPARENT)
                            .text_color(if hovered && !pressed && !active {
                                colors.dim.color()
                            } else {
                                colors.muted.color()
                            })
                            .padding_y(0.0)
                            .text_size(14.0)
                            .text_options(TextOptions {
                                horizontal_align: HorizontalAlign::Center,
                                vertical_align: VerticalAlign::Center,
                                ..Default::default()
                            })
                            .render(
                                ui,
                                LogicalRect {
                                    x: key.x,
                                    y: key.y + 1.0,
                                    width: key.width,
                                    height: key.height * 0.42,
                                },
                            );
                    }
                    if let Some((input, hint)) = hint {
                        let hint_width = hint.width();
                        render_hint(
                            ui,
                            colors,
                            hint,
                            (slot, input),
                            LogicalRect {
                                x: if primary == "Space" {
                                    key.x + 8.0
                                } else if left_side {
                                    key.x + key.width - hint_width - 8.0
                                } else {
                                    key.x + 8.0
                                },
                                y: key.y + (key.height - 24.0) * 0.5,
                                width: hint_width,
                                height: 24.0,
                            },
                        );
                    }
                },
            );

            for (half, pointer) in pointers {
                let Some(pointer) = pointer else {
                    continue;
                };
                let pressed = keyboard.pressed(half);
                let diameter = if pressed { 38.0 } else { 35.0 };
                blit::paint::Rectangle::new(LogicalRect {
                    x: pointer.x - diameter / 2.0,
                    y: pointer.y - diameter / 2.0,
                    width: diameter,
                    height: diameter,
                })
                .background(colors.hint_control.color())
                .uniform_radius(diameter / 2.0)
                .opacity(0.84)
                .render(ui);
                let center = if pressed { 27.0 } else { 24.0 };
                blit::paint::Rectangle::new(LogicalRect {
                    x: pointer.x - center / 2.0,
                    y: pointer.y - center / 2.0,
                    width: center,
                    height: center,
                })
                .background(colors.pressed.color())
                .uniform_radius(center / 2.0)
                .opacity(0.72)
                .render(ui);
            }
        });
    }

    pub fn animation_pending(&self) -> bool {
        self.runtime.has_pending_redraw()
    }

    pub fn pixels(&mut self) -> &[Argb8888] {
        self.runtime.platform().renderer.buffer().pixels()
    }

    pub fn physical_size(&mut self) -> [u32; 2] {
        let screen = self.runtime.platform().renderer.screen();
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

    fn width(self) -> f32 {
        match self {
            Self::Face(_) => 24.0,
            Self::Trigger(_) | Self::Paddle(_) => 36.0,
            Self::Control(_) => 44.0,
        }
    }
}

fn render_hint(
    ui: &mut blit::Ui,
    colors: &ThemeColors,
    hint: ControllerHint,
    id: impl Hash,
    area: LogicalRect,
) {
    let (label, background, text, radius, border) = match hint {
        ControllerHint::Face(label) => (
            label,
            colors.pressed.color(),
            colors.pressed_foreground.color(),
            12.0,
            2.0,
        ),
        ControllerHint::Trigger(label) => {
            (label, colors.hover.color(), colors.key.color(), 4.0, 0.0)
        }
        ControllerHint::Paddle(label) => (
            label,
            colors.hint_paddle.color(),
            colors.foreground.color(),
            4.0,
            0.0,
        ),
        ControllerHint::Control(label) => (
            label,
            colors.hint_control.color(),
            colors.foreground.color(),
            4.0,
            0.0,
        ),
    };
    Button::new(label)
        .id(id)
        .background(background)
        .border(border, colors.key.color())
        .text_color(text)
        .uniform_radius(radius)
        .padding_x(0.0)
        .padding_y(0.0)
        .text_size(11.0)
        .text_options(TextOptions {
            horizontal_align: HorizontalAlign::Center,
            vertical_align: VerticalAlign::Center,
            ..Default::default()
        })
        .render(ui, area);
}

impl PlatformImpl for BlitPlatform {
    fn render(&mut self, paint: &PaintList, damage: &[PhysicalRect]) {
        self.renderer.render(paint, damage);
    }

    fn screen(&mut self) -> PhysicalRect {
        self.renderer.screen()
    }

    fn scale_factor(&mut self) -> f32 {
        self.renderer.scale_factor()
    }

    fn repaint_buffer(&self) -> RepaintBuffer {
        RepaintBuffer::Reused
    }

    fn create_image(&mut self, data: ImageData) -> ImageId {
        self.renderer.create_image(data)
    }

    fn drop_image(&mut self, image: ImageId) {
        self.renderer.drop_image(image);
    }

    fn create_string(&mut self, string: StringData) -> StringId {
        self.renderer.create_string(string)
    }

    fn drop_string(&mut self, string: StringId) {
        self.renderer.drop_string(string);
    }

    fn string(&self, string: StringId) -> &str {
        self.renderer.string(string)
    }

    fn text_offset_at_position(&mut self, request: &TextRequest, position: LogicalPoint) -> usize {
        self.renderer.text_offset_at_position(request, position)
    }

    fn measure_text(&mut self, request: &TextRequest) -> LogicalSize {
        self.renderer.measure_text(request)
    }

    fn measure_text_height(&mut self, request: &TextRequest) -> f32 {
        self.renderer.measure_text_height(request)
    }

    fn text_cursor_rect(&mut self, request: &TextRequest, byte_offset: usize) -> LogicalRect {
        self.renderer.text_cursor_rect(request, byte_offset)
    }

    fn show_keyboard(&mut self, _: &KeyboardRequest<'_>) {}
}
