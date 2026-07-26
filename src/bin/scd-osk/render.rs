use crate::keyboard::{Half, Keyboard};
use blit::{
    RepaintBuffer, Runtime,
    color::Color,
    geometry::{LogicalPoint, LogicalRect, LogicalSize, PhysicalRect},
    input::Input,
    keyboard::KeyboardRequest,
    paint::{HorizontalAlign, TextOptions, TextRequest, VerticalAlign},
    paint_list::PaintList,
    platform::PlatformImpl,
    resource::{ImageData, ImageId, StringData, StringId},
    widget::Button,
};
use blit_cpu::{Font, FontFace, Renderer, RendererConfig, Scanline, VecBuffer};
use scd::{Error, Result, ResultExt};
use std::time::Duration;

pub struct KeyboardRenderer {
    runtime: Runtime<BlitPlatform>,
    logical_size: [u32; 2],
    physical_size: [u32; 2],
    scale: u32,
}

struct BlitPlatform {
    renderer: Renderer<VecBuffer<u32>, Scanline>,
}

impl KeyboardRenderer {
    pub fn new(font: Box<[u8]>, width: u32, height: u32, scale: u32) -> Result<Self> {
        if width == 0 || height == 0 || scale == 0 {
            return Err(Error::message("keyboard dimensions must be nonzero"));
        }
        let physical_width = width
            .checked_mul(scale)
            .ok_or_else(|| Error::message("keyboard width is too large"))?;
        let physical_height = height
            .checked_mul(scale)
            .ok_or_else(|| Error::message("keyboard height is too large"))?;
        let renderer = Renderer::new(
            VecBuffer::new(physical_width as usize, physical_height as usize),
            RendererConfig {
                fonts: vec![FontFace {
                    id: Default::default(),
                    weight: 600,
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
            logical_size: [width, height],
            physical_size: [physical_width, physical_height],
            scale,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32, scale: u32) -> Result<()> {
        if [width, height] == self.logical_size && scale == self.scale {
            return Ok(());
        }
        if width == 0 || height == 0 || scale == 0 {
            return Err(Error::message("keyboard dimensions must be nonzero"));
        }
        let physical_width = width
            .checked_mul(scale)
            .ok_or_else(|| Error::message("keyboard width is too large"))?;
        let physical_height = height
            .checked_mul(scale)
            .ok_or_else(|| Error::message("keyboard height is too large"))?;
        let platform = self.runtime.platform();
        platform
            .renderer
            .buffer_mut()
            .resize(physical_width as usize, physical_height as usize);
        platform.renderer.set_scale_factor(scale as f32);
        self.logical_size = [width, height];
        self.physical_size = [physical_width, physical_height];
        self.scale = scale;
        self.runtime.refresh_screen();
        self.runtime.invalidate_all();
        Ok(())
    }

    pub fn render(&mut self, keyboard: &Keyboard) {
        self.runtime.render(Duration::ZERO, Input::None, |ui| {
            let screen = ui.screen();
            blit::paint::Rectangle::new(screen)
                .background(Color::from_rgba8(5, 7, 10, 255))
                .render(ui);

            let side_margin = (screen.width * 0.045).clamp(12.0, 96.0);
            let panel = LogicalRect {
                x: side_margin,
                y: 5.0,
                width: (screen.width - side_margin * 2.0).max(0.0),
                height: (screen.height - 10.0).max(0.0),
            };
            blit::paint::Rectangle::new(panel)
                .background(Color::from_rgba8(35, 38, 46, 255))
                .border(2.0, Color::from_rgba8(73, 78, 88, 255))
                .uniform_radius(5.0)
                .render(ui);

            let grid = LogicalRect {
                x: panel.x + 5.0,
                y: panel.y + 5.0,
                width: (panel.width - 10.0).max(0.0),
                height: (panel.height - 10.0).max(0.0),
            };
            let pointers = [Half::Left, Half::Right].map(|half| {
                (
                    half,
                    keyboard.pointer(half).map(|position| LogicalPoint {
                        x: grid.x
                            + grid.width
                                * ((position[0].clamp(-1.0, 1.0) + 1.0) * 0.25
                                    + if half == Half::Left { 0.0 } else { 0.5 })
                                .min(1.0 - f32::EPSILON),
                        y: grid.y
                            + grid.height
                                * ((position[1].clamp(-1.0, 1.0) + 1.0) * 0.5)
                                    .min(1.0 - f32::EPSILON),
                    }),
                )
            });

            keyboard.for_each_key(
                grid.width,
                grid.height,
                |slot, primary, secondary, [x, y, width, height], active, special| {
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
                    let background = if pressed || active {
                        Color::from_rgba8(26, 159, 255, 255)
                    } else if hovered {
                        Color::WHITE
                    } else if special && primary != "Space" {
                        Color::BLACK
                    } else {
                        Color::from_rgba8(14, 20, 27, 255)
                    };
                    let text = if hovered && !pressed && !active {
                        Color::from_rgba8(14, 20, 27, 255)
                    } else {
                        Color::WHITE
                    };
                    let key = LogicalRect {
                        x: cell.x + 2.0,
                        y: cell.y + 2.0,
                        width: (cell.width - 4.0).max(0.0),
                        height: (cell.height - 4.0).max(0.0),
                    };
                    let text_options = TextOptions {
                        horizontal_align: HorizontalAlign::Center,
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
                        .text_color(text)
                        .uniform_radius(1.0)
                        .padding_x(2.0)
                        .padding_y(if secondary.is_some() { 7.0 } else { 2.0 })
                        .text_size(if special { 17.0 } else { 22.0 })
                        .text_weight(600)
                        .text_options(text_options)
                        .render(ui, key);
                    if let Some(secondary) = secondary {
                        Button::new(secondary)
                            .id((slot, "secondary"))
                            .background(Color::TRANSPARENT)
                            .text_color(if hovered && !pressed && !active {
                                Color::from_rgba8(77, 82, 88, 255)
                            } else {
                                Color::from_rgba8(139, 146, 154, 255)
                            })
                            .padding_y(0.0)
                            .text_size(14.0)
                            .text_weight(600)
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
                .background(Color::from_rgba8(79, 79, 79, 255))
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
                .background(Color::from_rgba8(26, 159, 255, 255))
                .uniform_radius(center / 2.0)
                .opacity(0.72)
                .render(ui);
            }
        });
    }

    pub fn pixels(&mut self) -> &[u32] {
        self.runtime.platform().renderer.buffer().pixels()
    }

    pub fn physical_size(&self) -> [u32; 2] {
        self.physical_size
    }
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
