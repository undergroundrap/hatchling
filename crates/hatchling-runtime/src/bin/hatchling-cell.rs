//! Binary edge for `hatchling-cell`: the program the broker runs in each
//! cell. It's the agent's command line and nothing else, so the program
//! inside a cell carries none of the broker's code. Point
//! `HATCHLING_AGENT_BIN` at it.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    hatchling_agent::cli::main("hatchling-cell", &args)
}
