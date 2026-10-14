//! The real `hatchling-dev preflight` binary against a fixture
//! repository: its arguments, `--root`, its output and its exit code.
//!
//! The fixture lives under `/tmp` directly, never
//! `std::env::temp_dir()`: under `cargo mutants` that follows `TMPDIR`
//! into the workspace's scratch copy.

#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const fn dev_binary() -> &'static str {
    env!("CARGO_BIN_EXE_hatchling-dev")
}

/// A scratch git repository, removed on drop.
struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Self {
        let root = Path::new("/tmp").join(format!(
            "hatchling-dev-preflight-bin-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let repo = Self { root };
        repo.git(&["init", "-q"]);
        repo
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn commit(&self) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "test(fixture): snapshot"]);
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// The binary, run from elsewhere: only `--root` points it here.
    fn preflight(&self, base: &str) -> Output {
        Command::new(dev_binary())
            .args(["preflight", "--base", base, "--root"])
            .arg(&self.root)
            .current_dir("/")
            .output()
            .unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const BASE: &str = r"pub fn inner() -> u32 {
    1
}

#[test]
fn inner_is_one() {
    assert_eq!(inner(), 1);
}
";

const LYING: &str = r"pub fn wrapper() -> u32 {
    inner()
}

pub fn inner() -> u32 {
    1
}

#[test]
fn inner_is_one() {
    // Mutants: `wrapper` returning 0.
    assert_eq!(inner(), 1);
}
";

const HONEST: &str = r"pub fn wrapper() -> u32 {
    inner()
}

pub fn inner() -> u32 {
    1
}

#[test]
fn inner_is_one() {
    // Mutants: `wrapper` returning 0.
    assert_eq!(wrapper(), 1);
    assert_eq!(inner(), 1);
}
";

#[test]
fn preflight_names_each_finding_and_fails() {
    let repo = Repo::new("fails");
    repo.write("crates/demo/src/lib.rs", BASE);
    let base = repo.commit();
    repo.write("crates/demo/src/lib.rs", LYING);
    let output = repo.preflight(&base);
    assert!(!output.status.success(), "a finding must fail the gate");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            "[mutant-comment] crates/demo/src/lib.rs:11: `// Mutants:` names `wrapper`, but `inner_is_one` never calls it",
            "[delegation] crates/demo/src/lib.rs:1: `wrapper` changed, but no test calls it directly",
        ]
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("2 finding(s)"), "stderr was: {stderr}");
}

#[test]
fn preflight_passes_a_clean_change() {
    let repo = Repo::new("passes");
    repo.write("crates/demo/src/lib.rs", BASE);
    let base = repo.commit();
    repo.write("crates/demo/src/lib.rs", HONEST);
    let output = repo.preflight(&base);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout was: {stdout}");
    assert_eq!(stdout.trim(), "hatchling-dev preflight: all checks passed");
}

#[test]
fn preflight_without_a_base_is_a_usage_error() {
    let output = Command::new(dev_binary())
        .arg("preflight")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("Usage: hatchling-dev "),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("preflight --base <commit>"),
        "stderr was: {stderr}"
    );
}
