use std::io::{self, BufReader, BufWriter, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use nucleo::pattern::CaseMatching;

use sieb::format::Format;
use sieb::layout::Layout;
use sieb::matcher::{self, Matcher, Print};
use sieb::render::Theme;
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
    let look = config::load(cli.config.as_deref())
        .map_err(|err| err.to_string())
        .and_then(|file| cli.appearance.over(file).resolve());
    let (font, layout, theme) = match look {
        Ok(look) => look,
        // Shown in the default look, the one asked for being what broke.
        Err(err) => return fail_shown("config error", err, None),
    };
    let look = (font.clone(), layout, theme.clone());

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
        Ok(window::Outcome::Accept(line, code)) => {
            let mut out = io::stdout().lock();
            let _ = writeln!(out, "{line}");
            ExitCode::from(code)
        }
        Ok(window::Outcome::Cancel) => ExitCode::FAILURE,
        Ok(window::Outcome::Quit) => ExitCode::SUCCESS,
        Ok(window::Outcome::Secret(_)) => unreachable!("only password mode asks for a secret"),
        Ok(window::Outcome::Failed(err)) => fail_shown("script failed", err, Some(look)),
        // The font or the compositor, so in the default look. Where it is
        // the compositor, the notice fails the same way and stays away.
        Err(err) => fail_shown("could not start", err, None),
    }
}

/// What a panel looks like: its font, geometry and theme.
type Look = (String, Layout, Theme);

/// Reports `err` on stderr and, when that is not a terminal, in a panel of
/// its own to read and dismiss. Started from a keybind, stderr goes where
/// nobody looks, and a picker that just does not come up says nothing
/// about why.
fn fail_shown(title: &str, err: impl std::fmt::Display, look: Option<Look>) -> ExitCode {
    let text = err.to_string();
    eprintln!("sieb: {text}");
    let look = look.or_else(|| config::Appearance::default().resolve().ok());
    if !io::stderr().is_terminal()
        && let Some((font, layout, theme)) = look
        && let Ok(wake) = window::wake()
    {
        let options = window::Options {
            prompt: Some("sieb".into()),
            case: CaseMatching::Smart,
            font,
            layout,
            theme,
        };
        let input = window::Input::Notice {
            title: title.into(),
            text,
        };
        // What stops the notice as well has been said on stderr already.
        let _ = window::run(options, input, wake);
    }
    ExitCode::from(2)
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
    let reader = matcher::spawn_reader(
        BufReader::new(io::stdin()),
        matcher.injector(),
        format,
        field,
        |_, _| {},
        || {},
    );
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
