//! `hatchling-runtime`: the `hatchling` command.
//!
//! One binary for the whole runtime: `hatchling broker` runs the broker
//! and `hatchling agent` runs the agent, each with exactly the command
//! line of `hatchling-broker` and `hatchling-agent`. Install it with
//! `cargo install hatchling-runtime`.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! Two invocations don't name a subcommand, because the broker makes
//! them itself:
//! - The broker starts its spawner helpers (`skill-cell`, `agent-run`,
//!   `browser-cell`) by re-running its own executable, which is `hatchling` here.
//! - A cell runs its program as `/agent`, so `hatchling` started under
//!   that name is the agent. Point `HATCHLING_AGENT_BIN` at `hatchling`
//!   and the one binary serves both sides.
//!
//! The routing is [`route`], a pure function, so it's tested without
//! starting anything.

use std::path::Path;

use hatchling_broker::AGENT_RUN_SUBCOMMAND;
use hatchling_broker::browser_cell::BROWSER_CELL_SUBCOMMAND;
use hatchling_broker::egress_authority::EGRESS_AUTHORITY_SUBCOMMAND;
use hatchling_broker::harness_tree::HARNESS_TREE_SUBCOMMAND;
use hatchling_broker::keeper::VAULT_KEEPER_SUBCOMMAND;
use hatchling_broker::skill_run::SKILL_CELL_SUBCOMMAND;
use hatchling_isolation::PROGRAM;

/// Usage, printed for `--help` on stdout and for a usage error on stderr.
pub const USAGE: &str = "\
hatchling: personal AI agents in kernel-isolated cells

Usage:
  hatchling broker [subcommand]   serve agents, or run one operator subcommand
  hatchling agent [run \"task\"]    run the agent (the broker starts it inside a cell)
  hatchling --help                print this help
  hatchling --version             print the version

Broker subcommands (run instead of serving):
  migrate-vault, rekey-vault [<new-key-file>], vault-keyring <action>,
  redactions <action>, vault-audit

The broker is configured by HATCHLING_* environment variables; see the
README's quickstart. Set HATCHLING_AGENT_BIN to this binary's path to run
the agent from it too.";

/// Front-door help includes both command lines' generated variable inventories.
#[must_use]
pub fn help() -> String {
    format!(
        "{USAGE}\n\n{}\n{}",
        hatchling_broker::cli::help("hatchling broker"),
        hatchling_agent::cli::help("hatchling agent")
    )
}

/// What one invocation of `hatchling` runs.
#[derive(Debug, PartialEq, Eq)]
pub enum Route<'a> {
    /// The broker's command line, with these arguments.
    Broker(&'a [String]),
    /// The agent's command line, with these arguments.
    Agent(&'a [String]),
    /// `--help`: usage on stdout, exit 0.
    Help,
    /// `--version`: the version on stdout, exit 0.
    Version,
    /// No subcommand, or an unknown one: usage on stderr, exit 64.
    Usage,
}

/// Routes one invocation: `argv0` is the name it was started under, and
/// `args` the arguments after it.
#[must_use]
pub fn route<'a>(argv0: &str, args: &'a [String]) -> Route<'a> {
    if Path::new(argv0).file_name() == Path::new(PROGRAM).file_name() {
        return Route::Agent(args);
    }
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("broker") => Route::Broker(rest),
        Some("agent") => Route::Agent(rest),
        Some(
            SKILL_CELL_SUBCOMMAND
            | AGENT_RUN_SUBCOMMAND
            | BROWSER_CELL_SUBCOMMAND
            | HARNESS_TREE_SUBCOMMAND
            | VAULT_KEEPER_SUBCOMMAND
            | EGRESS_AUTHORITY_SUBCOMMAND,
        ) => Route::Broker(args),
        Some("--help" | "-h" | "help") if args.len() == 1 => Route::Help,
        Some("--version" | "-V") if args.len() == 1 => Route::Version,
        Some(_) | None => Route::Usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// Everything after the subcommand.
    fn tail(given: &[String]) -> &[String] {
        given.get(1..).unwrap_or_default()
    }

    #[test]
    fn subcommands_route_to_their_command_line_without_the_subcommand() {
        let given = args(&["broker", "rekey-vault", "/new.key"]);
        assert_eq!(route("hatchling", &given), Route::Broker(tail(&given)));
        let given = args(&["agent", "run", "Say hi."]);
        assert_eq!(
            route("/usr/local/bin/hatchling", &given),
            Route::Agent(tail(&given))
        );
        let given = args(&["broker"]);
        assert_eq!(route("hatchling", &given), Route::Broker(&[]));
    }

    #[test]
    fn the_brokers_spawner_helpers_reach_the_broker_with_their_subcommand() {
        for helper in [
            SKILL_CELL_SUBCOMMAND,
            AGENT_RUN_SUBCOMMAND,
            BROWSER_CELL_SUBCOMMAND,
            HARNESS_TREE_SUBCOMMAND,
            VAULT_KEEPER_SUBCOMMAND,
            EGRESS_AUTHORITY_SUBCOMMAND,
        ] {
            let given = args(&[helper, "/run/spec"]);
            assert_eq!(
                route("/usr/local/bin/hatchling", &given),
                Route::Broker(&given)
            );
        }
    }

    #[test]
    fn started_as_the_cell_program_it_is_the_agent_with_every_argument() {
        let given = args(&["run", "Say hi."]);
        assert_eq!(route(PROGRAM, &given), Route::Agent(&given));
        assert_eq!(route(PROGRAM, &[]), Route::Agent(&[]));
        // Even an argument that names a subcommand stays the agent's.
        let given = args(&["broker"]);
        assert_eq!(route(PROGRAM, &given), Route::Agent(&given));
    }

    #[test]
    fn help_and_version_have_their_short_and_long_forms() {
        for flag in ["--help", "-h", "help"] {
            assert_eq!(route("hatchling", &args(&[flag])), Route::Help);
        }
        for flag in ["--version", "-V"] {
            assert_eq!(route("hatchling", &args(&[flag])), Route::Version);
        }
        for flag in ["--help", "-h", "help", "--version", "-V"] {
            assert_eq!(route("hatchling", &args(&[flag, "extra"])), Route::Usage);
        }
        let text = help();
        assert!(text.contains("Usage: hatchling broker"));
        assert!(text.contains("Usage: hatchling agent"));
        assert!(text.contains("HATCHLING_CELL_CGROUP"));
        assert!(text.contains("HATCHLING_MODEL_ENDPOINT"));
    }

    #[test]
    fn nothing_or_an_unknown_subcommand_is_a_usage_error() {
        assert_eq!(route("hatchling", &[]), Route::Usage);
        assert_eq!(route("hatchling", &args(&["serve"])), Route::Usage);
        assert_eq!(route("hatchling", &args(&["migrate-vault"])), Route::Usage);
    }

    #[test]
    fn a_name_that_only_ends_in_agent_is_not_the_cell_program() {
        assert_eq!(
            route("/usr/bin/hatchling-agent", &args(&["broker"])),
            Route::Broker(&[])
        );
        assert_eq!(route("secret-agent", &[]), Route::Usage);
    }
}
