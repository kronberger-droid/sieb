//! Script mode, speaking rofi's script protocol (`rofi-script(5)`) so
//! existing rofi scripts run unchanged.
//!
//! The script runs with no argument for the first menu, then with the
//! picked entry as its argument. Whatever it prints becomes the next menu;
//! printing nothing ends the session.
//!
//! A menu with the `complete` option is also asked, while the user types,
//! for completions of the query: see [`complete`].

use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use nucleo::Injector;
use rustix::process::{Pid, Signal, kill_process};

use crate::format::{self, Format, Line};
use crate::matcher::{self, Entry};

/// A script and the label of its button, from `--script [LABEL:]PATH`,
/// rofi's `-modi name:script` spelled for sieb.
#[derive(Clone, Debug, PartialEq)]
pub struct Mode {
    pub label: String,
    pub path: PathBuf,
}

impl FromStr for Mode {
    type Err = String;

    /// A colon splits off a label unless what comes before it has a slash,
    /// so `./odd:name.nu` stays a path.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (label, path) = match s.split_once(':') {
            Some((label, path)) if !label.contains('/') => (Some(label), path),
            _ => (None, s),
        };
        if path.is_empty() {
            return Err("missing script path".into());
        }
        let path = PathBuf::from(path);
        let label = match label {
            Some(label) => label.to_owned(),
            None => path.file_stem().map_or_else(
                || path.display().to_string(),
                |stem| stem.to_string_lossy().into_owned(),
            ),
        };
        Ok(Self { label, path })
    }
}

/// Why the script is being called, as `ROFI_RETV`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retv {
    Initial,
    Entry,
    Custom,
    /// rofi's `kb-custom-N`, on a row or typed text alike, sent as 9+N so
    /// scripts written for rofi read it the same way. 1 is Ctrl+Enter, the
    /// rest the menu's `custom-keys` in order.
    Kb(u8),
    /// sieb's own: completions for the query, see [`complete`].
    Complete,
}

impl Retv {
    pub fn code(self) -> u8 {
        match self {
            Retv::Initial => 0,
            Retv::Entry => 1,
            Retv::Custom => 2,
            Retv::Kb(n) => 9 + n,
            Retv::Complete => 20,
        }
    }
}

/// Owned, since a call can wait for its activation token while the menu it
/// was picked from keeps changing.
#[derive(Debug)]
pub struct Call {
    pub retv: Retv,
    /// The picked entry or typed text; none on the initial call.
    pub arg: Option<String>,
    pub query: String,
    pub info: Option<String>,
    pub data: Option<String>,
    /// xdg-activation token for whatever the script launches, so the new
    /// window may take focus.
    pub token: Option<String>,
}

impl Call {
    /// The first call, which builds the first menu.
    pub fn initial() -> Self {
        Self {
            retv: Retv::Initial,
            arg: None,
            query: String::new(),
            info: None,
            data: None,
            token: None,
        }
    }
}

/// Messages from a running call to the UI, tagged with the call they
/// belong to so a late message from an old call cannot touch a newer menu.
#[derive(Debug)]
pub enum Event {
    Mode {
        call: u32,
        key: String,
        value: String,
    },
    /// The first row arrived: time to show this call's menu.
    FirstRow { call: u32 },
    Done {
        call: u32,
        status: ExitStatus,
        rows: u32,
        /// The end of what a failed first call of a mode wrote to stderr,
        /// for telling the user why no menu came. Empty otherwise.
        stderr: String,
    },
    /// What a [`complete`] call printed, for the query of `generation`.
    Completions {
        generation: u32,
        items: Vec<Completion>,
    },
}

/// One completion: what the list shows and the query Tab puts in.
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub label: String,
    pub query: String,
}

/// A call that was started, to stop it when another one takes its place.
pub struct Running {
    pid: Pid,
    state: Arc<State>,
}

