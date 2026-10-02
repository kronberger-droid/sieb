use std::io::{self, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use nucleo::pattern::CaseMatching;

use sieb::format::Format;
use sieb::matcher::{self, Matcher, Print};
use sieb::{config, script, window};

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
    let cli = Cli::parse();
    let case = if cli.insensitive {
        CaseMatching::Ignore
    } else {
        CaseMatching::Smart
    };
    // Naming a field only makes sense for JSON, so don't make people say both.
    let json = cli.json || cli.field.is_some();
    let print = if cli.index {
        Print::Index
    } else if json {
        Print::Json {
            field: cli.field.clone(),
        }
    } else {
        Print::Text
    };
    let format = if json { Format::Json } else { Format::Plain };

    match &cli.filter {
        Some(query) => filter(case, query, format, cli.field.clone(), &print),
        None => pick(case, cli, format, print),
    }
}

fn pick(case: CaseMatching, cli: Cli, format: Format, print: Print) -> ExitCode {
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
            format,
            field: cli.field,
            print,
        }
    } else {
        window::Input::Script(cli.script)
    };

    let options = window::Options {
        prompt: cli.prompt,
        case,
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
        Ok(window::Outcome::Secret(_)) => unreachable!("only password mode asks for a secret"),
        Ok(window::Outcome::Failed(err)) => fail(err),
        Err(err) => fail(err),
    }
}

fn filter(
    case: CaseMatching,
    query: &str,
    format: Format,
    field: Option<String>,
    print: &Print,
) -> ExitCode {
    let mut matcher = Matcher::new(case, Arc::new(|| {}));
    matcher.set_query(query);
    let reader =
        matcher::spawn_reader(BufReader::new(io::stdin()), matcher.injector(), format, field, |_, _| {});
    // The worker going idle only means it caught up with what was injected so
    // far, so wait for EOF before draining it.
    if let Err(err) = reader.join().expect("reader thread panicked") {
        eprintln!("sieb: reading stdin: {err}");
        return ExitCode::from(2);
    }
    matcher.settle(Duration::MAX);

    let mut out = BufWriter::new(io::stdout().lock());
    for entry in matcher.matches() {
        // A closed pipe (`sieb -f x | head`) is a normal way to stop.
        if writeln!(out, "{}", print.entry(entry)).is_err() {
            break;
        }
    }
    let _ = out.flush();

    if matcher.matched() > 0 {
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
