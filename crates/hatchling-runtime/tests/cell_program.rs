//! `hatchling-cell`, the program the broker runs in each cell: it is the
//! agent's command line, and it carries none of the trusted side's code.

use std::process::Command;

const CELL: &str = env!("CARGO_BIN_EXE_hatchling-cell");
const HATCHLING: &str = env!("CARGO_BIN_EXE_hatchling");

/// The crates that hold keys, the network or the cell machinery, as they
/// appear in a binary's symbol names.
const TRUSTED_SIDE: [&str; 5] = [
    "hatchling_broker",
    "hatchling_vault",
    "hatchling_egress",
    "hatchling_isolation",
    "hatchling_fido2",
];

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn hatchling_cell_is_the_agents_command_line() {
    let mut command = Command::new(CELL);
    for (key, _) in std::env::vars() {
        if key.starts_with("HATCHLING_") {
            command.env_remove(key);
        }
    }
    let out = command
        .arg("frobnicate")
        .output()
        .expect("hatchling-cell runs");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("hatchling-cell: failed to start: "),
        "{err}"
    );
    assert!(err.contains("usage: hatchling-cell run"), "{err}");
}

#[test]
fn hatchling_cell_carries_none_of_the_trusted_sides_code() {
    let cell = std::fs::read(CELL).expect("read hatchling-cell");
    for name in TRUSTED_SIDE {
        assert!(!contains(&cell, name), "hatchling-cell contains `{name}`");
    }
    // The control: the all-in-one binary does carry the broker, so the
    // search would see it if it were there.
    let hatchling = std::fs::read(HATCHLING).expect("read hatchling");
    assert!(contains(&hatchling, "hatchling_broker"));
}
