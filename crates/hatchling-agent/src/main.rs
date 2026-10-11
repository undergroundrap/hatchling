//! Binary edge for `hatchling-agent`.
//!
//! The command line lives in [`hatchling_agent::cli`], which the
//! `hatchling agent` front door shares.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    hatchling_agent::cli::main("hatchling-agent", &args)
}
