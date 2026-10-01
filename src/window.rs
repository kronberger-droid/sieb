//! The Wayland side: a fullscreen overlay that dims the output and catches
//! clicks outside the panel, with the panel itself as a subsurface on top.

use std::error::Error;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            ping::{PingSource, make_ping},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Dispatch, QueueHandle, delegate_noop,
            globals::registry_queue_init,
            protocol::{
                wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_subsurface, wl_surface,
            },
        },
        protocols::wp::{
            cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                wp_fractional_scale_v1::{self, WpFractionalScaleV1},
            },
            viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler, cursor_shape::CursorShapeManager},
    },
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
    subcompositor::SubcompositorState,
};

use crate::matcher::Matcher;
use crate::picker::{Accept, Picker};
use crate::render::{self, View};
use crate::text::Text;

pub struct Options {
    pub prompt: Option<String>,
    pub lines: u32,
    pub index: bool,
    pub font: String,
}

/// How the window was closed.
pub enum Outcome {
    Cancel,
    /// Print this and exit 0.
    Accept(String),
}

/// Wakes the event loop when the matcher has new results.
pub struct Wake {
    pending: Arc<AtomicBool>,
    source: PingSource,
}

/// The notify callback for [`Matcher::new`] and the event source it wakes.
///
/// nucleo calls notify for every pushed line, so a plain ping would cost one
/// eventfd write per input line. The flag collapses those into one wakeup
/// until the UI has picked the results up.
pub fn wake() -> io::Result<(Wake, Arc<dyn Fn() + Send + Sync>)> {
    let (ping, source) = make_ping()?;
    let pending = Arc::new(AtomicBool::new(false));
    let flag = pending.clone();
    let notify = Arc::new(move || {
        if !flag.swap(true, Ordering::AcqRel) {
            ping.ping();
        }
    });
    Ok((Wake { pending, source }, notify))
}

pub fn run(options: Options, matcher: Matcher, wake: Wake) -> Result<Outcome, Box<dyn Error>> {
    let text = Text::load(&options.font)?;

    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;

    let compositor = CompositorState::bind(&globals, &qh)?;
    let subcompositor =
        SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    let viewporter: WpViewporter = globals.bind(&qh, 1..=1, ())?;
    // Optional: without it we fall back to the integer scale.
    let fractional_manager: Option<WpFractionalScaleManagerV1> =
        globals.bind(&qh, 1..=1, ()).ok();

    // The backdrop covers the whole output, bars included, and takes the
    // keyboard. Every click outside the panel lands on it.
    let backdrop = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, backdrop, Layer::Overlay, Some("sieb"), None);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    let backdrop_viewport = viewporter.get_viewport(layer.wl_surface(), &qh, ());
    // Requested on the backdrop, since the panel always shares its output.
    let fractional = fractional_manager
        .as_ref()
        .map(|m| m.get_fractional_scale(layer.wl_surface(), &qh, ()));

    let (panel_subsurface, panel) =
        subcompositor.create_subsurface(layer.wl_surface().clone(), &qh);
    let panel_viewport = viewporter.get_viewport(&panel, &qh, ());

    // Initial commit without a buffer: the compositor answers with a
    // configure carrying the output size, and the first frame follows that.
    layer.commit();

    let panel_size = render::panel_size(options.lines);
    // Room for two buffers at scale 2 before the pool has to grow.
    let pool = SlotPool::new(panel_size.0 as usize * panel_size.1 as usize * 4 * 8, &shm)?;
    let mut app = App {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        shm,
        pool,
        loop_handle: event_loop.handle(),
        qh: qh.clone(),
        cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),

        layer,
        backdrop_viewport,
        backdrop_buffer: None,
        panel,
        panel_subsurface,
        panel_viewport,
        panel_buffer: None,
        panel_size,
        fractional,

        picker: Picker::new(options.lines),
        options,
        matcher,
        text,
        frame_pending: false,
        dirty: false,

        size: None,
        scale: 1.0,
        keyboard: None,
        pointer: None,
        shape_device: None,
        modifiers: Modifiers::default(),
        outcome: None,
    };

    let pending = wake.pending;
    event_loop
        .handle()
        .insert_source(wake.source, move |(), _, app: &mut App| {
            // Clear before ticking, so results landing during the tick ping
            // again instead of getting lost.
            pending.store(false, Ordering::Release);
            app.refresh();
        })
        .map_err(|err| err.error)?;
    WaylandSource::new(conn, event_queue).insert(event_loop.handle())?;
    while app.outcome.is_none() {
        event_loop.dispatch(None, &mut app)?;
    }
    Ok(app.outcome.take().expect("loop exits with an outcome"))
}

struct App {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    loop_handle: LoopHandle<'static, App>,
    qh: QueueHandle<App>,
    cursor_shapes: Option<CursorShapeManager>,

