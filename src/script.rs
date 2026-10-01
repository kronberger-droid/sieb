//! Script mode, speaking rofi's script protocol (`rofi-script(5)`) so
//! existing rofi scripts run unchanged.
//!
//! The script runs with no argument for the first menu, then with the
//! picked entry as its argument. Whatever it prints becomes the next menu;
//! printing nothing ends the session.

use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

use nucleo::Injector;
use smithay_client_toolkit::reexports::calloop::channel::Sender;

use crate::matcher::{self, Entry};

/// One line of script output.
#[derive(Debug, PartialEq)]
pub enum Line {
    /// `\0key\x1fvalue`: an option for the whole menu, like the prompt.
    Mode(String, String),
    Row(Row),
}

#[derive(Debug, Default, PartialEq)]
pub struct Row {
    pub text: String,
    /// Handed back to the script in `ROFI_INFO` when this row is picked.
    pub info: Option<String>,
    /// Extra search terms, matched but not shown.
    pub meta: Option<String>,
    pub selectable: bool,
}

/// Splits rofi's in-band options off a line: `text\0key\x1fvalue\x1f...`.
pub fn parse(line: &str) -> Line {
    let (text, options) = line.split_once('\0').unwrap_or((line, ""));
    let mut pairs = options.split('\x1f');
    if text.is_empty() && !options.is_empty() {
        let key = pairs.next().unwrap_or_default().to_owned();
        let value = pairs.collect::<Vec<_>>().join("\x1f");
        return Line::Mode(key, value);
    }
    let mut row = Row {
        text: text.to_owned(),
        selectable: true,
        ..Row::default()
    };
    while let Some(key) = pairs.next() {
        let value = pairs.next().unwrap_or_default();
        match key {
            "info" => row.info = Some(value.to_owned()),
            "meta" => row.meta = Some(value.to_owned()),
            "nonselectable" => row.selectable = value != "true",
            // icon, urgent, active, permanent: not supported yet.
            _ => {}
        }
    }
    Line::Row(row)
}

/// Why the script is being called, as `ROFI_RETV`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retv {
    Initial = 0,
    Entry = 1,
    Custom = 2,
}

