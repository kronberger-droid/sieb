//! The Wayland side: a fullscreen overlay that dims the output and catches
//! clicks outside the panel, with the panel itself as a subsurface on top.

use std::error::Error;
use std::io::{self, BufReader};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use smithay_client_toolkit::{
    activation::{ActivationHandler, ActivationState, RequestData},
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle, channel,
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
        pointer::{
            BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler,
            cursor_shape::CursorShapeManager,
        },
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

use crate::format::Format;
use crate::matcher::{self, Matcher, Print};
use crate::script::{self, Call, Mode, Retv};
use crate::picker::{Accept, Picker, Wheel};
use crate::layout::Layout;
use nucleo::pattern::CaseMatching;
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use crate::render::{self, Theme, View};
use crate::secret::Secret;
use crate::text::Text;

pub struct Options {
    pub prompt: Option<String>,
    pub case: CaseMatching,
    pub font: String,
    pub layout: Layout,
    pub theme: Theme,
}

/// What fills the list.
pub enum Input {
    /// dmenu mode: rows from stdin, JSON records shown by `field`, and the
    /// pick printed as `print` says.
    Stdin {
        format: Format,
        field: Option<String>,
        print: Print,
    },
    /// Script mode: run a script for each menu, starting with the first.
    /// More than one makes them modes with buttons to switch between.
    Script(Vec<Mode>),
    /// Password mode: no list, the typing shown as dots, and `message` in
    /// the message box. Enter hands back the secret, never as text.
    Secret { message: Option<String> },
}

/// Where a left press landed. A click needs press and release on the same.
#[derive(Clone, Copy, PartialEq)]
enum Press {
    Row(usize),
    Button(usize),
}

/// Menu options and script progress, into the event loop.
enum Event {
    /// A menu option from stdin, which has no calls to tie it to.
    Option(String, String),
    Script(script::Event),
}

/// How the window was closed.
pub enum Outcome {
    Cancel,
    /// Print this and exit 0.
    Accept(String),
    /// Exit 0 without printing: a script finished its action.
    Quit,
    /// Password mode's Enter. Wiped when dropped.
    Secret(Secret),
    Failed(String),
}

/// Options a script sets for its menu with `\0key\x1fvalue` lines, or
/// JSON input with an object without `text`.
#[derive(Default)]
struct Menu {
    prompt: Option<String>,
    /// A line of text above the list.
    message: Option<String>,
    /// Rank to select when the menu appears.
    new_selection: Option<u32>,
    /// Typed text that matches nothing cannot be submitted.
    no_custom: bool,
    /// Keep the query when this menu replaces the previous one.
    keep_filter: bool,
    /// Handed to the next call as `ROFI_DATA`.
    data: Option<String>,
}

impl Menu {
    fn set(&mut self, key: &str, value: String) {
        match key {
            "prompt" => self.prompt = Some(value),
            "message" => self.message = Some(value),
            "new-selection" => self.new_selection = value.parse().ok(),
            "no-custom" => self.no_custom = value == "true",
            "keep-filter" => self.keep_filter = value == "true",
            "data" => self.data = Some(value),
            // markup-rows is applied while reading rows (format::feed).
            // urgent, active, use-hot-keys, keep-selection, delim, theme:
            // not yet.
            _ => {}
        }
    }
}

/// A call whose menu is not shown yet. The current menu stays up until its
/// first row arrives, so a script that acts and prints nothing closes sieb
/// without an empty frame in between.
struct Pending {
    id: u32,
    matcher: Matcher,
    menu: Menu,
    /// Why the call runs. An initial call after the first one is a mode
    /// switch: it keeps the query, like rofi, and shows an empty list
    /// rather than closing if it prints nothing.
    retv: Retv,
}

impl Pending {
    fn is_switch(&self) -> bool {
        self.retv == Retv::Initial && self.id > FIRST_CALL
    }
}

/// What password mode shows for each typed character.
const DOT: &str = "\u{2022}";

/// Id of the very first call, which builds the first menu.
const FIRST_CALL: u32 = 1;

/// What the script side is doing. One call at a time.
enum Busy {
    Idle,
    /// A picked call waiting for its activation token before it runs.
    AwaitingToken(Call),
    Running(Pending),
}

struct ScriptState {
    modes: Vec<Mode>,
    /// The mode whose script runs.
    active: usize,
    events: Sender<Event>,
    case: CaseMatching,
    notify: Arc<dyn Fn() + Send + Sync>,
    next_id: u32,
    /// The call whose menu is on screen.
    visible: Option<u32>,
    busy: Busy,
}

/// Wakes the event loop when the matcher has new results.
pub struct Wake {
    pending: Arc<AtomicBool>,
    source: PingSource,
    notify: Arc<dyn Fn() + Send + Sync>,
}

/// The notify callback for [`Matcher::new`] and the event source it wakes.
///
/// nucleo calls notify for every pushed line, so a plain ping would cost one
/// eventfd write per input line. The flag collapses those into one wakeup
/// until the UI has picked the results up.
pub fn wake() -> io::Result<Wake> {
    let (ping, source) = make_ping()?;
    let pending = Arc::new(AtomicBool::new(false));
    let flag = pending.clone();
    let notify = Arc::new(move || {
        if !flag.swap(true, Ordering::AcqRel) {
            ping.ping();
        }
    });
    Ok(Wake {
        pending,
        source,
        notify,
    })
}

pub fn run(mut options: Options, input: Input, wake: Wake) -> Result<Outcome, Box<dyn Error>> {
    // Input starts streaming before anything else, so it arrives while
    // fonts load and the window maps.
    let (events, event_channel) = channel::channel();
    let mut script = None;
    let mut secret = None;
    let mut menu = Menu::default();
    let mut print = Print::Text;
    let matcher = match input {
        Input::Stdin {
            format,
            field,
            print: how,
        } => {
            print = how;
            let matcher = Matcher::new(options.case, wake.notify.clone());
            // Read errors just end the list; there is no one to report
            // them to mid-pick.
            let _ = matcher::spawn_reader(
                BufReader::new(io::stdin()),
                matcher.injector(),
                format,
                field,
                move |key, value| {
                    let _ = events.send(Event::Option(key, value));
                },
            );
            matcher
        }
        Input::Script(modes) => {
            let mut state = ScriptState {
                modes,
                active: 0,
                events,
                case: options.case,
                notify: wake.notify.clone(),
                next_id: FIRST_CALL,
                visible: None,
                busy: Busy::Idle,
            };
            // No token: the first menu launches nothing.
            state.start(Call::initial())?;
            script = Some(state);
            // Shown until the first menu arrives.
            Matcher::new(options.case, wake.notify.clone())
        }
        Input::Secret { message } => {
            // Whatever the theme says: there is nothing to list or count.
            options.layout.lines = 0;
            options.theme.counter = false;
            menu.message = message;
            secret = Some(Secret::new());
            // Stays empty, so rows and clicks on them have nothing to hit.
            Matcher::new(options.case, wake.notify.clone())
        }
    };
    // fontconfig takes a while and needs nothing from the compositor, so
    // it runs alongside the connection and the first roundtrips.
    let font = options.font.clone();
    let text = std::thread::spawn(move || Text::load(&font).map_err(|err| err.to_string()));

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
    // Optional too: without it, launched apps open the way they always did.
    let activation = ActivationState::bind(&globals, &qh).ok();

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

    let (pw, ph) = options.layout.size();
    // Room for two buffers at scale 2 before the pool has to grow.
    let pool = SlotPool::new(pw as usize * ph as usize * 4 * 8, &shm)?;
    let text = text.join().expect("font thread panicked")?;
    let mut app = App {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        shm,
        pool,
        loop_handle: event_loop.handle(),
        qh: qh.clone(),
        cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),
        activation,

        layer,
        backdrop_viewport,
        backdrop_buffer: None,
        panel,
        panel_subsurface,
        panel_viewport,
        panel_buffer: None,
        fractional,

        picker: Picker::new(options.layout.lines),
        script,
        secret,
        menu,
        print,
        wheel: Wheel::default(),
        hovered: None,
        pointer_at: None,
        press: None,
        options,
        matcher,
        text,
        frame_pending: false,
        dirty: false,

        size: None,
        scale: 1.0,
        seat: None,
        serial: 0,
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
    event_loop
        .handle()
        .insert_source(event_channel, |event, _, app: &mut App| {
            if let channel::Event::Msg(event) = event {
                app.event(event);
            }
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
    activation: Option<ActivationState>,

    layer: LayerSurface,
    backdrop_viewport: WpViewport,
    backdrop_buffer: Option<Buffer>,
    panel: wl_surface::WlSurface,
    panel_subsurface: wl_subsurface::WlSubsurface,
    panel_viewport: WpViewport,
    // Held so the slot stays alive while the compositor may still read it.
    panel_buffer: Option<Buffer>,
    fractional: Option<WpFractionalScaleV1>,

    options: Options,
    matcher: Matcher,
    picker: Picker,
    script: Option<ScriptState>,
    /// Password mode's typing, in place of the query.
    secret: Option<Secret>,
    /// Options of the script menu on screen.
    menu: Menu,
    /// dmenu mode: what a pick prints.
    print: Print,
    wheel: Wheel,
    /// Visible row under the pointer, to act only when it changes.
    hovered: Option<usize>,
    /// Last pointer position on the panel.
    pointer_at: Option<(f64, f64)>,
    /// What a left press started on.
    press: Option<Press>,
    text: Text,
    /// A frame callback is outstanding; draw again when it fires.
    frame_pending: bool,
    /// Something changed since the last draw.
    dirty: bool,

    /// Logical output size, known after the first configure.
    size: Option<(u32, u32)>,
    scale: f64,
    /// The seat the keyboard is on, and the serial of its latest key press
    /// or click: what an activation token request has to show.
    seat: Option<wl_seat::WlSeat>,
    serial: u32,
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
        self.picker.clamp(self.matcher.matched());
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
        // Keys after Enter in the same batch would otherwise reach the
        // dmenu path below, which in password mode means drawing them as
        // plain text once the secret is gone.
        if self.outcome.is_some() {
            return;
        }
        if self.secret.is_some() {
            self.secret_key(event);
            return;
        }
        let ctrl = self.modifiers.ctrl;
        let count = self.matcher.matched();
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
                // Right after typing, the snapshot may still rank the previous
                // query, and Enter would pick from the wrong list.
                self.matcher.settle(Duration::from_millis(150));
                let count = self.matcher.matched();
                self.picker.clamp(count);
                let accept = self.picker.accept(count, self.modifiers.shift);
                self.accept(accept);
            }
            // rofi's mode keys. Before the plain Tab arms, which move.
            Keysym::Tab if ctrl && self.modifiers.shift => self.cycle_mode(-1),
            Keysym::ISO_Left_Tab if ctrl => self.cycle_mode(-1),
            Keysym::Tab if ctrl => self.cycle_mode(1),
            Keysym::Left | Keysym::KP_Left if self.modifiers.shift => self.cycle_mode(-1),
            Keysym::Right | Keysym::KP_Right if self.modifiers.shift => self.cycle_mode(1),
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

    /// Password mode's keys: typing, deleting, Enter and the ways out.
    /// Nothing moves, since there is nothing to move through.
    fn secret_key(&mut self, event: KeyEvent) {
        let ctrl = self.modifiers.ctrl;
        let alt = self.modifiers.alt;
        let Some(secret) = &mut self.secret else {
            return;
        };
        let is = |lower: Keysym, upper: Keysym| {
            ctrl && (event.keysym == lower || event.keysym == upper)
        };
        let changed = match event.keysym {
            Keysym::Escape => {
                self.outcome = Some(Outcome::Cancel);
                return;
            }
            _ if is(Keysym::c, Keysym::C) || is(Keysym::g, Keysym::G) => {
                self.outcome = Some(Outcome::Cancel);
                return;
            }
            Keysym::Return | Keysym::KP_Enter => {
                self.outcome = self.secret.take().map(Outcome::Secret);
                return;
            }
            // Words mean nothing in text you cannot see, so the word
            // deletions clear it all, like Ctrl+U.
            Keysym::BackSpace if ctrl => secret.clear(),
            _ if is(Keysym::w, Keysym::W) || is(Keysym::u, Keysym::U) => secret.clear(),
            Keysym::BackSpace => secret.backspace(),
            // `utf8` is sctk's own String, which we cannot wipe. See
            // `secret` for what that leaves.
            _ if !ctrl && !alt => event.utf8.as_deref().is_some_and(|text| secret.insert(text)),
            _ => false,
        };
        if changed {
            self.redraw();
        }
    }

    /// The match rank shown in visible row `row`, if that row has one.
    fn rank_at(&self, row: usize) -> Option<u32> {
        let rank = self.picker.scroll() + row as u32;
        (rank < self.matcher.matched()).then_some(rank)
    }

    /// Enter or a click: print and exit in dmenu mode, call the script in
    /// script mode.
    fn accept(&mut self, accept: Accept) {
        let Some(script) = &mut self.script else {
            self.outcome = Some(Outcome::Accept(self.output(accept)));
            return;
        };
        // One call at a time: a second Enter while the script works is
        // dropped rather than queued against a menu that is about to go.
        if !matches!(script.busy, Busy::Idle) {
            return;
        }
        let query = self.picker.query();
        let (retv, arg, info) = match accept {
            Accept::Match(rank) => match self.matcher.get(rank) {
                Some(entry) if entry.row.selectable => {
                    (Retv::Entry, entry.row.text.as_str(), entry.row.info().map(str::to_owned))
                }
                _ => return,
            },
            Accept::Query if self.menu.no_custom => return,
            Accept::Query => (Retv::Custom, query, None),
        };
        let call = Call {
            retv,
            arg: Some(arg.to_owned()),
            query: query.to_owned(),
            info,
            data: self.menu.data.clone(),
            token: None,
        };
        // Any call may launch something, and only the script knows which.
        // The token comes back in `new_token`, which then starts the call.
        if let (Some(activation), Some(seat)) = (&self.activation, &self.seat) {
            script.busy = Busy::AwaitingToken(call);
            activation.request_token(
                &self.qh,
                RequestData {
                    app_id: None,
                    seat_and_serial: Some((seat.clone(), self.serial)),
                    surface: Some(self.layer.wl_surface().clone()),
                    udata: (),
                },
            );
        } else {
            self.start(call);
        }
    }

    /// Runs `call`, or ends sieb with the reason it could not.
    fn start(&mut self, call: Call) {
        if let Some(script) = &mut self.script
            && let Err(err) = script.start(call)
        {
            self.outcome = Some(Outcome::Failed(err));
        }
    }

    /// Moves `delta` modes along, wrapping around.
    fn cycle_mode(&mut self, delta: isize) {
        let Some(script) = &self.script else {
            return;
        };
        let n = script.modes.len() as isize;
        self.switch_mode((script.active as isize + delta).rem_euclid(n) as usize);
    }

    /// Shows mode `to`, starting its script afresh. Whatever the mode on
    /// screen was waiting for is dropped.
    fn switch_mode(&mut self, to: usize) {
        let Some(script) = &mut self.script else {
            return;
        };
        if to == script.active || to >= script.modes.len() {
            return;
        }
        script.active = to;
        let call = Call {
            query: self.picker.query().to_owned(),
            ..Call::initial()
        };
        self.start(call);
        // The button follows right away, the list once rows arrive.
        self.redraw();
    }

    /// Puts a pending call's menu on screen.
    fn show(&mut self, pending: Pending) {
        if let Some(script) = &mut self.script {
            script.visible = Some(pending.id);
        }
        let keep_query = pending.menu.keep_filter || pending.is_switch();
        self.matcher = pending.matcher;
        self.menu = pending.menu;
        if !keep_query {
            self.picker.clear();
        }
        self.picker.clamp(0);
        self.matcher.set_query(self.picker.query());
        self.refresh();
        if let Some(rank) = self.menu.new_selection {
            // The script printed its rows before exiting, but they may still
            // be in flight: give the matcher a moment so a selection near
            // the end has a row to land on.
            self.matcher.settle(Duration::from_millis(50));
            self.picker.select(rank, self.matcher.matched());
        }
        self.redraw();
    }

    fn event(&mut self, event: Event) {
        let event = match event {
            Event::Option(key, value) => {
                self.menu.set(&key, value);
                self.redraw();
                return;
            }
            Event::Script(event) => event,
        };
        let Some(script) = &mut self.script else {
            return;
        };
        match event {
            // Options for the call on its way in, or the one on screen.
            script::Event::Mode { call, key, value } => match &mut script.busy {
                Busy::Running(pending) if pending.id == call => pending.menu.set(&key, value),
                _ if script.visible == Some(call) => {
                    self.menu.set(&key, value);
                    self.redraw();
                }
                _ => {}
            },
            script::Event::FirstRow { call } => {
                if let Some(pending) = script.take_running(call) {
                    self.show(pending);
                }
            }
            script::Event::Done { call, status, rows } => {
                let path = script.path().display().to_string();
                match script.take_running(call) {
                    // A mode with nothing to list still gets its screen.
                    Some(pending) if pending.is_switch() => self.show(pending),
                    // Rows would have shown the menu already, so this call
                    // printed nothing: the script is done.
                    Some(_) => {
                        debug_assert_eq!(rows, 0);
                        self.outcome = Some(if status.success() {
                            Outcome::Quit
                        } else {
                            Outcome::Failed(format!("{path} exited with {status}"))
                        });
                        return;
                    }
                    None => {}
                }
                if !status.success() {
                    eprintln!("sieb: {path} exited with {status}");
                }
            }
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
        match accept {
            Accept::Match(rank) => self
                .matcher
                .get(rank)
                .map_or_else(String::new, |entry| self.print.entry(entry)),
            Accept::Query => self.print.query(self.picker.query()),
        }
    }

    /// The row or mode button at surface position (`x`, `y`).
    fn press_at(&self, (x, y): (f64, f64)) -> Option<Press> {
        let layout = self.layout();
        layout
            .row_at(y)
            .map(Press::Row)
            .or_else(|| layout.button_at(x, y).map(Press::Button))
    }

    /// The layout of the frame on screen. Drawing and hit-testing both use
    /// it, so a click always lands where the last frame put things.
    fn layout(&self) -> Layout {
        Layout {
            message: self.menu.message.is_some(),
            buttons: self.script.as_ref().map_or(0, |s| s.modes.len()),
            ..self.options.layout
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
        let layout = self.layout();
        let (pw, ph) = layout.size();
        let (pw, ph) = (pw.min(width), ph.min(height));
        // Buffer sizes round half away from zero, as wp_fractional_scale asks.
        let bw = (pw as f64 * self.scale).round() as i32;
        let bh = (ph as f64 * self.scale).round() as i32;

        let (matched, total) = self.matcher.counts();
        let scroll = self.picker.scroll();
        let selected = (matched > 0).then(|| (self.picker.selected() - scroll) as usize);
        let rows = self.matcher.window(scroll, self.picker.lines());
        // Password mode draws a dot per character and never the secret, so
        // it stays out of shaping, glyph caches and the buffer alike.
        let dots = self.secret.as_ref().map(|secret| DOT.repeat(secret.len()));
        let view = View {
            // As in rofi, a mode's name is its prompt unless it sets one.
            prompt: self
                .menu
                .prompt
                .as_deref()
                .or(self.options.prompt.as_deref())
                .or(self.script.as_ref().map(|s| s.modes[s.active].label.as_str())),
            message: self.menu.message.as_deref(),
            query: dots.as_deref().unwrap_or(self.picker.query()),
            rows: rows
                .into_iter()
                .map(|(entry, indices)| render::RowView {
                    text: &entry.row.text,
                    styles: entry.row.styles(),
                    indices,
                })
                .collect(),
            selected,
            scroll,
            buttons: self.script.as_ref().map_or_else(Vec::new, |s| {
                s.modes.iter().map(|mode| mode.label.as_str()).collect()
            }),
            active: self.script.as_ref().map_or(0, |s| s.active),
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
        render::panel(
            canvas,
            bw as u32,
            bh as u32,
            self.scale as f32,
            &layout,
            &self.options.theme,
            &mut self.text,
            &view,
        );
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
            render::backdrop(&mut canvas[..4], self.options.theme.colors.backdrop);
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

impl ScriptState {
    /// Starts the next call. Its rows go into a fresh matcher that replaces
    /// the visible one once the first row arrives.
    /// Whatever was running or waiting is dropped: its late events no
    /// longer match a call id.
    fn start(&mut self, call: Call) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;
        let retv = call.retv;
        let matcher = Matcher::new(self.case, self.notify.clone());
        let events = self.events.clone();
        let send = move |event| {
            let _ = events.send(Event::Script(event));
        };
        script::spawn(self.path(), call, id, matcher.injector(), send)
            .map_err(|err| format!("{}: {err}", self.path().display()))?;
        self.busy = Busy::Running(Pending {
            id,
            matcher,
            menu: Menu::default(),
            retv,
        });
        Ok(())
    }

    /// The running call `id`, which is done waiting.
    fn take_running(&mut self, id: u32) -> Option<Pending> {
        match std::mem::replace(&mut self.busy, Busy::Idle) {
            Busy::Running(pending) if pending.id == id => Some(pending),
            other => {
                self.busy = other;
                None
            }
        }
    }

    fn path(&self) -> &Path {
        &self.modes[self.active].path
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

impl ActivationHandler for App {
    type RequestUdata = ();

    /// The compositor always answers, with a token it may later refuse to
    /// honor, so the waiting call never hangs here.
    fn new_token(&mut self, token: String, _: &RequestData<()>) {
        let Some(script) = &mut self.script else {
            return;
        };
        // A mode switch in the meantime has dropped the call.
        if let Busy::AwaitingToken(call) = std::mem::replace(&mut script.busy, Busy::Idle) {
            self.start(Call {
                token: Some(token),
                ..call
            });
        }
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
            self.seat = Some(seat.clone());
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
        serial: u32,
        event: KeyEvent,
    ) {
        self.serial = serial;
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
                    self.pointer_at = Some(event.position);
                }
                // A release always arrives on the surface that got the press
                // (implicit grab), so a release on the backdrop means the
                // press was outside too. Closing on release rather than press
                // keeps the release from reaching whatever is underneath.
                PointerEventKind::Release { .. } if &event.surface == self.layer.wl_surface() => {
                    self.outcome = Some(Outcome::Cancel);
                }
                _ if event.surface != self.panel => {}

                // Hover follows motion only. A panel mapping under a resting
                // pointer also sends a position, and acting on that would
                // silently override the keyboard selection.
                PointerEventKind::Motion { .. } => {
                    // Some compositors send a motion along with the enter of
                    // a surface mapping under a resting pointer. Only a
                    // position that actually changed counts as hovering.
                    if self.pointer_at.replace(event.position) == Some(event.position) {
                        continue;
                    }
                    let row = self.layout().row_at(event.position.1);
                    if row != self.hovered {
                        self.hovered = row;
                        if let Some(rank) = row.and_then(|row| self.rank_at(row)) {
                            let count = self.matcher.matched();
                            self.picker.select(rank, count);
                            self.redraw();
                        }
                    }
                }
                PointerEventKind::Leave { .. } => self.hovered = None,
                PointerEventKind::Press { button: BTN_LEFT, serial, .. } => {
                    self.serial = serial;
                    self.press = self.press_at(event.position);
                }
                // Single click accepts. Wayland has no double click, and
                // inventing a threshold would ignore the user's settings.
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    let press = self.press.take();
                    match self.press_at(event.position) {
                        released if released != press => {}
                        Some(Press::Button(button)) => self.switch_mode(button),
                        // The rank is what the user sees, so no settling:
                        // it comes from the snapshot that was drawn.
                        Some(Press::Row(row)) => {
                            if let Some(rank) = self.rank_at(row) {
                                self.accept(Accept::Match(rank));
                            }
                        }
                        None => {}
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let steps = self.wheel.steps(
                        vertical.value120,
                        vertical.discrete,
                        vertical.absolute,
                        self.options.layout.row() as f64,
                    );
                    if steps != 0 {
                        let count = self.matcher.matched();
                        self.move_by(steps, count);
                    }
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
