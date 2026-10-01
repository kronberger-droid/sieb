mod matcher;

use std::io::{self, BufReader, BufWriter, Write};
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

    /// Number of visible lines
    #[arg(short, long)]
    lines: Option<u16>,

    /// Print the index of the selection instead of its text
    #[arg(long)]
    index: bool,

    /// Print all matches for QUERY without opening a window
    #[arg(short, long, value_name = "QUERY")]
    filter: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let case = if cli.insensitive {
        CaseMatching::Ignore
    } else {
        CaseMatching::Smart
    };
    let mut matcher = Matcher::new(case, Arc::new(|| {}));

    let Some(query) = cli.filter else {
        eprintln!("sieb: no UI yet, use --filter");
        return ExitCode::FAILURE;
    };

    matcher.set_query(&query);
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
        let res = if cli.index {
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
