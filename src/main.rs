mod config;
mod layout;
mod matcher;
mod picker;
mod render;
mod script;
mod text;
mod window;

use std::io::{self, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use nucleo::pattern::CaseMatching;

use matcher::Matcher;

/// dmenu-first Wayland picker.
///
/// Reads newline-separated entries on stdin and prints the selected one.
/// Exits 0 on selection and 1 on cancel, like dmenu.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Prompt shown left of the input
    #[arg(short, long)]
    prompt: Option<String>,

    /// Match case-insensitively (default: smart case)
    #[arg(short = 'i', long)]
    insensitive: bool,

    /// Print the index of the selection instead of its text
    #[arg(long)]
    index: bool,

    /// Print all matches for QUERY without opening a window
    #[arg(short, long, value_name = "QUERY")]
    filter: Option<String>,

    /// Build the menus with SCRIPT instead of reading stdin, using rofi's
    /// script protocol
    #[arg(short, long, value_name = "SCRIPT", conflicts_with = "filter")]
    script: Option<PathBuf>,

    /// Config file [default: $XDG_CONFIG_HOME/sieb/config.toml]
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(flatten)]
    appearance: config::Appearance,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let case = if cli.insensitive {
        CaseMatching::Ignore
    } else {
        CaseMatching::Smart
    };

    match &cli.filter {
        Some(query) => filter(case, query, cli.index),
        None => pick(case, cli),
    }
}

fn pick(case: CaseMatching, cli: Cli) -> ExitCode {
    // Read before anything else, so a broken config fails fast instead of
    // after the window is up.
    let file = match config::load(cli.config.as_deref()) {
        Ok(file) => file,
        Err(err) => return fail(err),
    };
    let (font, layout, theme) = match cli.appearance.over(file).resolve() {
        Ok(resolved) => resolved,
        Err(err) => return fail(err),
    };

    let (wake, notify) = match window::wake() {
        Ok(pair) => pair,
        Err(err) => return fail(err),
    };
    let input = match cli.script {
        Some(path) => window::Input::Script(path),
        None => {
            let matcher = Matcher::new(case, notify);
            // Started before the window, so input streams in while it maps.
            // Read errors just end the list; there is no one to report them
            // to mid-pick.
            let _ = matcher::spawn_reader(BufReader::new(io::stdin()), matcher.injector());
            window::Input::Lines(matcher)
        }
    };

    let options = window::Options {
        prompt: cli.prompt,
        case,
        index: cli.index,
        font,
        layout,
        theme,
    };
    match window::run(options, input, wake) {
        Ok(window::Outcome::Accept(line)) => {
            let mut out = io::stdout().lock();
            let _ = writeln!(out, "{line}");
            ExitCode::SUCCESS
        }
        Ok(window::Outcome::Cancel) => ExitCode::FAILURE,
        Ok(window::Outcome::Quit) => ExitCode::SUCCESS,
        Ok(window::Outcome::Failed(err)) => fail(err),
        Err(err) => fail(err),
    }
}

fn filter(case: CaseMatching, query: &str, index: bool) -> ExitCode {
    let mut matcher = Matcher::new(case, Arc::new(|| {}));
    matcher.set_query(query);
    let reader = matcher::spawn_reader(BufReader::new(io::stdin()), matcher.injector());
    // The worker going idle only means it caught up with what was injected so
    // far, so wait for EOF before draining it.
    if let Err(err) = reader.join().expect("reader thread panicked") {
        eprintln!("sieb: reading stdin: {err}");
        return ExitCode::from(2);
    }
    while matcher.tick(10).running {}

    let mut out = BufWriter::new(io::stdout().lock());
    let mut matched = false;
    for entry in matcher.matches() {
        matched = true;
        let res = if index {
            writeln!(out, "{}", entry.index)
        } else {
            writeln!(out, "{}", entry.text)
        };
        // A closed pipe (`sieb -f x | head`) is a normal way to stop.
        if res.is_err() {
            break;
        }
    }
    let _ = out.flush();

    if matched {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn fail(err: impl std::fmt::Display) -> ExitCode {
    eprintln!("sieb: {err}");
    ExitCode::from(2)
}
