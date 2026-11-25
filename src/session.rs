//! Which of a script's calls is on screen, which is on its way in, and
//! when the one replaces the other. No window and no process in here: the
//! caller reports what the script did and is told what to show.
//!
//! A call's menu stays back until its first row, so a script that acts and
//! prints nothing closes sieb without an empty menu in between, and the
//! menu before it stays up meanwhile. A menu that asks for a selection
//! waits a little longer, for that row. Before the first menu there is
//! nothing to leave up, so the panel itself stays closed: see
//! [`Session::opening`].

use crate::script::{Call, Retv};

/// Id of the very first call, which builds the first menu.
const FIRST_CALL: u32 = 1;

/// `M` is what a call's menu is made of while it waits, handed back when
/// it is time to show it.
pub struct Session<M> {
    next_id: u32,
    /// The call whose menu is on screen.
    visible: Option<u32>,
    state: State<M>,
    opening: bool,
}

/// One call at a time.
enum State<M> {
    Idle,
    /// A picked call waiting for its activation token before it runs.
    AwaitingToken(Call),
    /// Boxed, since a waiting menu carries a whole matcher.
    Running(Box<Running<M>>),
}

/// A call whose menu is not shown yet.
struct Running<M> {
    id: u32,
    /// Why the call runs. An initial call after the first one is a mode
    /// switch: it keeps the query, like rofi, and shows an empty list
    /// rather than closing if it prints nothing.
    retv: Retv,
    /// Its first row is in, but not yet the one it wants selected: the
    /// menu waits for that row, so its first frame has the selection in
    /// place instead of jumping there.
    held: bool,
    menu: M,
}

impl<M> Running<M> {
    fn is_switch(&self) -> bool {
        self.retv == Retv::Initial && self.id > FIRST_CALL
    }
}

/// A menu to put on screen.
#[derive(Debug, PartialEq)]
pub struct Shown<M> {
    pub menu: M,
    /// The query stays as it is: the menu is a mode switch, or the first
    /// one, which has no pick behind it that the query belonged to, so
    /// what is typed by then was typed ahead for it.
    pub keep_query: bool,
}

/// What a call's first row leads to.
#[derive(Debug, PartialEq)]
pub enum FirstRow<M> {
    Show(Shown<M>),
    /// The menu waits for the row it wants selected. The caller starts the
    /// clock for [`Session::hold_expired`].
    Hold,
    /// From a call that has been replaced since.
    Stale,
}

/// What the end of a call leads to.
#[derive(Debug, PartialEq)]
pub enum Done<M> {
    /// It printed nothing: the script has acted, and sieb is done too.
    Ended,
    /// Its menu shows now, with every row it is going to get.
    Show(Shown<M>),
    /// Its menu was on screen already and has every row now.
    Complete,
    Stale,
}

impl<M> Default for Session<M> {
    fn default() -> Self {
        Self {
            next_id: FIRST_CALL,
            visible: None,
            state: State::Idle,
            opening: true,
        }
    }
}

