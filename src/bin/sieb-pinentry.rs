//! sieb as a pinentry: point rbw's `pinentry` or gpg-agent's
//! `pinentry-program` here, and passwords are asked for in a sieb panel.
//!
//! Themed by `$XDG_CONFIG_HOME/sieb/pinentry.toml` when it exists, else by
//! sieb's `config.toml` without its placeholder, which would read like a
//! search field over a password.

use std::fs::File;
use std::io::{self, BufReader};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::process::ExitCode;

use nucleo::pattern::CaseMatching;

use sieb::assuan::{self, Answer, Prompt};
use sieb::config::{self, Appearance};
use sieb::{secret, window};

fn main() -> ExitCode {
    secret::harden();
    // Clients pass pinentry's own flags (--timeout, --ttyname, --display,
    // --no-global-grab). None of them mean anything to a Wayland panel, and
    // rejecting them would fail the unlock, so they are not parsed at all.

    let pinentry = config::dir()
        .map(|dir| dir.join("pinentry.toml"))
        .filter(|path| path.exists());
    let appearance = config::load(pinentry.as_deref())
        .map_err(|err| err.to_string())
        .and_then(|file| Appearance::default().over(file).resolve());
    // A broken theme must not lock anyone out of the vault: exiting here
    // leaves the client nothing to report but an EOF, with the reason on a
    // stderr nobody reads. Prompt in the default look instead.
    let fallback = appearance.is_err();
    let appearance = appearance.or_else(|err| {
        eprintln!("sieb-pinentry: {err}; using the default look");
        Appearance::default().resolve()
    });
    let (font, layout, mut theme) = match appearance {
        Ok(resolved) => resolved,
        Err(err) => {
            eprintln!("sieb-pinentry: {err}");
            return ExitCode::from(2);
        }
    };
    if pinentry.is_none() || fallback {
        theme.placeholder.clear();
    }

    // stdout unbuffered: std's `Stdout` is a LineWriter whose buffer would
    // keep a copy of the PIN line nobody wipes. ManuallyDrop, since fd 1 is
    // not ours to close.
    // SAFETY: fd 1 is open for the life of the process.
    let stdout = ManuallyDrop::new(unsafe { File::from_raw_fd(1) });
    let ask = |prompt: &Prompt| {
        let wake = match window::wake() {
            Ok(wake) => wake,
            Err(err) => return Answer::Failed(err.to_string()),
        };
        let options = window::Options {
            prompt: prompt.prompt.clone(),
            case: CaseMatching::Smart,
            font: font.clone(),
            layout,
            theme: theme.clone(),
        };
        let input = window::Input::Secret {
            message: prompt.message(),
        };
        match window::run(options, input, wake) {
            Ok(window::Outcome::Secret(pin)) => {
                if !pin.is_locked() {
                    // Usually RLIMIT_MEMLOCK. Asking anyway beats locking
                    // the user out of their vault, but it should be known.
                    eprintln!("sieb-pinentry: could not lock the password out of swap");
                }
                Answer::Pin(pin)
            }
            Ok(window::Outcome::Failed(err)) => Answer::Failed(err),
            Err(err) => Answer::Failed(err.to_string()),
            Ok(_) => Answer::Cancel,
        }
    };
    match assuan::serve(BufReader::new(io::stdin()), &*stdout, ask) {
        Ok(()) => ExitCode::SUCCESS,
        // The client went away mid-reply; there is no one left to tell.
        Err(_) => ExitCode::FAILURE,
    }
}