/// What a call's reader thread and its [`Running`] handle share.
struct State {
    /// The call is there to list rows: the first call of a mode, or any
    /// call once it has printed one. A pick that has printed nothing may
    /// be acting on it, and is never cut short.
    listing: AtomicBool,
    /// Asked to stop: the reader takes its next read for the end.
    stopped: AtomicBool,
    /// Set right before the reader waits for the script. Until then the
    /// script has not been reaped and its pid cannot be anyone else's; the
    /// lock keeps a signal from slipping in after.
    reaping: Mutex<bool>,
}

impl Running {
    /// Ends a call that is still listing rows nobody will see: a stream
    /// like `fd` over a home directory would otherwise run to its end
    /// behind the menu that replaced it. The script gets SIGTERM, and its
    /// output is closed, which stops what it started at the next write.
    pub fn stop(&self) {
        if !self.state.listing.load(Ordering::Acquire) {
            return;
        }
        self.state.stopped.store(true, Ordering::Release);
        let reaping = self.state.reaping.lock().unwrap_or_else(|e| e.into_inner());
        if !*reaping {
            let _ = kill_process(self.pid, Signal::TERM);
        }
    }
}

/// Reads as ended once the call is stopped, whatever is still coming.
struct Until<R> {
    inner: R,
    state: Arc<State>,
}

impl<R: Read> Read for Until<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.state.stopped.load(Ordering::Acquire) {
            return Ok(0);
        }
        self.inner.read(buf)
    }
}

/// Starts `script` and streams its rows into `injector` on a thread.
///
/// Spawning happens here rather than on the thread, so a script that is
/// missing or not executable fails right away with a useful error.
pub fn spawn(
    script: &Path,
    call: Call,
    id: u32,
    injector: Injector<Entry>,
    send: impl Fn(Event) + Send + 'static,
) -> io::Result<Running> {
    let mut command = command(script, &call);
    // Only for the call that lists a mode's first menu, where a failure
    // leaves the user with nothing on screen to go by. A picked call may
    // launch a program that inherits this pipe, and one that outlives sieb
    // would be killed by its next line to a stderr nobody reads any more.
    if call.retv == Retv::Initial {
        command.stderr(Stdio::piped());
    }
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let said = child.stderr.take().map(Said::listen);
    let state = Arc::new(State {
        listing: AtomicBool::new(call.retv == Retv::Initial),
        stopped: AtomicBool::new(false),
        reaping: Mutex::new(false),
    });
    let running = Running {
        pid: Pid::from_child(&child),
        state: state.clone(),
    };
    thread::spawn(move || {
        let mut rows = 0;
        let reader = Until {
            inner: stdout,
            state: state.clone(),
        };
        // A read error ends the menu where it got to; the exit status
        // below still tells the UI how the script fared. The pipe closes
        // with the reader, when this returns.
        let _ = format::feed(
            BufReader::new(reader),
            Format::Rofi,
            None,
            |line| match line {
                Line::Mode(key, value) => send(Event::Mode {
                    call: id,
                    key,
                    value,
                }),
                Line::Row(row) => {
                    matcher::push(&injector, rows, row);
                    if rows == 0 {
                        state.listing.store(true, Ordering::Release);
                        send(Event::FirstRow { call: id });
                    }
                    rows += 1;
                }
            },
        );
        *state.reaping.lock().unwrap_or_else(|e| e.into_inner()) = true;
        // Waiting reaps the child; a failed wait reads as a failed script.
        let status = child.wait().unwrap_or_else(|_| failed_status());
        // A stopped call ends on a signal, which is no failure to report.
        if !state.stopped.load(Ordering::Acquire) {
            let stderr = match said {
                Some(said) if !status.success() => said.end(),
                _ => String::new(),
            };
            send(Event::Done {
                call: id,
                status,
                rows,
                stderr,
            });
        }
    });
    Ok(running)
}

/// What a script writes to stderr, passed on to sieb's own as it comes, as
/// if inherited, with the end of it kept.
struct Said {
    kept: Arc<Mutex<Vec<u8>>>,
    /// Hangs up when the script's stderr closes.
    closed: mpsc::Receiver<()>,
}

