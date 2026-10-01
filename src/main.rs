mod config;
mod format;
mod layout;
mod markup;
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

    /// Show this key of each JSON record instead of `text`. Implies --json
    // Not `text`: that id belongs to `--color-text`.
    #[arg(long = "text", value_name = "FIELD", conflicts_with = "script")]
    field: Option<String>,

    /// Print all matches for QUERY without opening a window
    #[arg(short, long, value_name = "QUERY")]
    filter: Option<String>,

    /// Build the menus with SCRIPT instead of reading stdin, using rofi's
    /// script protocol. Given more than once, each script is a mode with a
    /// button, named by LABEL or the file name
    #[arg(short, long, value_name = "[LABEL:]SCRIPT", conflicts_with = "filter")]
    script: Vec<script::Mode>,

    /// Config file [default: $XDG_CONFIG_HOME/sieb/config.toml]
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(flatten)]
    appearance: config::Appearance,
}

fn main() -> ExitCode {
    let mut cli = Cli::parse();
    // Naming a field only makes sense for JSON, so don't make people say both.
    cli.json |= cli.field.is_some();

    let case = if cli.insensitive {
        CaseMatching::Ignore
    } else {
        CaseMatching::Smart
    };

    match &cli.filter {
        Some(query) => filter(case, query, cli.index, cli.json, cli.field.clone()),
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
    let input = if cli.script.is_empty() {
        window::Input::Stdin {
            format: format(cli.json),
            field: cli.field.clone(),
        }
    } else {
        window::Input::Script(cli.script)
    };

    let options = window::Options {
        prompt: cli.prompt,
        case,
        index: cli.index,
        json: cli.json,
        field: cli.field,
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

fn filter(
    case: CaseMatching,
    query: &str,
    index: bool,
    json: bool,
    field: Option<String>,
) -> ExitCode {
    let mut matcher = Matcher::new(case, Arc::new(|| {}));
    matcher.set_query(query);
    let reader = matcher::spawn_reader(
        BufReader::new(io::stdin()),
        matcher.injector(),
        format(json),
        field,
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

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    /// clap only checks the argument table when it parses, so a clash like
    /// two args with one id would otherwise surface as a panic at startup.
    #[test]
    fn cli_is_well_formed() {
        super::Cli::command().debug_assert();
    }
}