    layer: LayerSurface,
    backdrop_viewport: WpViewport,
    backdrop_buffer: Option<Buffer>,
    panel: wl_surface::WlSurface,
    panel_subsurface: wl_subsurface::WlSubsurface,
    panel_viewport: WpViewport,
    // Held so the slot stays alive while the compositor may still read it.
    panel_buffer: Option<Buffer>,
    panel_size: (u32, u32),
    fractional: Option<WpFractionalScaleV1>,

    options: Options,
    matcher: Matcher,
    picker: Picker,
    text: Text,
    /// A frame callback is outstanding; draw again when it fires.
    frame_pending: bool,
    /// Something changed since the last draw.
    dirty: bool,

    /// Logical output size, known after the first configure.
    size: Option<(u32, u32)>,
    scale: f64,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    shape_device: Option<WpCursorShapeDeviceV1>,
    modifiers: Modifiers,
    outcome: Option<Outcome>,
}

impl App {
    fn set_scale(&mut self, scale: f64) {
        if scale != self.scale {
            self.scale = scale;
            self.redraw();
        }
    }

    /// Picks up new matcher results.
    fn refresh(&mut self) {
        let status = self.matcher.tick(0);
        self.picker.clamp(self.matcher.counts().0);
        if status.changed {
            self.redraw();
        }
    }

    fn query_changed(&mut self) {
        self.matcher.set_query(self.picker.query());
        // Starts the worker on the new pattern; results arrive via notify.
        self.refresh();
        self.redraw();
    }

    fn key(&mut self, event: KeyEvent) {
        let ctrl = self.modifiers.ctrl;
        let count = self.matcher.counts().0;
        // Letters compare both cases, since Caps Lock uppercases the keysym.
        let is = |lower: Keysym, upper: Keysym| {
            ctrl && (event.keysym == lower || event.keysym == upper)
        };

        match event.keysym {
            Keysym::Escape => self.outcome = Some(Outcome::Cancel),
            _ if is(Keysym::c, Keysym::C) || is(Keysym::g, Keysym::G) => {
                self.outcome = Some(Outcome::Cancel)
            }
            Keysym::Return | Keysym::KP_Enter => {
                let accept = self.picker.accept(count, self.modifiers.shift);
                self.outcome = Some(Outcome::Accept(self.output(accept)));
            }
            Keysym::Up | Keysym::KP_Up | Keysym::ISO_Left_Tab => self.move_by(-1, count),
            Keysym::Down | Keysym::KP_Down | Keysym::Tab => self.move_by(1, count),
            _ if is(Keysym::p, Keysym::P) || is(Keysym::k, Keysym::K) => self.move_by(-1, count),
            _ if is(Keysym::n, Keysym::N) || is(Keysym::j, Keysym::J) => self.move_by(1, count),
            Keysym::Page_Up | Keysym::KP_Page_Up => {
                self.picker.page(-1, count);
                self.redraw();
            }
            Keysym::Page_Down | Keysym::KP_Page_Down => {
                self.picker.page(1, count);
                self.redraw();
            }
            Keysym::BackSpace if ctrl => self.edit(Picker::delete_word),
            Keysym::BackSpace => self.edit(Picker::backspace),
            _ if is(Keysym::w, Keysym::W) => self.edit(Picker::delete_word),
            _ if is(Keysym::u, Keysym::U) => self.edit(Picker::clear),
            _ if !ctrl && !self.modifiers.alt => {
                if let Some(text) = &event.utf8
                    && self.picker.insert(text)
                {
                    self.query_changed();
                }
            }
            _ => {}
        }
    }

    fn move_by(&mut self, delta: i64, count: u32) {
        self.picker.move_by(delta, count);
        self.redraw();
    }

    fn edit(&mut self, f: fn(&mut Picker) -> bool) {
        if f(&mut self.picker) {
            self.query_changed();
        }
    }

    fn output(&self, accept: Accept) -> String {
        match (accept, self.options.index) {
            (Accept::Match(rank), index) => match self.matcher.get(rank) {
                Some(entry) if index => entry.index.to_string(),
                Some(entry) => entry.text.clone(),
                None => String::new(),
            },
            // Typed text has no position in the input. -1 like rofi.
            (Accept::Query, true) => "-1".into(),
            (Accept::Query, false) => self.picker.query().to_owned(),
        }
    }

    /// Draws now, or once the compositor wants the next frame. Keeps a
    /// fast stdin from rendering more often than the display refreshes.
    fn redraw(&mut self) {
        self.dirty = true;
        if !self.frame_pending {
            self.draw();
        }
    }