impl<M> Session<M> {
    /// Registers a call that is being started and returns its id. Whatever
    /// was running or waiting is dropped: its late events no longer match.
    pub fn begin(&mut self, retv: Retv, menu: M) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.state = State::Running(Box::new(Running {
            id,
            retv,
            held: false,
            menu,
        }));
        id
    }

    /// Whether a pick may start a call. A second Enter while the script
    /// works is dropped rather than queued against a menu about to go.
    pub fn idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// Parks a picked call until its activation token is there.
    pub fn await_token(&mut self, call: Call) {
        self.state = State::AwaitingToken(call);
    }

    /// The call a token just arrived for. None when a mode switch in the
    /// meantime has dropped it and started its own, which stays as it is.
    pub fn take_awaiting(&mut self) -> Option<Call> {
        if !matches!(self.state, State::AwaitingToken(_)) {
            return None;
        }
        match std::mem::replace(&mut self.state, State::Idle) {
            State::AwaitingToken(call) => Some(call),
            _ => unreachable!("checked above"),
        }
    }

    /// The waiting menu of `call`, to set the options its script prints.
    pub fn pending_mut(&mut self, call: u32) -> Option<&mut M> {
        match &mut self.state {
            State::Running(running) if running.id == call => Some(&mut running.menu),
            _ => None,
        }
    }

    /// Whether `call`'s menu is the one on screen.
    pub fn is_visible(&self, call: u32) -> bool {
        self.visible == Some(call)
    }

    /// The first menu is still on its way, and the panel stays closed for
    /// it: sieb then opens on the menu instead of on an empty list that
    /// fills a moment later.
    pub fn opening(&self) -> bool {
        self.opening
    }

    /// `call` printed its first row. `wants_row` tells whether its menu
    /// asks for a selection and so waits for that row.
    pub fn first_row(&mut self, call: u32, wants_row: impl FnOnce(&M) -> bool) -> FirstRow<M> {
        let State::Running(running) = &mut self.state else {
            return FirstRow::Stale;
        };
        if running.id != call {
            return FirstRow::Stale;
        }
        if wants_row(&running.menu) {
            running.held = true;
            return FirstRow::Hold;
        }
        match self.take(call) {
            Some(running) => FirstRow::Show(self.show(running)),
            None => FirstRow::Stale,
        }
    }

    /// More rows may have arrived. `ready` tells whether a held menu has
    /// the row it wants selected, which shows it.
    pub fn rows(&mut self, ready: impl FnOnce(&mut M) -> bool) -> Option<Shown<M>> {
        let State::Running(running) = &mut self.state else {
            return None;
        };
        if !running.held || !ready(&mut running.menu) {
            return None;
        }
        let id = running.id;
        let running = self.take(id)?;
        Some(self.show(running))
    }

    /// `call` has been held for as long as a menu may be: it shows as it
    /// is, unless it has shown or been replaced already.
    pub fn hold_expired(&mut self, call: u32) -> Option<Shown<M>> {
        let held =
            matches!(&self.state, State::Running(running) if running.id == call && running.held);
        let running = held.then(|| self.take(call)).flatten()?;
        Some(self.show(running))
    }

    /// The panel has waited for the first menu long enough. True when it
    /// should open empty now. A held menu is left to show itself, which
    /// its own clock sees to shortly.
    pub fn open_expired(&mut self) -> bool {
        let held = matches!(&self.state, State::Running(running) if running.held);
        let open = self.opening && !held;
        if open {
            self.opening = false;
        }
        open
    }

    /// `call`'s script has exited, every row printed.
    pub fn done(&mut self, call: u32) -> Done<M> {
        match self.take(call) {
            // A mode with nothing to list still gets its screen, and a
            // held menu has every row it is going to get.
            Some(running) if running.is_switch() || running.held => Done::Show(self.show(running)),
            // Rows would have shown the menu already.
            Some(_) => Done::Ended,
            None if self.is_visible(call) => Done::Complete,
            None => Done::Stale,
        }
    }

    /// The running call `id`, which is done waiting.
    fn take(&mut self, id: u32) -> Option<Running<M>> {
        if !matches!(&self.state, State::Running(running) if running.id == id) {
            return None;
        }
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Running(running) => Some(*running),
            _ => unreachable!("checked above"),
        }
    }

    fn show(&mut self, running: Running<M>) -> Shown<M> {
        self.visible = Some(running.id);
        self.opening = false;
        Shown {
            keep_query: running.is_switch() || running.id == FIRST_CALL,
            menu: running.menu,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A menu that only knows its name and whether it asks for a row.
    #[derive(Debug, PartialEq, Clone, Copy)]
    struct Menu(&'static str, bool);

    fn plain(name: &'static str) -> Menu {
        Menu(name, false)
    }

    fn selecting(name: &'static str) -> Menu {
        Menu(name, true)
    }

    fn wants(menu: &Menu) -> bool {
        menu.1
    }

    fn shown(name: &'static str, keep_query: bool) -> Shown<Menu> {
        Shown {
            menu: plain(name),
            keep_query,
        }
    }

    /// A session with its first menu on screen.
    fn open() -> Session<Menu> {
        let mut session = Session::default();
        let first = session.begin(Retv::Initial, plain("first"));
        session.first_row(first, wants);
        session.done(first);
        session
    }

    #[test]
    fn first_menu_shows_on_its_first_row_and_keeps_what_was_typed() {
        let mut session = Session::default();
        assert!(session.opening());
        let call = session.begin(Retv::Initial, plain("first"));
        assert!(!session.idle());
        assert_eq!(
            session.first_row(call, wants),
            FirstRow::Show(shown("first", true))
        );
        assert!(!session.opening());
        assert!(session.idle() && session.is_visible(call));
        assert_eq!(session.done(call), Done::Complete);
    }

    #[test]
    fn menu_after_a_pick_clears_the_query() {
        let mut session = open();
        let call = session.begin(Retv::Entry, plain("second"));
        assert_eq!(
            session.first_row(call, wants),
            FirstRow::Show(shown("second", false))
        );
    }

    #[test]
    fn call_that_prints_nothing_ends_the_session() {
        let mut session = open();
        let call = session.begin(Retv::Entry, plain("action"));
        // The menu before it is still the one on screen.
        assert!(!session.is_visible(call));
        assert_eq!(session.done(call), Done::Ended);
        // The very first call may do the same, with the panel never open.
        let mut session = Session::default();
        let call = session.begin(Retv::Initial, plain("first"));
        assert_eq!(session.done(call), Done::Ended);
        assert!(session.opening());
    }

    #[test]
    fn menu_asking_for_a_row_waits_for_it() {
        let mut session = Session::default();
        let call = session.begin(Retv::Initial, selecting("first"));
        assert_eq!(session.first_row(call, wants), FirstRow::Hold);
        assert!(session.opening());
        assert_eq!(session.rows(|_| false), None);
        let menu = session.rows(|_| true).expect("the row is in");
        assert_eq!((menu.menu.0, menu.keep_query), ("first", true));
        assert!(!session.opening());
        // Its clock runs out later and finds nothing left to do.
        assert_eq!(session.hold_expired(call), None);
        assert_eq!(session.done(call), Done::Complete);
    }

    #[test]
    fn held_menu_shows_when_its_clock_runs_out_or_the_script_ends() {
        let mut session = open();
        let call = session.begin(Retv::Entry, selecting("slow"));
        session.first_row(call, wants);
        assert!(session.hold_expired(call).is_some());

        let call = session.begin(Retv::Entry, selecting("short"));
        session.first_row(call, wants);
        // Fewer rows than the rank asked for: it shows with what it has.
        assert!(matches!(session.done(call), Done::Show(_)));
    }

    #[test]
    fn rows_do_not_show_a_menu_that_is_not_held() {
        let mut session = open();
        // No first row yet, so nothing is held, whatever the rows say.
        session.begin(Retv::Entry, selecting("pending"));
        assert_eq!(session.rows(|_| true), None);
    }

    #[test]
    fn mode_switch_keeps_the_query_and_shows_an_empty_mode() {
        let mut session = open();
        let call = session.begin(Retv::Initial, plain("files"));
        assert_eq!(
            session.first_row(call, wants),
            FirstRow::Show(shown("files", true))
        );
        // A mode with nothing to list still gets its screen.
        let call = session.begin(Retv::Initial, plain("empty"));
        assert_eq!(session.done(call), Done::Show(shown("empty", true)));
    }

    #[test]
    fn events_of_a_replaced_call_are_ignored() {
        let mut session = open();
        let old = session.begin(Retv::Entry, plain("old"));
        let new = session.begin(Retv::Initial, plain("new"));
        assert_eq!(session.first_row(old, wants), FirstRow::Stale);
        assert_eq!(session.hold_expired(old), None);
        assert_eq!(session.done(old), Done::Stale);
        assert!(session.pending_mut(old).is_none());
        assert_eq!(session.pending_mut(new), Some(&mut plain("new")));
    }

    #[test]
    fn token_starts_the_call_that_waited_for_it() {
        let mut session = open();
        session.await_token(Call::initial());
        assert!(!session.idle());
        assert!(session.take_awaiting().is_some());
        assert!(session.idle());
        assert!(session.take_awaiting().is_none());
    }

    #[test]
    fn late_token_leaves_a_mode_switch_alone() {
        let mut session = open();
        session.await_token(Call::initial());
        // The mode switch drops the waiting pick and starts its own call.
        let switch = session.begin(Retv::Initial, plain("files"));
        assert!(session.take_awaiting().is_none());
        assert_eq!(
            session.first_row(switch, wants),
            FirstRow::Show(shown("files", true))
        );
    }

    #[test]
    fn panel_opens_empty_when_the_first_menu_takes_too_long() {
        let mut session = Session::default();
        let call = session.begin(Retv::Initial, plain("slow"));
        assert!(session.open_expired());
        assert!(!session.opening());
        // Only once.
        assert!(!session.open_expired());
        assert!(matches!(session.first_row(call, wants), FirstRow::Show(_)));
    }

    #[test]
    fn panel_stays_closed_for_a_held_first_menu() {
        let mut session = Session::default();
        let call = session.begin(Retv::Initial, selecting("first"));
        session.first_row(call, wants);
        assert!(!session.open_expired());
        assert!(session.opening());
        assert!(session.hold_expired(call).is_some());
        assert!(!session.opening());
    }

    #[test]
    fn opening_is_over_once_a_menu_has_shown() {
        let mut session = open();
        assert!(!session.open_expired());
    }
}
