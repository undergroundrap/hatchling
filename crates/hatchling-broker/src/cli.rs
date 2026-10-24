//! The broker's command line, shared by the `hatchling-broker` binary and
//! `hatchling broker` (the `hatchling-runtime` crate).
//!
//! Thin by design: parse config, dispatch an operator subcommand or
//! serve agents until killed. Everything else lives in the library.
//!
//! Subcommands (operator actions, run instead of serving):
//! - `migrate-vault`: encrypt a plaintext store into sealed envelopes.
//! - `rekey-vault [<new-key-file>]`: rotate the master key, inside the
//!   keyring if there is one, else to the new key file.
//! - `vault-keyring <action>`: adopt, enroll or inspect the vault keyring.
//! - `redactions <action>`, `vault-audit`: redaction review, credential access.
//!
//! Internal: `skill-cell <spec-file>` and `agent-run <spec-file>` are the spawner
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

use crate::browser_bundle::{BROWSER_DIGEST_SUBCOMMAND, browser_digest_main};
use crate::browser_cell::{BROWSER_CELL_SUBCOMMAND, browser_cell_main};
use crate::egress_authority::{EGRESS_AUTHORITY_SUBCOMMAND, egress_authority_main};
use crate::spawner::{SPAWNER_SUBCOMMAND, spawner_main};
use crate::harness_tree::{HARNESS_TREE_SUBCOMMAND, harness_tree_main};
use crate::keeper::{VAULT_KEEPER_SUBCOMMAND, vault_keeper_main};
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
    // starts for one skill cell, one scheduled run or one browser cell,
    // before any config.
    match args.first().map(String::as_str) {
        Some(SKILL_CELL_SUBCOMMAND) => return skill_cell_main(args.get(1).map(String::as_str)),
        Some(BROWSER_CELL_SUBCOMMAND) => {
            return browser_cell_main(args.get(1).map(String::as_str));
        }
        // The operator's, and like the helpers it reads no configuration.
        Some(BROWSER_DIGEST_SUBCOMMAND) => {
            return browser_digest_main(args.get(1).map(String::as_str));
        }
        Some(AGENT_RUN_SUBCOMMAND) => return agent_run_main(args.get(1).map(String::as_str)),
        Some(HARNESS_TREE_SUBCOMMAND) => {
            return harness_tree_main(args.get(1..).unwrap_or_default());
        }
        Some(VAULT_KEEPER_SUBCOMMAND) => return vault_keeper_main(),
        Some(EGRESS_AUTHORITY_SUBCOMMAND) => return egress_authority_main(),
        Some(SPAWNER_SUBCOMMAND) => return spawner_main(),
        Some(_) | None => {}
    }
    match run(args) {
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
        "Serve agents with no subcommand. Operator subcommands:\n  migrate-vault\n  rekey-vault [<new-key-file>]   (no file with a keyring)\n  vault-keyring <action>\n    init, enroll-totp, enroll-keys, use-file-tier, backup-codes,\n    status, add-key, remove-key, label-key\n  redactions list | stats | categories\n  redactions show <id> | raw <id> | override <id> [--any-scope]\n  vault-audit [log [count]]\n  clean [--dry-run]   (a dead broker's leftovers; never your data)\n  browser-digest <browser>   (the digest HATCHLING_BROWSER_SHA256 pins)\n\nThe broker builds and supervises the agent's cell. Internal spawner\ncommands skill-cell <spec-file>, agent-run <spec-file>, browser-cell <spec-file>,\nharness-tree, vault-keeper, egress-authority and spawner are broker-owned.",
        crate::environment::ENVIRONMENT,
    )
}

