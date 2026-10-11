//! Binary edge for `hatchling-broker`.
//!
//! The command line lives in [`hatchling_broker::cli`], which the
//! `hatchling broker` front door shares.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    hatchling_broker::cli::main("hatchling-broker", &args)
}
