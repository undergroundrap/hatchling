//! The agent's command line, shared by the `hatchling-agent` binary and
//! `hatchling agent` (the `hatchling-runtime` crate).
//!
//! Thin by design: parse config and arguments, install logging, run
//! the cell lifecycle, and translate the outcome to a process exit
//! code.
//!
//! Note the denied lints: no `println!`/`eprint!`/`process::exit`.
//! Startup diagnostics go to stderr via `io::Write`, the task answer
//! goes to stdout the same way, and the process outcome is a returned
//! [`ExitCode`].

use std::io::Write as _;
use std::process::ExitCode;

use crate::{Cell, Config, Error, run_command};

/// Runs the agent's command line.
///
/// `args` are the arguments after the program name; `program` is how
/// the agent was invoked (`hatchling-agent`, or `hatchling agent`),
/// used in usage text and as the prefix of a startup error.
#[must_use]
pub fn main(program: &str, args: &[String]) -> ExitCode {
    match run(program, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "{program}: failed to start: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches on the subcommand. Bare invocation keeps the historical
/// behavior: connect, handshake, report readiness, exit.
fn run(program: &str, args: &[String]) -> Result<(), Error> {
    match args.first().map(String::as_str) {
        None => run_lifecycle(),
        Some("run") => run_task(args.get(1..).unwrap_or_default()),
        Some(other) => Err(Error::Usage(format!(
            "unknown subcommand {other:?}; usage: {program} run \"task\""
        ))),
    }
}

/// Parses configuration, checks preconditions, connects to the broker,
/// authorizes the broker's kernel peer credentials, and completes the
/// handshake.
///
/// # Errors
///
/// Propagates [`Error`] from config parsing, precondition checks, the
/// broker connection, peer authorization, or the handshake. Any failure
/// is fail-closed: the cell does not run.
fn run_lifecycle() -> Result<(), Error> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_max_level(config.log_level.to_tracing())
        .init();
    let cell = Cell::new(config)
        .check_preconditions()?
        .connect_broker()?
        .authorize()?
        .handshake()?;
    tracing::info!(cell_id = %cell.config().cell_id, "cell authorized, handshake complete, ready");
    Ok(())
}

/// Prints the final answer of one agent task to stdout.
///
/// The library owns the whole subcommand flow (see [`run_command`]);
/// this only moves the answer to stdout. Logs already went to stderr,
/// so the answer stays machine-readable.
///
/// # Errors
///
/// Propagates [`Error`] from [`run_command`]: empty task, missing or
/// invalid model configuration, mint failure, or loop failure.
fn run_task(task_args: &[String]) -> Result<(), Error> {
    let answer = run_command(task_args)?;
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{answer}");
    Ok(())
}
