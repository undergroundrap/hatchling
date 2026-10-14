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

use hatchling_core::cli::{EX_USAGE, help_requested, render_help};

use crate::{Cell, Config, Error, run_command};

/// Runs the agent's command line.
///
/// `args` are the arguments after the program name; `program` is how
/// the agent was invoked (`hatchling-agent`, or `hatchling agent`),
/// used in usage text and as the prefix of a startup error.
#[must_use]
pub fn main(program: &str, args: &[String]) -> ExitCode {
    if help_requested(args) {
        let _ = write!(std::io::stdout().lock(), "{}", help(program));
        return ExitCode::SUCCESS;
    }
    if !valid_args(args) {
        let _ = write!(std::io::stderr().lock(), "{}", help(program));
        return ExitCode::from(EX_USAGE);
    }
    match run(program, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "{program}: failed to start: {err}");
            if matches!(err, Error::Usage(_)) {
                let _ = write!(stderr, "{}", help(program));
                ExitCode::from(EX_USAGE)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// Help for either agent invocation, using its configuration declarations.
#[must_use]
pub fn help(program: &str) -> String {
    render_help(
        program,
        "[run \"task\"] | --help | -h",
        "Runs inside a cell built by the broker; the broker supplies its\nidentity, socket and configuration. With no subcommand, complete the\ncell handshake and report readiness. run executes one task and prints\nits final answer. Model requests cross the broker's egress boundary.",
        crate::environment::ENVIRONMENT,
    )
}

fn valid_args(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        None => true,
        Some("run") => args.len() >= 2,
        Some(_) => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn main_fails_closed_on_misuse() {
        assert_eq!(
            main("hatchling-agent", &args(&["frobnicate"])),
            ExitCode::from(EX_USAGE)
        );
        assert_eq!(
            main("hatchling-agent", &args(&["run"])),
            ExitCode::from(EX_USAGE)
        );
        assert_eq!(
            main("hatchling-agent", &args(&["run", "   "])),
            ExitCode::from(EX_USAGE)
        );
    }

    #[test]
    fn an_unknown_subcommand_names_the_program_in_its_usage() {
        let result = run("hatchling agent", &args(&["frobnicate"]));
        assert!(
            matches!(&result, Err(Error::Usage(text)) if text.contains("usage: hatchling agent run")),
            "{result:?}"
        );
    }

    #[test]
    fn run_task_refuses_an_empty_task() {
        assert!(run_task(&[]).is_err());
        assert!(run_task(&args(&["  "])).is_err());
    }

    #[test]
    fn run_lifecycle_refuses_without_configuration() {
        // No HATCHLING_* configuration in a test process: the cell never
        // starts, rather than running unconfigured.
        assert!(run_lifecycle().is_err());
    }

    #[test]
    fn help_explains_the_cell_and_every_setting() {
        for flag in ["--help", "-h"] {
            assert_eq!(main("hatchling-agent", &args(&[flag])), ExitCode::SUCCESS);
        }
        let text = help("hatchling agent");
        assert!(text.starts_with("Usage: hatchling agent "));
        assert!(text.contains("cell built by the broker"));
        for variable in crate::environment::ENVIRONMENT {
            assert!(text.contains(variable.name));
            assert!(text.contains(variable.requirement));
            assert!(text.contains(variable.description));
        }
        assert_eq!(
            main("hatchling-agent", &args(&["-h", "extra"])),
            ExitCode::from(EX_USAGE)
        );
    }

    #[test]
    fn valid_args_keeps_tasks_as_task_text() {
        assert!(valid_args(&[]));
        assert!(valid_args(&args(&["run", "task", "with", "words"])));
        assert!(valid_args(&args(&["run", "--a-task-not-a-flag"])));
        assert!(!valid_args(&args(&["run"])));
        assert!(!valid_args(&args(&["bogus"])));
    }
}
