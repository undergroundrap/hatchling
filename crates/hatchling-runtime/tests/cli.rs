//! The real `hatchling` binary: each route reaches the command line it
//! names, with nothing configured, so no cell or broker is needed.

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Output};

const HATCHLING: &str = env!("CARGO_BIN_EXE_hatchling");

/// Runs `hatchling` with no `HATCHLING_*` configuration.
fn hatchling(argv0: Option<&str>, args: &[&str]) -> std::io::Result<Output> {
    let mut command = Command::new(HATCHLING);
    for (key, _) in std::env::vars() {
        if key.starts_with("HATCHLING_") {
            command.env_remove(key);
        }
    }
    if let Some(name) = argv0 {
        command.arg0(name);
    }
    command.args(args).output()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_and_version_exit_zero_on_stdout() {
    for flag in ["--help", "-h", "help"] {
        let out = hatchling(None, &[flag]).expect("hatchling runs");
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert!(
            stdout(&out).contains("hatchling broker"),
            "{flag}: {}",
            stdout(&out)
        );
        assert!(out.stderr.is_empty(), "{flag}");
    }
    for flag in ["--version", "-V"] {
        let out = hatchling(None, &[flag]).expect("hatchling runs");
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(
            stdout(&out),
            format!("hatchling {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn no_subcommand_or_an_unknown_one_exits_64_with_usage_on_stderr() {
    for args in [&[][..], &["serve"][..]] {
        let out = hatchling(None, args).expect("hatchling runs");
        assert_eq!(out.status.code(), Some(64), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(
            stderr(&out).contains("hatchling broker"),
            "{args:?}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn broker_reaches_the_brokers_command_line() {
    // With nothing configured, the broker's config parsing refuses,
    // under the front door's name.
    let out = hatchling(None, &["broker"]).expect("hatchling runs");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).starts_with("hatchling broker: "),
        "{}",
        stderr(&out)
    );
}

#[test]
fn agent_reaches_the_agents_command_line() {
    let out = hatchling(None, &["agent", "frobnicate"]).expect("hatchling runs");
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.starts_with("hatchling agent: failed to start: "),
        "{err}"
    );
    assert!(err.contains("usage: hatchling agent run"), "{err}");
}

#[test]
fn started_as_the_cell_program_it_is_the_agent() {
    let out = hatchling(Some("/agent"), &["frobnicate"]).expect("hatchling runs");
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.starts_with("hatchling agent: failed to start: "),
        "{err}"
    );
    assert!(err.contains("unknown subcommand \"frobnicate\""), "{err}");
}

#[test]
fn the_brokers_spawner_helpers_reach_the_broker() {
    // The broker re-runs its own executable with these; with no spec,
    // each helper refuses in its own words, which only the broker has.
    for helper in ["skill-cell", "agent-run"] {
        let out = hatchling(None, &[helper]).expect("hatchling runs");
        assert_eq!(out.status.code(), Some(1), "{helper}");
        let err = stderr(&out);
        assert!(
            err.contains(&format!("{helper} requires a cell spec")),
            "{helper}: {err}"
        );
    }
}
