mod config;
mod format;
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

    /// Read rows as JSON (one value per line, or one array) and print the
    /// selected object back as JSON
    #[arg(long, conflicts_with = "script")]
    json: bool,

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
        Some(query) => filter(case, query, cli.index, cli.json),
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

    let wake = match window::wake() {
        Ok(wake) => wake,
        Err(err) => return fail(err),
    };
    let input = match cli.script {
        Some(path) => window::Input::Script(path),
        None => window::Input::Stdin(format(cli.json)),
    };

    let options = window::Options {
        prompt: cli.prompt,
        case,
        index: cli.index,
        json: cli.json,
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

fn format(json: bool) -> format::Format {
    if json {
        format::Format::Json
    } else {
        format::Format::Plain
    }
}

fn filter(case: CaseMatching, query: &str, index: bool, json: bool) -> ExitCode {
    let mut matcher = Matcher::new(case, Arc::new(|| {}));
    matcher.set_query(query);
    let reader = matcher::spawn_reader(
        BufReader::new(io::stdin()),
        matcher.injector(),
        format(json),
        |_, _| {},
    );
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
        let res = match (&entry.raw, index) {
            (_, true) => writeln!(out, "{}", entry.index),
            (Some(raw), false) => writeln!(out, "{raw}"),
            (None, false) => writeln!(out, "{}", entry.text),
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
