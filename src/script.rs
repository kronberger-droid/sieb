//! Script mode, speaking rofi's script protocol (`rofi-script(5)`) so
//! existing rofi scripts run unchanged.
//!
//! The script runs with no argument for the first menu, then with the
//! picked entry as its argument. Whatever it prints becomes the next menu;
//! printing nothing ends the session.

use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

use nucleo::Injector;

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
            None => path
                .file_stem()
                .map_or_else(|| path.display().to_string(), |stem| stem.to_string_lossy().into_owned()),
        };
        Ok(Self { label, path })
    }
}

/// Why the script is being called, as `ROFI_RETV`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retv {
    Initial = 0,
    Entry = 1,
    Custom = 2,
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
    },
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
) -> io::Result<()> {
    let mut command = Command::new(script);
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
    let retv = (call.retv as u8).to_string();
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

    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("stdout is piped");
    thread::spawn(move || {
        let mut rows = 0;
        // A read error ends the menu where it got to; the exit status
        // below still tells the UI how the script fared.
        let _ = format::feed(BufReader::new(stdout), Format::Rofi, None, |line| match line {
            Line::Mode(key, value) => send(Event::Mode { call: id, key, value }),
            Line::Row(row) => {
                matcher::push(&injector, rows, row);
                if rows == 0 {
                    send(Event::FirstRow { call: id });
                }
                rows += 1;
            }
        });
        // Waiting reaps the child; a failed wait reads as a failed script.
        let status = child.wait().unwrap_or_else(|_| failed_status());
        send(Event::Done {
            call: id,
            status,
            rows,
        });
    });
    Ok(())
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
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, mpsc};

        let dir = std::env::temp_dir().join(format!("sieb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("script-{:x}", body.len() ^ body.as_ptr() as usize));
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut matcher = Matcher::new(CaseMatching::Smart, Arc::new(|| {}));
        let (sender, receiver) = mpsc::channel();
        let send = move |event| {
            let _ = sender.send(event);
        };
        spawn(&path, call, 7, matcher.injector(), send).unwrap();

        let mut events = Vec::new();
        while !events.iter().any(|e| matches!(e, Event::Done { .. })) {
            events.push(receiver.recv().unwrap());
        }
        matcher.settle(std::time::Duration::from_secs(1));
        let rows = matcher.matches().map(|e| e.row.text.clone()).collect();
        let _ = std::fs::remove_file(path);
        (rows, events)
    }

    #[test]
    fn mode_labels() {
        let mode = |s: &str| s.parse::<Mode>().unwrap();
        assert_eq!(mode("examples/launcher/drun.nu").label, "drun");
        let labelled = mode("apps:examples/launcher/drun.nu");
        assert_eq!((labelled.label.as_str(), labelled.path.to_str()), ("apps", Some("examples/launcher/drun.nu")));
        // A colon after a slash belongs to the path.
        assert_eq!(mode("./odd:name.nu").path.to_str(), Some("./odd:name.nu"));
        assert!("apps:".parse::<Mode>().is_err());
    }

    #[test]
    fn rows_options_and_exit() {
        let (rows, events) = run(r"printf '\0prompt\037Power\n'; echo Shutdown; echo Reboot", Call::initial());
        assert_eq!(rows, ["Shutdown", "Reboot"]);
        assert!(matches!(&events[0], Event::Mode { call: 7, key, value } if key == "prompt" && value == "Power"));
        assert!(matches!(events[1], Event::FirstRow { call: 7 }));
        assert!(matches!(events[2], Event::Done { call: 7, rows: 2, status } if status.success()));
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
    fn silent_failure_reports_status() {
        let (rows, events) = run("exit 3", Call::initial());
        assert!(rows.is_empty());
        assert!(matches!(events[0], Event::Done { rows: 0, status, .. } if status.code() == Some(3)));
    }
}
