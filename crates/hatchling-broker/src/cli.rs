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
//! - `tools <action>`: the approved tools a CLI cell may run (WO-019).
//! - `doctor [--env <file>]`: what stops Hatchling working on this
//!   machine, and the fix for each ([`crate::doctor`]).
//! - `secret add <name> | --for <host:port> [--replace]`: a key into the
//!   vault, typed unseen or piped in ([`crate::secret_ops`]).
//!
//! Internal: `skill-cell <spec-file>` and `agent-run <spec-file>` are the spawner
//! helpers the broker starts, by re-running its own executable, to run
//! one skill cell or one scheduled run (see [`crate::skill_run`] and
//! [`crate::run_launcher`]). They read no broker configuration.
//!
//! An operator command from a shell with no settings, where Hatchling is
//! installed as a service, runs with the service's ([`crate::service_settings`]).
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
use crate::harness_tree::{HARNESS_TREE_SUBCOMMAND, harness_tree_main};
use crate::keeper::{VAULT_KEEPER_SUBCOMMAND, vault_keeper_main};
use crate::skill_run::{SKILL_CELL_SUBCOMMAND, skill_cell_main};
use crate::spawner::{ROOM_MOUNT_SUBCOMMAND, SPAWNER_SUBCOMMAND, spawner_main};
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
        // The installer's: how a loaded model fits, from its server; it
        // reads no settings.
        Some(crate::model_window::MODEL_FIT_SUBCOMMAND) => {
            return crate::model_window::model_fit_main(args.get(1..).unwrap_or_default());
        }
        Some(AGENT_RUN_SUBCOMMAND) => return agent_run_main(args.get(1).map(String::as_str)),
        Some(HARNESS_TREE_SUBCOMMAND) => {
            return harness_tree_main(args.get(1..).unwrap_or_default());
        }
        Some(VAULT_KEEPER_SUBCOMMAND) => return vault_keeper_main(),
        Some(EGRESS_AUTHORITY_SUBCOMMAND) => return egress_authority_main(),
        Some(SPAWNER_SUBCOMMAND) => return spawner_main(),
        Some(ROOM_MOUNT_SUBCOMMAND) => {
            return crate::room_launch::helper_main(hatchling_sys::take_stdin);
        }
        // It reads the settings itself, and reports one that doesn't
        // parse rather than stopping at it.
        Some("doctor") => return crate::doctor::doctor_main(args.get(1..).unwrap_or_default()),
        // Root's, before the service's settings are taken on: it writes the
        // file they're read from, and restarts the service.
        Some(crate::model_switch::USE_HOSTED | crate::model_switch::USE_LOCAL) => {
            return crate::model_switch::switch_main(program, args);
        }
        // The owner's, as a client of the local API: it reads no
        // settings, and runs without sudo.
        Some("schedule") => {
            return crate::schedule_cli::schedule_main(program, args.get(1..).unwrap_or_default());
        }
        Some("sign-in") => {
            return crate::sign_in_cli::sign_in_main(program, args.get(1..).unwrap_or_default());
        }
        Some(_) | None => {}
    }
    // The guided setup, and the authenticator enrollment on a service
    // install, are root's, on a service install: said before
    // anything is read. It changes nothing yet: the service is stopped and
    // its settings added to once the setup itself starts.
    let service = Path::new(crate::doctor::SERVICE_SETTINGS);
    if crate::key_setup::for_service(args, service.exists()) {
        let ready = crate::key_setup::installed(|service| {
            crate::key_setup::ready(hatchling_sys::euid(), service)
        });
        if let Err(why) = ready {
            let _ = writeln!(std::io::stderr().lock(), "{program} vault-keyring: {why}");
            return ExitCode::FAILURE;
        }
    }
    // An operator command from a shell with no settings, on a service
    // install, and the guided setup from any shell: it runs again with the
    // service's (the call returns only when it can't).
    if let Some(unread) = crate::service_settings::rerun_if_wanted(args, service, 0) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "{program}: {}\n{}",
            unread.what,
            unread.fix
        );
        return ExitCode::FAILURE;
    }
    // Root's, with the service's settings alone: what stopping the service
    // runs, so it reads nothing else and acts for no other user.
    if args.iter().map(String::as_str).eq(["room-disk", "close"]) {
        return crate::room_disk::close_main(program);
    }
    // Root's, as itself: it opens the security keys the keeper may not,
    // so it acts for no other user.
    if args.first().map(String::as_str) == Some(crate::unlock_relay::UNLOCK_SUBCOMMAND) {
        return match Config::from_env() {
            Ok(config) => crate::unlock_relay::unlock_main(program, &config.vault),
            Err(err) => {
                let _ = writeln!(std::io::stderr().lock(), "{program}: {err}");
                ExitCode::FAILURE
            }
        };
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "{program}: {err}");
            if matches!(err, Error::Usage { .. }) {
                // What's wrong was just said: the whole manual after it
                // would bury it.
                let _ = writeln!(stderr, "{}", see_help(program));
                ExitCode::from(EX_USAGE)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// Where the commands and settings are listed, said after a usage error.
fn see_help(program: &str) -> String {
    format!("`{program} --help` lists every command and setting.")
}

/// Help for either broker invocation, generated from its reader declarations.
#[must_use]
pub fn help(program: &str) -> String {
    render_help(
        program,
        "[subcommand] | --help | -h",
        "Serve agents with no subcommand. Operator subcommands:\n  doctor [--env <file>]   (what stops it working here, and the fix)\n  migrate-vault\n  rekey-vault [<new-key-file>]   (no file with a keyring)\n  vault-keyring <action>\n    setup   (security keys, guided; run again, it carries on)\n    lost-key   (a key that's lost or stolen, guided)\n    change-pin   (a new PIN on the key plugged in)\n    reminders-off, reminders-on   (the reminder to try a key that's gone quiet)\n    init, enroll-totp, enroll-keys, use-file-tier, backup-codes,\n    status, add-key, remove-key, label-key\n  unlock   (a vault with a security key or codes waits, locked, after a\n    start as a service: open it here, with the key or a code)\n  redactions list | stats | categories\n  redactions show <id> | raw <id> | override <id> [--any-scope]\n  vault-audit [log [count]]\n  tools list | add <path> | remove <path>   (what a CLI cell may run)\n  secret add <name> | --for <host:port> [--replace | --if-missing]   (a key, shown as *)\n  use-hosted openrouter | --endpoint <url> [--model <id>] [--window <tokens>] [--thinking off|on]\n  use-local --model <name> [--endpoint <url>] [--window <tokens>] [--thinking off|on]\n    (switch the model, its server and its egress together, and restart)\n  clean [--dry-run]   (a dead broker's leftovers; never your data)\n  lock   (the vault's key out of memory and the room disk closed, now, until it's unlocked)\n  room-disk close   (the agent's room disk closed, now: what stopping the service does)\n  backup [<file>]   (one sealed file)\n  restore <file>\n  reset conversation | agent | everything [--yes] [--no-backup]\n  schedule list | add <task> <when> | remove <id>   (as yourself, not sudo)\n  browser-digest <browser>   (the digest HATCHLING_BROWSER_SHA256 pins)\n\nThe broker builds and supervises the agent's cell. Internal spawner\ncommands skill-cell <spec-file>, agent-run <spec-file>, browser-cell <spec-file>,\nharness-tree, vault-keeper, egress-authority and spawner are broker-owned.",
        crate::environment::ENVIRONMENT,
    )
}

/// Rejects unknown top-level arguments before reading any configuration.
fn valid_args(args: &[String]) -> bool {
    let values: Vec<&str> = args.iter().map(String::as_str).collect();
    match values.as_slice() {
        []
        | [
            "backup"
            | "lock"
            | "migrate-vault"
            | "vault-audit"
            | "rekey-vault"
            | "clean"
            | "doctor"
            | crate::unlock_relay::UNLOCK_SUBCOMMAND,
        ]
        | ["clean", "--dry-run"]
        | ["room-disk", "close"]
        | ["doctor", "--env", _]
        | [
            "backup" | "restore" | "rekey-vault" | BROWSER_DIGEST_SUBCOMMAND,
            _,
        ]
        | [crate::model_window::MODEL_FIT_SUBCOMMAND, _, _]
        | [
            "vault-keyring",
            "setup" | "lost-key" | "change-pin" | "init" | "enroll-totp" | "enroll-keys"
            | "use-file-tier" | "backup-codes" | "status" | "add-key" | "remove-key" | "label-key"
            | "reminders-off" | "reminders-on",
        ]
        | ["redactions", "list" | "stats" | "categories"]
        | ["redactions", "show" | "raw" | "override", _]
        | ["redactions", "override", _, "--any-scope"]
        | ["vault-audit", "log"]
        | ["tools", "list"]
        | [
            "reset"
            | "schedule"
            | "sign-in"
            | crate::model_switch::USE_HOSTED
            | crate::model_switch::USE_LOCAL,
            ..,
        ]
        | ["tools", "add" | "remove", _]
        | ["secret", "add", _]
        | ["secret", "add", _, _]
        | ["secret", "add", "--for", _, "--replace" | "--if-missing"]
        | [HARNESS_TREE_SUBCOMMAND, "give" | "reclaim", _, ..] => true,
        [
            VAULT_KEEPER_SUBCOMMAND
            | EGRESS_AUTHORITY_SUBCOMMAND
            | SPAWNER_SUBCOMMAND
            | ROOM_MOUNT_SUBCOMMAND,
        ] => args.len() == 1,
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
    // the socket; run as root, they act for the keeper's user there, and
    // for the broker's in its stores. Anything else falls through to
    // serving.
    if args.first().is_some_and(|command| command != "clean") {
        crate::keeper::act_for_keeper(&config)?;
        crate::broker_user::act_for_broker(&config)?;
    }
    match args.first().map(String::as_str) {
        Some("migrate-vault") => run_migrate_vault(&config),
        Some("rekey-vault") => run_rekey_vault(&config, args.get(1).map(Path::new)),
        Some("vault-keyring") => run_keyring(&config, args.get(1).map(String::as_str)),
        Some("clean") => crate::sweep::run_clean(&config, args.get(1..).unwrap_or_default()),
        Some("backup") => crate::backup::run_backup(&config, args.get(1).map(Path::new)),
        Some("lock") => crate::backup::run_lock(),
        Some("restore") => args.get(1).map_or_else(
            || {
                Err(Error::Usage {
                    reason: String::from("usage: hatchling-broker restore <file>"),
                })
            },
            |from| crate::backup::run_restore(&config, Path::new(from)),
        ),
        Some("reset") => crate::backup::run_reset(&config, args.get(1..).unwrap_or_default()),
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

    /// A usage error is followed by where the manual is, not by all of
    /// it: what's wrong was just said.
    #[test]
    fn a_usage_error_points_at_the_help() {
        assert_eq!(
            see_help("hatchling broker"),
            "`hatchling broker --help` lists every command and setting."
        );
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
        assert!(text.contains("doctor [--env <file>]"));
        assert!(text.contains("secret add <name> | --for <host:port> [--replace | --if-missing]"));
        assert!(text.contains("use-hosted openrouter | --endpoint <url>"));
        assert!(text.contains("use-local --model <name>"));
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
            args(&["secret", "add", "search-brave"]),
            args(&["secret", "add", "search-brave", "--replace"]),
            args(&["secret", "add", "--for", "openrouter.ai:443"]),
            args(&["secret", "add", "--for", "openrouter.ai:443", "--replace"]),
            args(&["clean"]),
            args(&["clean", "--dry-run"]),
            args(&["room-disk", "close"]),
            args(&["doctor"]),
            args(&["doctor", "--env", "/etc/hatchling/hatchling.env"]),
            args(&[SKILL_CELL_SUBCOMMAND]),
            args(&[ROOM_MOUNT_SUBCOMMAND]),
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
            args(&["secret"]),
            args(&["secret", "add"]),
            args(&["secret", "list"]),
            args(&["secret", "add", "a", "b", "c"]),
            args(&["clean", "--force"]),
            args(&["clean", "--dry-run", "extra"]),
            args(&["room-disk"]),
            args(&["room-disk", "open"]),
            args(&["room-disk", "close", "extra"]),
            args(&["doctor", "--env"]),
            args(&["doctor", "--fix"]),
            args(&["doctor", "--env", "/a", "/b"]),
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
