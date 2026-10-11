//! The broker's command line, shared by the `hatchling-broker` binary and
//! `hatchling broker` (the `hatchling-runtime` crate).
//!
//! Thin by design: parse config, dispatch an operator subcommand or
//! serve agents until killed. Everything else lives in the library.
//!
//! Subcommands (operator actions, run instead of serving):
//! - `migrate-vault`: encrypt a plaintext store into sealed envelopes.
//! - `rekey-vault <new-key-file>`: rotate the master key.
//! - `vault-keyring <action>`: adopt, enroll or inspect the vault keyring.
//! - `redactions <action>`, `vault-audit`: redaction review, credential access.
//!
//! Internal: `skill-cell <spec>` and `agent-run <spec>` are the spawner
//! helpers the broker starts, by re-running its own executable, to run
//! one skill cell or one scheduled run (see [`crate::skill_run`] and
//! [`crate::run_launcher`]). They read no broker configuration.
//!
//! Note the denied lints: no `println!`/`eprint!`/`process::exit`. Startup
//! diagnostics go to stderr via `io::Write`, and the process outcome is a
//! returned [`ExitCode`].

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use crate::skill_run::{SKILL_CELL_SUBCOMMAND, skill_cell_main};
use crate::vault_keyring::run_keyring;
use crate::{AGENT_RUN_SUBCOMMAND, agent_run_main, run_operator};
use crate::{Config, Error, run_migrate_vault, run_rekey_vault, run_serve};

/// Runs the broker's command line.
///
/// `args` are the arguments after the program name; `program` is how
/// the broker was invoked (`hatchling-broker`, or `hatchling broker`),
/// used in usage text and as the prefix of a startup error.
#[must_use]
pub fn main(program: &str, args: &[String]) -> ExitCode {
    // The spawner helpers: fresh, single-threaded processes the broker
    // starts for one skill cell or one scheduled run, before any config.
    match args.first().map(String::as_str) {
        Some(SKILL_CELL_SUBCOMMAND) => return skill_cell_main(args.get(1).map(String::as_str)),
        Some(AGENT_RUN_SUBCOMMAND) => return agent_run_main(args.get(1).map(String::as_str)),
        Some(_) | None => {}
    }
    match run(program, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "{program}: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Parses configuration, then either runs an operator subcommand or
/// serves agents until killed.
///
/// # Errors
///
/// Propagates [`Error`] from config parsing, subcommand misuse, or the
/// dispatched library action.
fn run(program: &str, args: &[String]) -> Result<(), Error> {
    let config = Config::from_env()?;
    // Operator subcommands act on the vault directly and never bind
    // the socket. Anything else falls through to serving.
    match args.first().map(String::as_str) {
        Some("migrate-vault") => run_migrate_vault(&config),
        Some("rekey-vault") => {
            let new_key_file = args.get(1).ok_or_else(|| Error::Usage {
                reason: format!(
                    "rekey-vault requires a new key file path: \
                         {program} rekey-vault <new-key-file>"
                ),
            })?;
            run_rekey_vault(&config, Path::new(new_key_file))
        }
        Some("vault-keyring") => run_keyring(&config.vault, args.get(1).map(String::as_str)),
        Some(other) => run_operator(other, &config, args.iter().skip(1).cloned()),
        None => run_serve(&config),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn the_spawner_helpers_refuse_without_a_spec() {
        // Each helper is reached before any configuration is read, and
        // fails closed when the broker didn't hand it a cell spec.
        for helper in [SKILL_CELL_SUBCOMMAND, AGENT_RUN_SUBCOMMAND] {
            assert_eq!(
                main("hatchling-broker", &args(&[helper])),
                ExitCode::FAILURE
            );
        }
    }
}
