use crate::{
    keyboard::{Keyboard, KeyboardOutput},
    render::KeyboardRenderer,
    theme::Theme,
};
use scd::{Client, OskState, Result, ResultExt};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, channel},
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Dispatch, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_output, wl_region, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use std::{path::PathBuf, sync::mpsc, thread, time::Duration};

pub fn run(socket: PathBuf, font: Box<[u8]>, theme: Theme, width: u32, height: u32) -> Result<()> {
    let connection = Connection::connect_to_env().whence()?;
    let (globals, event_queue) = registry_queue_init(&connection).whence()?;
    let qh = event_queue.handle();
    let mut event_loop = EventLoop::<State>::try_new().whence()?;
    WaylandSource::new(connection.clone(), event_queue)
        .insert(event_loop.handle())
        .whence()?;

    let compositor = CompositorState::bind(&globals, &qh).whence()?;
    let shell = LayerShell::bind(&globals, &qh).whence()?;
    let shm = Shm::bind(&globals, &qh).whence()?;
    let surface = compositor.create_surface(&qh);
    let layer = shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("scd-osk"), None);
    let input_region = compositor.wl_compositor().create_region(&qh, ());
    layer.set_input_region(Some(&input_region));
    input_region.destroy();

    let (input_sender, input) = channel::channel();
    thread::Builder::new()
        .name("scd-osk-input".into())
        .spawn({
            let socket = socket.clone();
            move || {
                let client = Client::new(socket);
                loop {
                    match client.osk() {
                        Ok(mut stream) => {
                            for state in &mut stream {
                                match state {
                                    Ok(state) => {
                                        if input_sender.send(Some(state)).is_err() {
                                            return;
                                        }
                                    }
                                    Err(error) => {
                                        log::warn!("keyboard input stream failed: {error}");
                                        break;
                                    }
                                }
                            }
                        }
                        Err(error) => log::warn!("could not connect keyboard input: {error}"),
                    }
                    if input_sender.send(None).is_err() {
                        return;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            }
        })
        .whence()?;

    let (key_output, keys) = mpsc::channel::<KeyboardOutput>();
    thread::Builder::new()
        .name("scd-osk-output".into())
        .spawn(move || {
            let client = Client::new(socket);
            for output in keys {
                let result = match output {
                    KeyboardOutput::Key {
                        code,
                        shift,
                        session,
                    } => client.key(code, shift, session),
                    KeyboardOutput::Hide { session } => client.hide_osk(session),
                };
                if let Err(error) = result {
                    log::warn!("keyboard output failed: {error}");
                }
            }
        })
        .whence()?;

    let pool = SlotPool::new(4, &shm).whence()?;
    let mut state = State {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm,
        qh,
        layer,
        pool,
        buffer: None,
        renderer: KeyboardRenderer::new(font, theme, width, height, 1)?,
        keyboard: Keyboard::default(),
        key_output,
        width,
        height,
        scale: 1,
        configured: false,
        frame_pending: false,
        redraw_pending: false,
        exit: false,
    };

    event_loop
        .handle()
        .insert_source(input, |event, _, state| match event {
            channel::Event::Msg(Some(input)) => state.input(input),
            channel::Event::Msg(None) => state.disconnected(),
            channel::Event::Closed => state.exit = true,
        })
        .whence()?;

    while !state.exit {
        event_loop.dispatch(None::<Duration>, &mut state).whence()?;
    }
    Ok(())
}

struct State {
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    qh: QueueHandle<Self>,
    layer: LayerSurface,
    pool: SlotPool,
    buffer: Option<Buffer>,
    renderer: KeyboardRenderer,
    keyboard: Keyboard,
    key_output: mpsc::Sender<KeyboardOutput>,
    width: u32,
    height: u32,
    scale: u32,
    configured: bool,
    frame_pending: bool,
    redraw_pending: bool,
    exit: bool,
}

impl State {
    fn input(&mut self, input: OskState) {
        let was_visible = self.keyboard.visible();
        let changed = self.keyboard.update(input, &self.key_output);
        match (was_visible, input.visible) {
            (false, true) => {
                if let Err(error) = self.show() {
                    log::error!("could not show keyboard: {error}");
                    self.exit = true;
                }
            }
            (true, false) => {
                self.hide();
            }
            (true, true) if changed => {
                self.redraw_pending = true;
                if let Err(error) = self.redraw() {
                    log::error!("could not redraw keyboard: {error}");
                    self.exit = true;
                }
            }
            _ => {}
        }
    }

    fn disconnected(&mut self) {
        let was_visible = self.keyboard.visible();
        self.keyboard.disconnect();
        if was_visible {
            self.hide();
        }
    }

    fn show(&mut self) -> Result<()> {
        self.layer.set_layer(Layer::Overlay);
        self.layer.set_anchor(Anchor::BOTTOM);
        self.layer.set_size(self.width, self.height);
        self.layer.set_exclusive_zone(0);
        self.layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        if self.layer.set_buffer_scale(self.scale).is_err() {
            return Err(scd::Error::message(
                "Wayland surface does not support buffer scaling",
            ));
        }
        self.configured = false;
        self.frame_pending = false;
        self.redraw_pending = true;
        self.layer.commit();
        Ok(())
    }

    fn hide(&mut self) {
        self.layer.wl_surface().attach(None, 0, 0);
        self.layer.commit();
        self.configured = false;
        self.frame_pending = false;
        self.redraw_pending = false;
    }

    fn redraw(&mut self) -> Result<()> {
        if !self.keyboard.visible()
            || !self.configured
            || self.frame_pending
            || !self.redraw_pending
        {
            return Ok(());
        }
        self.renderer.render(&self.keyboard);
        let [width, height] = self.renderer.physical_size();
        let stride = width as i32 * 4;
        if self.buffer.is_none() {
            self.buffer = Some(
                self.pool
                    .create_buffer(
                        width as i32,
                        height as i32,
                        stride,
                        wl_shm::Format::Argb8888,
                    )
                    .whence()?
                    .0,
            );
        }
        let Some(buffer) = self.buffer.as_mut() else {
            return Err(scd::Error::message(
                "could not allocate Wayland keyboard buffer",
            ));
        };
        let canvas = match self.pool.canvas(buffer) {
            Some(canvas) => canvas,
            None => {
                let (next, canvas) = self
                    .pool
                    .create_buffer(
                        width as i32,
                        height as i32,
                        stride,
                        wl_shm::Format::Argb8888,
                    )
                    .whence()?;
                *buffer = next;
                canvas
            }
        };
        let pixels = self.renderer.pixels();
        if canvas.len() != pixels.len() * 4 {
            return Err(scd::Error::message(
                "Wayland keyboard buffer has the wrong size",
            ));
        }
        for (target, pixel) in canvas.chunks_exact_mut(4).zip(pixels) {
            target.copy_from_slice(&pixel.raw().to_le_bytes());
        }

        self.layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        self.layer
            .wl_surface()
            .frame(&self.qh, FrameCallbackData(self.layer.wl_surface().clone()));
        buffer.attach_to(self.layer.wl_surface()).whence()?;
        self.layer.commit();
        self.frame_pending = true;
        self.redraw_pending = self.renderer.animation_pending();
        Ok(())
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if surface != self.layer.wl_surface() || factor <= 0 || factor as u32 == self.scale {
            return;
        }
        self.scale = factor as u32;
        self.buffer = None;
        self.redraw_pending = true;
        let result = if self.layer.set_buffer_scale(self.scale).is_err() {
            Err(scd::Error::message(
                "Wayland surface does not support buffer scaling",
            ))
        } else {
            self.renderer
                .resize(self.width, self.height, self.scale)
                .and_then(|()| self.redraw())
        };
        if let Err(error) = result {
            log::error!("could not scale keyboard: {error}");
            self.exit = true;
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if surface != self.layer.wl_surface() {
            return;
        }
        self.frame_pending = false;
        if let Err(error) = self.redraw() {
            log::error!("could not draw keyboard frame: {error}");
            self.exit = true;
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if !self.keyboard.visible() {
            return;
        }
        let width = if configure.new_size.0 == 0 {
            self.width
        } else {
            configure.new_size.0
        };
        let height = if configure.new_size.1 == 0 {
            self.height
        } else {
            configure.new_size.1
        };
        if [width, height] != [self.width, self.height] {
            self.width = width;
            self.height = height;
            self.buffer = None;
        }
        self.configured = true;
        self.redraw_pending = true;
        if let Err(error) = self
            .renderer
            .resize(self.width, self.height, self.scale)
            .and_then(|()| self.redraw())
        {
            log::error!("could not configure keyboard: {error}");
            self.exit = true;
        }
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(State);

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState];
}

impl Dispatch<wl_region::WlRegion, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_region::WlRegion,
        _: wl_region::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

smithay_client_toolkit::delegate_dispatch2!(State);
