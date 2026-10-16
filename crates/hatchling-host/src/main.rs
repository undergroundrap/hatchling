//! Binary edge for `hatchling-host`: the command line lives in
//! [`hatchling_host::cli`].

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    hatchling_host::cli::main("hatchling-host", &args)
}