pub struct Call<'a> {
    pub retv: Retv,
    /// The picked entry or typed text; none on the initial call.
    pub arg: Option<&'a str>,
    pub query: &'a str,
    pub info: Option<&'a str>,
    pub data: Option<&'a str>,
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
    events: Sender<Event>,
) -> io::Result<()> {
    let mut command = Command::new(script);
    command
        .args(call.arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let retv = (call.retv as u8).to_string();
    for (name, value) in [
        ("RETV", Some(retv.as_str())),
        ("INFO", call.info),
        ("DATA", call.data),
        ("QUERY", Some(call.query)),
    ] {
        for prefix in ["SIEB_", "ROFI_"] {
            if prefix == "ROFI_" && name == "QUERY" {
                continue;
            }
            let var = format!("{prefix}{name}");
            match value {
                Some(value) => command.env(var, value),
                // Unset rather than inherited from a sieb that a script
                // started, which would leak an unrelated value.
                None => command.env_remove(var),
            };
        }
    }

    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("stdout is piped");
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        let mut rows = 0;
        while let Ok(n) = reader.read_until(b'\n', &mut buf) {
            if n == 0 {
                break;
            }
            let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            match parse(&String::from_utf8_lossy(line)) {
                Line::Mode(key, value) => {
                    let _ = events.send(Event::Mode { call: id, key, value });
                }
                Line::Row(row) => {
                    matcher::push(&injector, rows, row);
                    if rows == 0 {
                        let _ = events.send(Event::FirstRow { call: id });
                    }
                    rows += 1;
                }
            }
            buf.clear();
        }
        // Waiting reaps the child; a failed wait reads as a failed script.
        let status = child.wait().unwrap_or_else(|_| failed_status());
        let _ = events.send(Event::Done {
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

    #[test]
    fn plain_row() {
        assert_eq!(
            parse("Shutdown"),
            Line::Row(Row {
                text: "Shutdown".into(),
                selectable: true,
                ..Row::default()
            })
        );
    }

    #[test]
    fn row_options() {
        assert_eq!(
            parse("Shutdown\0info\x1fpoweroff\x1fmeta\x1fhalt off\x1fnonselectable\x1ftrue\x1ficon\x1fsystem"),
            Line::Row(Row {
                text: "Shutdown".into(),
                info: Some("poweroff".into()),
                meta: Some("halt off".into()),
                selectable: false,
            })
        );
    }

    #[test]
    fn mode_option() {
        assert_eq!(
            parse("\0prompt\x1fPower"),
            Line::Mode("prompt".into(), "Power".into())
        );
    }

    /// Runs `body` as a script through `spawn` and returns the rows it fed
    /// the matcher plus the events it sent, in order.
    fn run(body: &str, call: Call) -> (Vec<String>, Vec<Event>) {
        use crate::matcher::Matcher;
        use nucleo::pattern::CaseMatching;
        use smithay_client_toolkit::reexports::calloop::{EventLoop, channel};
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!("sieb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("script-{:x}", body.len() ^ body.as_ptr() as usize));
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut matcher = Matcher::new(CaseMatching::Smart, Arc::new(|| {}));
        let (sender, receiver) = channel::channel();
        spawn(&path, call, 7, matcher.injector(), sender).unwrap();

        let mut event_loop: EventLoop<Vec<Event>> = EventLoop::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(receiver, |event, _, events: &mut Vec<Event>| {
                if let channel::Event::Msg(event) = event {
                    events.push(event);
                }
            })
            .unwrap();
        let mut events = Vec::new();
        while !events.iter().any(|e| matches!(e, Event::Done { .. })) {
            event_loop.dispatch(None, &mut events).unwrap();
        }
        matcher.settle(std::time::Duration::from_secs(1));
        let rows = matcher.matches().map(|e| e.text.clone()).collect();
        let _ = std::fs::remove_file(path);
        (rows, events)
    }

    fn initial() -> Call<'static> {
        Call {
            retv: Retv::Initial,
            arg: None,
            query: "",
            info: None,
            data: None,
        }
    }

    #[test]
    fn rows_options_and_exit() {
        let (rows, events) = run(r"printf '\0prompt\037Power\n'; echo Shutdown; echo Reboot", initial());
        assert_eq!(rows, ["Shutdown", "Reboot"]);
        assert!(matches!(&events[0], Event::Mode { call: 7, key, value } if key == "prompt" && value == "Power"));
        assert!(matches!(events[1], Event::FirstRow { call: 7 }));
        assert!(matches!(events[2], Event::Done { call: 7, rows: 2, status } if status.success()));
    }

    #[test]
    fn environment_and_argument() {
        let call = Call {
            retv: Retv::Entry,
            arg: Some("Shutdown"),
            query: "shut",
            info: Some("poweroff"),
            data: Some("state"),
        };
        let script = r#"echo "$1|$ROFI_RETV|$SIEB_RETV|$ROFI_INFO|$ROFI_DATA|$SIEB_QUERY""#;
        let (rows, _) = run(script, call);
        assert_eq!(rows, ["Shutdown|1|1|poweroff|state|shut"]);
    }

    #[test]
    fn unset_info_is_not_inherited() {
        let (rows, _) = run(r#"echo "[${ROFI_INFO-unset}]""#, initial());
        assert_eq!(rows, ["[unset]"]);
    }

    #[test]
    fn silent_failure_reports_status() {
        let (rows, events) = run("exit 3", initial());
        assert!(rows.is_empty());
        assert!(matches!(events[0], Event::Done { rows: 0, status, .. } if status.code() == Some(3)));
    }

    #[test]
    fn empty_line_is_an_empty_row() {
        assert!(matches!(parse(""), Line::Row(row) if row.text.is_empty()));
    }
}
