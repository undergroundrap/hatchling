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

use hatchling_core::cli::{EX_USAGE, help_requested, render_help};

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
    if help_requested(args) {
        let _ = write!(std::io::stdout().lock(), "{}", help(program));
        return ExitCode::SUCCESS;
    }
    if !valid_args(args) {
        let _ = write!(std::io::stderr().lock(), "{}", help(program));
        return ExitCode::from(EX_USAGE);
    }
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
            if matches!(err, Error::Usage { .. }) {
                let _ = write!(stderr, "{}", help(program));
                ExitCode::from(EX_USAGE)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// Help for either broker invocation, generated from its reader declarations.
#[must_use]
pub fn help(program: &str) -> String {
    render_help(
        program,
        "[subcommand] | --help | -h",
        "Serve agents with no subcommand. Operator subcommands:\n  migrate-vault\n  rekey-vault <new-key-file>\n  vault-keyring <action>\n    init, enroll-totp, enroll-keys, use-file-tier, backup-codes,\n    status, add-key, remove-key, label-key\n  redactions list | stats | categories\n  redactions show <id> | raw <id> | override <id> [--any-scope]\n  vault-audit [log [count]]\n\nThe broker builds and supervises the agent's cell. Internal spawner\ncommands skill-cell <spec> and agent-run <spec> are broker-owned.",
        crate::environment::ENVIRONMENT,
    )
}

/// Rejects unknown top-level arguments before reading any configuration.
fn valid_args(args: &[String]) -> bool {
    let values: Vec<&str> = args.iter().map(String::as_str).collect();
    match values.as_slice() {
        []
        | ["migrate-vault" | "vault-audit"]
        | ["rekey-vault", _]
        | [
            "vault-keyring",
            "init" | "enroll-totp" | "enroll-keys" | "use-file-tier" | "backup-codes" | "status"
            | "add-key" | "remove-key" | "label-key",
        ]
        | ["redactions", "list" | "stats" | "categories"]
        | ["redactions", "show" | "raw" | "override", _]
        | ["redactions", "override", _, "--any-scope"]
        | ["vault-audit", "log"] => true,
        ["vault-audit", "log", count] => count.parse::<usize>().is_ok(),
        [SKILL_CELL_SUBCOMMAND | AGENT_RUN_SUBCOMMAND, ..] => args.len() <= 2,
        _ => false,
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

    #[test]
    fn help_and_usage_work_before_configuration() {
        for flag in ["--help", "-h"] {
            assert_eq!(main("hatchling-broker", &args(&[flag])), ExitCode::SUCCESS);
        }
        for invalid in [
            &["--unknown"][..],
            &["--help", "extra"],
            &["migrate-vault", "extra"],
            &["rekey-vault"],
        ] {
            assert_eq!(
                main("hatchling broker", &args(invalid)),
                ExitCode::from(EX_USAGE)
            );
        }
        let text = help("hatchling broker");
        assert!(text.starts_with("Usage: hatchling broker "));
        assert!(text.contains("migrate-vault"));
        assert!(text.contains("rekey-vault <new-key-file>"));
        assert!(text.contains("vault-keyring <action>"));
        for variable in crate::environment::ENVIRONMENT {
            assert!(text.contains(variable.name), "missing {}", variable.name);
            assert!(text.contains(variable.requirement));
            assert!(text.contains(variable.description));
        }
    }

    #[test]
    fn valid_args_preserves_operator_and_helper_commands() {
        for given in [
            vec![],
            args(&["migrate-vault"]),
            args(&["rekey-vault", "/new.key"]),
            args(&["vault-keyring", "status"]),
            args(&["redactions", "show", "event"]),
            args(&["redactions", "list"]),
            args(&["redactions", "stats"]),
            args(&["redactions", "categories"]),
            args(&["redactions", "raw", "id"]),
            args(&["redactions", "override", "id"]),
            args(&["redactions", "override", "id", "--any-scope"]),
            args(&["vault-audit"]),
            args(&["vault-audit", "log"]),
            args(&["vault-audit", "log", "12"]),
            args(&[SKILL_CELL_SUBCOMMAND]),
            args(&[AGENT_RUN_SUBCOMMAND, "/spec"]),
        ] {
            assert!(valid_args(&given), "{given:?}");
        }
        assert!(valid_args(&args(&["vault-audit", "log", "0"])));
        assert!(valid_args(&args(&[
            "vault-audit",
            "log",
            &usize::MAX.to_string()
        ])));
        for count in ["--unknown", "-1", "", "184467440737095516160"] {
            assert!(!valid_args(&args(&["vault-audit", "log", count])));
        }
        for action in [
            "init",
            "enroll-totp",
            "enroll-keys",
            "use-file-tier",
            "backup-codes",
            "add-key",
            "remove-key",
            "label-key",
        ] {
            assert!(valid_args(&args(&["vault-keyring", action])));
        }
        for given in [
            args(&["bogus"]),
            args(&["migrate-vault", "extra"]),
            args(&["rekey-vault"]),
            args(&["vault-keyring"]),
            args(&["vault-keyring", "bogus"]),
            args(&["redactions", "bogus"]),
            args(&["redactions", "override", "id", "--bogus"]),
            args(&["vault-audit", "stats"]),
            args(&[SKILL_CELL_SUBCOMMAND, "/spec", "extra"]),
        ] {
            assert!(!valid_args(&given), "{given:?}");
        }
    }

    #[test]
    fn run_still_reports_missing_configuration() {
        assert!(run("hatchling-broker", &[]).is_err());
    }
}
