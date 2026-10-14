//! Smoke: the real `hatchling-dev` binary's command dispatch.
//!
//! Spawning the binary (instead of calling library functions) proves
//! `main` wires argument parsing, dispatch, and the exit-code contract
//! together. `env!("CARGO_BIN_EXE_hatchling-dev")` is a build
//! dependency of this test target, so cargo always builds the binary
//! during `cargo test` — it can never be stale.

use std::process::Command;

const fn dev_binary() -> &'static str {
    env!("CARGO_BIN_EXE_hatchling-dev")
}

#[test]
fn audit_passes_on_this_tree() {
    let output = Command::new(dev_binary()).arg("audit").output().unwrap();
    assert!(
        output.status.success(),
        "audit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("all checks passed"), "stdout was: {stdout}");
}

#[test]
fn unknown_command_fails_closed() {
    // An unknown command must exit non-zero. Pins `main`'s wiring: a
    // `main` that always succeeds, or a `run` that always returns
    // `Ok(())`, exits 0 and fails here.
    let output = Command::new(dev_binary())
        .arg("no-such-command")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("Usage: hatchling-dev "),
        "stderr was: {stderr}"
    );
}