/// Rejects unknown top-level arguments before reading any configuration.
fn valid_args(args: &[String]) -> bool {
    let values: Vec<&str> = args.iter().map(String::as_str).collect();
    match values.as_slice() {
        []
        | ["migrate-vault" | "vault-audit" | "rekey-vault" | "clean"]
        | ["clean", "--dry-run"]
        | ["rekey-vault" | BROWSER_DIGEST_SUBCOMMAND, _]
        | [
            "vault-keyring",
            "init" | "enroll-totp" | "enroll-keys" | "use-file-tier" | "backup-codes" | "status"
            | "add-key" | "remove-key" | "label-key",
        ]
        | ["redactions", "list" | "stats" | "categories"]
        | ["redactions", "show" | "raw" | "override", _]
        | ["redactions", "override", _, "--any-scope"]
        | ["vault-audit", "log"]
        | [HARNESS_TREE_SUBCOMMAND, "give" | "reclaim", _, ..] => true,
        [VAULT_KEEPER_SUBCOMMAND | EGRESS_AUTHORITY_SUBCOMMAND | SPAWNER_SUBCOMMAND] => {
            args.len() == 1
        }
        ["vault-audit", "log", count] => count.parse::<usize>().is_ok(),
        [
            SKILL_CELL_SUBCOMMAND | AGENT_RUN_SUBCOMMAND | BROWSER_CELL_SUBCOMMAND,
            ..,
        ] => args.len() <= 2,
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
fn run(args: &[String]) -> Result<(), Error> {
    let config = Config::from_env()?;
    // Operator subcommands act on the vault directly and never bind
    // the socket. Anything else falls through to serving.
    match args.first().map(String::as_str) {
        Some("migrate-vault") => run_migrate_vault(&config),
        Some("rekey-vault") => run_rekey_vault(&config, args.get(1).map(Path::new)),
        Some("vault-keyring") => run_keyring(&config, args.get(1).map(String::as_str)),
        Some("clean") => crate::sweep::run_clean(&config, args.get(1..).unwrap_or_default()),
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
        for helper in [
            SKILL_CELL_SUBCOMMAND,
            AGENT_RUN_SUBCOMMAND,
            BROWSER_CELL_SUBCOMMAND,
        ] {
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
            &["rekey-vault", "a", "b"],
        ] {
            assert_eq!(
                main("hatchling broker", &args(invalid)),
                ExitCode::from(EX_USAGE)
            );
        }
        let text = help("hatchling broker");
        assert!(text.starts_with("Usage: hatchling broker "));
        assert!(text.contains("migrate-vault"));
        assert!(text.contains("rekey-vault [<new-key-file>]"));
        assert!(text.contains("vault-keyring <action>"));
        assert!(text.contains("browser-digest <browser>"));
        // The digest is the operator's to ask for before any setting.
        assert_eq!(
            main(
                "hatchling broker",
                &args(&[BROWSER_DIGEST_SUBCOMMAND, "/nonexistent/chrome"])
            ),
            ExitCode::FAILURE
        );
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
            args(&["rekey-vault"]),
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
            args(&["clean"]),
            args(&["clean", "--dry-run"]),
            args(&[SKILL_CELL_SUBCOMMAND]),
            args(&[AGENT_RUN_SUBCOMMAND, "/spec"]),
            args(&[BROWSER_DIGEST_SUBCOMMAND, "/opt/chrome/chrome"]),
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
            args(&[BROWSER_DIGEST_SUBCOMMAND]),
            args(&[BROWSER_DIGEST_SUBCOMMAND, "/a", "/b"]),
            args(&["migrate-vault", "extra"]),
            args(&["rekey-vault", "/new.key", "extra"]),
            args(&["vault-keyring"]),
            args(&["vault-keyring", "bogus"]),
            args(&["redactions", "bogus"]),
            args(&["redactions", "override", "id", "--bogus"]),
            args(&["vault-audit", "stats"]),
            args(&["clean", "--force"]),
            args(&["clean", "--dry-run", "extra"]),
            args(&[SKILL_CELL_SUBCOMMAND, "/spec", "extra"]),
        ] {
            assert!(!valid_args(&given), "{given:?}");
        }
    }

    #[test]
    fn run_still_reports_missing_configuration() {
        assert!(run(&[]).is_err());
    }
}