    /// Draws the panel and commits it together with the backdrop.
    fn draw(&mut self) {
        let Some((width, height)) = self.size else {
            return;
        };
        self.dirty = false;
        let (pw, ph) = (self.panel_size.0.min(width), self.panel_size.1.min(height));
        // Buffer sizes round half away from zero, as wp_fractional_scale asks.
        let bw = (pw as f64 * self.scale).round() as i32;
        let bh = (ph as f64 * self.scale).round() as i32;

        let (matched, total) = self.matcher.counts();
        let scroll = self.picker.scroll();
        let selected = (matched > 0).then(|| (self.picker.selected() - scroll) as usize);
        let rows = self.matcher.window(scroll, self.picker.lines());
        let view = View {
            prompt: self.options.prompt.as_deref(),
            query: self.picker.query(),
            rows: rows
                .into_iter()
                .map(|(entry, indices)| (entry.text.as_str(), indices))
                .collect(),
            selected,
            matched,
            total,
        };

        let (buffer, canvas) = self
            .pool
            .create_buffer(bw, bh, bw * 4, wl_shm::Format::Argb8888)
            .expect("allocate panel buffer");
        // Slots are rounded up to 64 byte alignment, so the canvas can be
        // longer than the buffer it backs.
        let canvas = &mut canvas[..(bw * bh * 4) as usize];
        render::panel(canvas, bw as u32, bh as u32, self.scale as f32, &mut self.text, &view);
        self.panel_viewport.set_destination(pw as i32, ph as i32);
        buffer.attach_to(&self.panel).expect("attach panel buffer");
        self.panel.damage_buffer(0, 0, bw, bh);
        self.panel.commit();
        self.panel_buffer = Some(buffer);

        // Centered, like a dialog. Applied with the parent commit below.
        self.panel_subsurface
            .set_position(((width - pw) / 2) as i32, ((height - ph) / 2) as i32);

        let backdrop = self.layer.wl_surface();
        if self.backdrop_buffer.is_none() {
            let (buffer, canvas) = self
                .pool
                .create_buffer(1, 1, 4, wl_shm::Format::Argb8888)
                .expect("allocate backdrop buffer");
            render::backdrop(&mut canvas[..4]);
            buffer.attach_to(backdrop).expect("attach backdrop buffer");
            backdrop.damage_buffer(0, 0, 1, 1);
            self.backdrop_buffer = Some(buffer);
        }
        self.backdrop_viewport
            .set_destination(width as i32, height as i32);
        // Every draw commits the parent, so that is where the callback goes.
        backdrop.frame(&self.qh, FrameCallbackData(backdrop.clone()));
        self.frame_pending = true;
        // The panel is a synced subsurface, so its commit above only becomes
        // visible here, together with the backdrop. No half-drawn first frame.
        self.layer.commit();
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.outcome = Some(Outcome::Cancel);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (width, height) = configure.new_size;
        if width == 0 || height == 0 {
            // Anchored to all edges, the compositor has to pick the size.
            // Not getting one is a compositor bug, but don't spin on it.
            self.outcome = Some(Outcome::Cancel);
            return;
        }
        self.size = Some((width, height));
        // A configure must be answered with a commit, frame callback or not.
        self.draw();
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            app.set_scale(scale as f64 / 120.0);
        }
    }
}

delegate_noop!(App: ignore WpViewporter);
delegate_noop!(App: ignore WpViewport);
delegate_noop!(App: ignore WpFractionalScaleManagerV1);

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if self.fractional.is_none() {
            self.set_scale(factor as f64);
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

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.dirty {
            self.draw();
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

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            // niri's wl_seat predates compositor-side repeat, so sctk drives
            // repeat from a calloop timer and calls back into `key`.
            self.keyboard = self
                .seats
                .get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    self.loop_handle.clone(),
                    Box::new(|app, _, event| app.key(event)),
                )
                .ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none()
            && let Ok(pointer) = self.seats.get_pointer(qh, &seat) {
                self.shape_device = self
                    .cursor_shapes
                    .as_ref()
                    .map(|m| m.get_shape_device(&pointer, qh));
                self.pointer = Some(pointer);
            }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
        if capability == Capability::Pointer {
            if let Some(device) = self.shape_device.take() {
                device.destroy();
            }
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            match event.kind {
                // The overlay sits under the pointer everywhere, so without
                // this the cursor is whatever the compositor last showed.
                PointerEventKind::Enter { serial } => {
                    if let Some(device) = &self.shape_device {
                        device.set_shape(serial, Shape::Default);
                    }
                }
                // A release always arrives on the surface that got the press
                // (implicit grab), so a release on the backdrop means the
                // press was outside too. Closing on release rather than press
                // keeps the release from reaching whatever is underneath.
                PointerEventKind::Release { .. } if &event.surface == self.layer.wl_surface() => {
                    self.outcome = Some(Outcome::Cancel);
                }
                _ => {}
            }
        }
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