/// How much of a script's stderr is kept, counted from its end.
const SAID_BYTES: usize = 4096;
const SAID_LINES: usize = 10;

impl Said {
    fn listen(mut stderr: ChildStderr) -> Self {
        let kept = Arc::new(Mutex::new(Vec::new()));
        let (open, closed) = mpsc::channel();
        let keep = kept.clone();
        thread::spawn(move || {
            let _open = open;
            let mut buf = [0; 1024];
            while let Ok(n) = stderr.read(&mut buf)
                && n > 0
            {
                let _ = io::stderr().write_all(&buf[..n]);
                let mut kept = keep.lock().unwrap_or_else(|e| e.into_inner());
                kept.extend_from_slice(&buf[..n]);
                let excess = kept.len().saturating_sub(SAID_BYTES);
                kept.drain(..excess);
            }
        });
        Self { kept, closed }
    }

    /// The last lines of it, once the script has exited. Its own writes
    /// are in the pipe by then, but perhaps not read yet, so this waits a
    /// moment for the pipe to close, and not longer: a program the script
    /// left running may hold it open for good.
    fn end(self) -> String {
        let _ = self.closed.recv_timeout(Duration::from_millis(100));
        let kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        last_lines(&String::from_utf8_lossy(&kept), SAID_LINES)
    }
}

/// The last `count` lines of `text` that have something on them, without
/// the escape sequences a script may color its errors with.
fn last_lines(text: &str, count: usize) -> String {
    let plain = strip_escapes(text);
    let lines: Vec<&str> = plain
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

/// `text` without ANSI escape sequences: `ESC [`, parameters, one letter.
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
        } else if chars.next() == Some('[') {
            // Up to and including the final byte, a letter or the like.
            for ch in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    break;
                }
            }
        }
    }
    out
}

/// Asks `script` for completions of `call.query`, with `ROFI_RETV` 20.
///
/// Each row it prints is one: the text is what the list shows, and `info`
/// the whole query that Tab puts in, the text itself when there is none.
/// Menu options are ignored. Completions run beside the menu's own call
/// and never replace its rows. The caller gets the child to kill and reap,
/// since a newer query makes its answer worthless.
pub fn complete(
    script: &Path,
    call: Call,
    generation: u32,
    send: impl Fn(Event) + Send + 'static,
) -> io::Result<Child> {
    let mut child = command(script, &call).spawn()?;
    let stdout = child.stdout.take().expect("stdout is piped");
    thread::spawn(move || {
        let mut items = Vec::new();
        let _ = format::feed(BufReader::new(stdout), Format::Rofi, None, |line| {
            if let Line::Row(row) = line {
                let query = row.info().map_or_else(|| row.text.clone(), str::to_owned);
                items.push(Completion {
                    label: row.text,
                    query,
                });
            }
        });
        send(Event::Completions { generation, items });
    });
    Ok(child)
}

/// The script's process, with the call's environment set and stdout piped.
fn command(script: &Path, call: &Call) -> Command {
    let mut command = Command::new(program(script));
    command
        .args(&call.arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // The names GTK 4 / Qt and GTK 3 read the token from. Without a fresh
    // one, unset: sieb was likely started with a token of its own, already
    // spent on mapping sieb, and an app handed that one stays unfocused.
    for var in ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"] {
        match &call.token {
            Some(token) => command.env(var, token),
            None => command.env_remove(var),
        };
    }
    let retv = call.retv.code().to_string();
    let vars: [(&[&str], Option<&str>); 4] = [
        (&["SIEB_RETV", "ROFI_RETV"], Some(&retv)),
        (&["SIEB_INFO", "ROFI_INFO"], call.info.as_deref()),
        (&["SIEB_DATA", "ROFI_DATA"], call.data.as_deref()),
        // rofi has no query variable.
        (&["SIEB_QUERY"], Some(&call.query)),
    ];
    for (names, value) in vars {
        for name in names {
            match value {
                Some(value) => command.env(name, value),
                // Unset rather than inherited from a sieb that a script
                // started, which would leak an unrelated value.
                None => command.env_remove(name),
            };
        }
    }
    command
}

