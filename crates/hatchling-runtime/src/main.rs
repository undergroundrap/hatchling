//! Binary edge for `hatchling`: routes to the broker or the agent (see
//! [`hatchling_runtime::route`]).
//!
//! Note the denied lints: no `println!`/`eprint!`/`process::exit`. Output
//! goes through `io::Write`, and the process outcome is a returned
//! [`ExitCode`].

use std::io::Write as _;
use std::process::ExitCode;

use hatchling_runtime::{Route, USAGE, help, route};

/// `EX_USAGE` from `sysexits.h`: the command was used incorrectly.
const EX_USAGE: u8 = 64;

fn main() -> ExitCode {
    let mut invocation = std::env::args();
    let argv0 = invocation.next().unwrap_or_default();
    let args: Vec<String> = invocation.collect();
    match route(&argv0, &args) {
        Route::Broker(rest) => hatchling_broker::cli::main("hatchling broker", rest),
        Route::Agent(rest) => hatchling_agent::cli::main("hatchling agent", rest),
        Route::Help => {
            let _ = write!(std::io::stdout().lock(), "{}", help());
            ExitCode::SUCCESS
        }
        Route::Version => {
            let _ = writeln!(
                std::io::stdout().lock(),
                "hatchling {}",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Route::Usage => {
            let _ = writeln!(std::io::stderr().lock(), "{USAGE}");
            ExitCode::from(EX_USAGE)
        }
    }
}
