use std::io::{self, BufRead};
use std::process::ExitCode;

use clap::Parser;

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

    /// Match case-insensitively
    #[arg(short = 'i', long)]
    insensitive: bool,

    /// Number of visible lines
    #[arg(short, long)]
    lines: Option<u16>,

    /// Print the index of the selection instead of its text
    #[arg(long)]
    index: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let entries: Vec<String> = io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .collect();

    eprintln!("{cli:?}: {} entries, no UI yet", entries.len());
    ExitCode::FAILURE
}