/// What to execute for `script`. A name without a slash is looked up on
/// `PATH` when executed, which misses `--script menu.nu` given from the
/// script's own directory: a file of that name here wins, as it would for
/// any other file argument, and `PATH` stays the fallback.
fn program(script: &Path) -> PathBuf {
    if script.components().count() == 1 && script.is_file() {
        Path::new(".").join(script)
    } else {
        script.to_owned()
    }
}

fn failed_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(1 << 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `body` as a script through `spawn` and returns the rows it fed
    /// the matcher plus the events it sent, in order.
    fn run(body: &str, call: Call) -> (Vec<String>, Vec<Event>) {
        use crate::matcher::Matcher;
        use nucleo::pattern::CaseMatching;
        use std::sync::{Arc, mpsc};

        let mut matcher = Matcher::new(CaseMatching::Smart, Arc::new(|| {}));
        let (sender, receiver) = mpsc::channel();
        let send = move |event| {
            let _ = sender.send(event);
        };
        let injector = matcher.injector();
        let (path, running) = with_script(body, |path| spawn(path, call, 7, injector, send));
        running.unwrap();

        let mut events = Vec::new();
        while !events.iter().any(|e| matches!(e, Event::Done { .. })) {
            events.push(receiver.recv().unwrap());
        }
        matcher.settle(std::time::Duration::from_secs(1));
        let rows = matcher.matches().map(|e| e.row.text.clone()).collect();
        let _ = std::fs::remove_file(path);
        (rows, events)
    }

    /// Writes `body` as an executable sh script in a temp dir, named after it
    /// so tests running at once do not overwrite each other's, and hands it
    /// to `start`, which executes it.
    ///
    /// One test at a time gets from the write to the end of `start`. A child
    /// forked by another test meanwhile would inherit the descriptor the
    /// script is written through and hold it until its own exec, and
    /// executing a file that is open for writing fails with ETXTBSY.
    fn with_script<T>(body: &str, start: impl FnOnce(&Path) -> T) -> (PathBuf, T) {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Mutex, PoisonError};

        static STARTING: Mutex<()> = Mutex::new(());
        // A test that panicked in here guarded nothing worth distrusting.
        let _starting = STARTING.lock().unwrap_or_else(PoisonError::into_inner);

        let dir = std::env::temp_dir().join(format!("sieb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("script-{:x}", body.len() ^ body.as_ptr() as usize));
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = start(&path);
        (path, started)
    }

    #[test]
    fn mode_labels() {
        let mode = |s: &str| s.parse::<Mode>().unwrap();
        assert_eq!(mode("examples/launcher/drun.nu").label, "drun");
        let labelled = mode("apps:examples/launcher/drun.nu");
        assert_eq!(
            (labelled.label.as_str(), labelled.path.to_str()),
            ("apps", Some("examples/launcher/drun.nu"))
        );
        // A colon after a slash belongs to the path.
        assert_eq!(mode("./odd:name.nu").path.to_str(), Some("./odd:name.nu"));
        assert!("apps:".parse::<Mode>().is_err());
    }

    #[test]
    fn rows_options_and_exit() {
        let (rows, events) = run(
            r"printf '\0prompt\037Power\n'; echo Shutdown; echo Reboot",
            Call::initial(),
        );
        assert_eq!(rows, ["Shutdown", "Reboot"]);
        assert!(
            matches!(&events[0], Event::Mode { call: 7, key, value } if key == "prompt" && value == "Power")
        );
        assert!(matches!(events[1], Event::FirstRow { call: 7 }));
        assert!(
            matches!(&events[2], Event::Done { call: 7, rows: 2, status, stderr } if status.success() && stderr.is_empty())
        );
    }

    #[test]
    fn environment_and_argument() {
        let call = Call {
            retv: Retv::Entry,
            arg: Some("Shutdown".into()),
            query: "shut".into(),
            info: Some("poweroff".into()),
            data: Some("state".into()),
            token: Some("tok".into()),
        };
        let script = r#"echo "$1|$ROFI_RETV|$SIEB_RETV|$ROFI_INFO|$ROFI_DATA|$SIEB_QUERY|$XDG_ACTIVATION_TOKEN|$DESKTOP_STARTUP_ID""#;
        let (rows, _) = run(script, call);
        assert_eq!(rows, ["Shutdown|1|1|poweroff|state|shut|tok|tok"]);
    }

    #[test]
    fn unset_info_is_not_inherited() {
        let (rows, _) = run(r#"echo "[${ROFI_INFO-unset}]""#, Call::initial());
        assert_eq!(rows, ["[unset]"]);
    }

    #[test]
    fn stale_activation_token_is_not_inherited() {
        // Safety: tests run as threads of one process, and no other test
        // reads this variable.
        unsafe { std::env::set_var("XDG_ACTIVATION_TOKEN", "stale") };
        let (rows, _) = run(r#"echo "[${XDG_ACTIVATION_TOKEN-unset}]""#, Call::initial());
        assert_eq!(rows, ["[unset]"]);
    }

    #[test]
    fn completions_take_info_as_the_query() {
        use std::sync::mpsc;

        let script = r"printf '\0prompt\037ignored\nwhere\0info\037ls | where\n'; echo $ROFI_RETV";
        let (sender, receiver) = mpsc::channel();
        let call = Call {
            retv: Retv::Complete,
            arg: Some("ls | wh".into()),
            query: "ls | wh".into(),
            ..Call::initial()
        };
        let (path, child) = with_script(script, |path| {
            complete(path, call, 3, move |event| {
                let _ = sender.send(event);
            })
        });
        let mut child = child.unwrap();
        let event = receiver.recv().unwrap();
        let _ = child.wait();
        let _ = std::fs::remove_file(path);
        let item = |label: &str, query: &str| Completion {
            label: label.into(),
            query: query.into(),
        };
        assert!(matches!(
            event,
            Event::Completions { generation: 3, items }
                if items == [item("where", "ls | where"), item("20", "20")]
        ));
    }

    #[test]
    fn silent_failure_reports_status() {
        let (rows, events) = run("exit 3", Call::initial());
        assert!(rows.is_empty());
        assert!(
            matches!(events[0], Event::Done { rows: 0, status, .. } if status.code() == Some(3))
        );
    }

    #[test]
    fn failed_first_call_reports_what_it_said() {
        let script = "echo 'rbw: the vault is locked' >&2; exit 1";
        let (_, events) = run(script, Call::initial());
        assert!(
            matches!(&events[0], Event::Done { stderr, .. } if stderr == "rbw: the vault is locked")
        );
        // A picked call keeps sieb's own stderr, and nothing is kept.
        let picked = Call {
            retv: Retv::Entry,
            ..Call::initial()
        };
        let (_, events) = run("echo oops >&2; exit 1", picked);
        assert!(matches!(&events[0], Event::Done { stderr, .. } if stderr.is_empty()));
    }

    #[test]
    fn said_does_not_wait_for_a_program_left_running() {
        // The sleep inherits stderr and holds it open well past the exit.
        let script = "echo first >&2; sleep 5 > /dev/null & exit 1";
        let start = std::time::Instant::now();
        let (_, events) = run(script, Call::initial());
        assert!(matches!(&events[0], Event::Done { stderr, .. } if stderr == "first"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn last_lines_keeps_the_end_without_blanks_or_colors() {
        let text = "one\n\n\u{1b}[31mtwo\u{1b}[0m  \nthree\n";
        assert_eq!(last_lines(text, 2), "two\nthree");
        assert_eq!(last_lines(text, 9), "one\ntwo\nthree");
        assert_eq!(last_lines("", 3), "");
    }
}
