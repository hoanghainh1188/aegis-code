//! `aegis`: see [`aegis_client::cli::USAGE`].

use std::io::IsTerminal;
use std::process::ExitCode;

use aegis_client::cli::{Exit, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    match run(args, &mut input, &mut out, interactive) {
        Ok(exit) => ExitCode::from(exit as u8),
        Err(e) => {
            eprintln!("aegis: {e}");
            ExitCode::from(Exit::Error as u8)
        }
    }
}
