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
                .background(Color::from_rgba8(16, 19, 26, 255))
                .render(ui);
            blit::paint::Rectangle::new(LogicalRect {
                x: screen.width / 2.0 - 1.0,
                y: 0.0,
                width: 2.0,
                height: screen.height,
            })
            .background(Color::from_rgba8(80, 90, 108, 255))
            .render(ui);

            let text_options = TextOptions {
                horizontal_align: HorizontalAlign::Center,
                vertical_align: VerticalAlign::Center,
                ..Default::default()
            };
            keyboard.for_each_key(
                screen.width,
                screen.height,
                |slot, label, [x, y, width, height], active, special| {
                    let selected = keyboard.selected(slot.half) == Some(slot);
                    let pressed = selected && keyboard.pressed(slot.half);
                    let background = match (slot.half, pressed, selected, active, special) {
                        (Half::Left, true, _, _, _) => Color::from_rgba8(102, 151, 239, 255),
                        (Half::Right, true, _, _, _) => Color::from_rgba8(68, 194, 178, 255),
                        (Half::Left, false, true, _, _) => Color::from_rgba8(67, 113, 211, 255),
                        (Half::Right, false, true, _, _) => Color::from_rgba8(38, 155, 142, 255),
                        (_, false, false, true, _) => Color::from_rgba8(94, 78, 139, 255),
                        (_, false, false, false, true) => Color::from_rgba8(57, 66, 83, 255),
                        _ => Color::from_rgba8(43, 51, 65, 255),
                    };
                    Button::new(label)
                        .id(slot)
                        .background(background)
                        .uniform_radius(10.0)
                        .text_size(if label.len() > 3 { 17.0 } else { 24.0 })
                        .text_options(text_options)
                        .render(
                            ui,
                            LogicalRect {
                                x: x + 4.0,
                                y: y + 4.0,
                                width: (width - 8.0).max(0.0),
                                height: (height - 8.0).max(0.0),
                            },
                        );
                },
            );
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
