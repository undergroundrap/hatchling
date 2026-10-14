//! CLI contract against the workspace binaries, including source-level drift.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[path = "support/help_audit.rs"]
mod help_audit;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);

impl Fixture {
    #[expect(
        clippy::unwrap_used,
        reason = "test fixture setup must fail the test if it cannot create its private files"
    )]
    fn new() -> Self {
        let root = Path::new("/tmp").join(format!(
            "hatchling-help-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("watch")).unwrap();
        fs::write(root.join("watch/canary"), b"unchanged help fixture").unwrap();
        Self(root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "Cargo supplies the integration test executable path"
)]
fn binary(name: &str) -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_hatchling-dev"))
        .parent()
        .unwrap()
        .join(name)
}

#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "bounded test subprocess supervision and explicit assertion failures"
)]
fn invoke(name: &str, args: &[&str], fixture: &Fixture, poisoned: bool) -> Output {
    let watch = fixture.0.join("watch");
    let stdout_path = fixture.0.join("stdout.log");
    let stderr_path = fixture.0.join("stderr.log");
    let mut command = Command::new(binary(name));
    command
        .args(args)
        .env_clear()
        .current_dir(&fixture.0)
        .env("HATCHLING_BROKER_SOCKET", watch.join("sockets/broker.sock"))
        .env("HATCHLING_VAULT_DIR", watch.join("vault"))
        .env("HATCHLING_VAULT_KEY_FILE", watch.join("keys/master.key"))
        .env(
            "HATCHLING_AGENT_UID",
            if poisoned { "invalid uid" } else { "65534" },
        )
        .env(
            "HATCHLING_BROKER_UID",
            if poisoned { "invalid uid" } else { "0" },
        )
        .env("HATCHLING_CELL_ID", "help-contract")
        .env("HOME", watch.join("home"))
        .env("HATCHLING_CARGO", watch.join("must-not-run-cargo"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(fs::File::create(&stdout_path).unwrap()))
        .stderr(Stdio::from(fs::File::create(&stderr_path).unwrap()));
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let status = child.wait().unwrap();
            panic!("{name} {args:?} started work instead of returning help/usage: {status:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = Output {
        status: child.wait().unwrap(),
        stdout: fs::read(stdout_path).unwrap(),
        stderr: fs::read(stderr_path).unwrap(),
    };
    let entries: BTreeSet<_> = fs::read_dir(&watch)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        entries,
        BTreeSet::from([std::ffi::OsString::from("canary")]),
        "{name} {args:?} touched a startup directory"
    );
    assert_eq!(
        fs::read(watch.join("canary")).unwrap(),
        b"unchanged help fixture"
    );
    output
}

#[test]
fn help_and_short_help_are_stdout_success_without_startup_side_effects() {
    let fixture = Fixture::new();
    for name in [
        "hatchling-broker",
        "hatchling-agent",
        "hatchling-dev",
        "hatchling",
    ] {
        for poisoned in [false, true] {
            for flag in ["--help", "-h"] {
                let output = invoke(name, &[flag], &fixture, poisoned);
                assert_eq!(output.status.code(), Some(0), "{name} {flag}: {output:?}");
                assert!(output.stderr.is_empty(), "{name}: {output:?}");
                assert!(String::from_utf8(output.stdout).unwrap().contains("Usage:"));
            }
        }
    }
    for command in ["broker", "agent"] {
        for flag in ["--help", "-h"] {
            let output = invoke("hatchling", &[command, flag], &fixture, true);
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            assert!(output.stderr.is_empty());
            let usage = format!("Usage: hatchling {command}");
            assert!(String::from_utf8(output.stdout).unwrap().contains(&usage));
        }
    }
}

#[test]
fn unknown_arguments_are_stderr_usage_with_exit_64() {
    let fixture = Fixture::new();
    for name in [
        "hatchling-broker",
        "hatchling-agent",
        "hatchling-dev",
        "hatchling",
    ] {
        for args in [
            &["--unknown"][..],
            &["--help", "--unknown"],
            &["-h", "extra"],
        ] {
            let output = invoke(name, args, &fixture, true);
            assert_eq!(
                output.status.code(),
                Some(64),
                "{name} {args:?}: {output:?}"
            );
            assert!(output.stdout.is_empty(), "{name}: {output:?}");
            assert!(String::from_utf8(output.stderr).unwrap().contains("Usage:"));
        }
    }
    for command in ["broker", "agent"] {
        let output = invoke("hatchling", &[command, "--unknown"], &fixture, true);
        assert_eq!(output.status.code(), Some(64), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr).unwrap().contains("Usage:"));
    }
    for args in [
        &["vault-keyring", "bogus"][..],
        &["redactions", "--unknown"],
        &["vault-audit", "--unknown"],
    ] {
        let output = invoke("hatchling-broker", args, &fixture, true);
        assert_eq!(output.status.code(), Some(64), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr).unwrap().contains("Usage:"));
    }
}

#[test]
fn malformed_vault_audit_counts_are_usage_before_configuration() {
    let fixture = Fixture::new();
    for count in ["--unknown", "-1", "", "184467440737095516160"] {
        for name in ["hatchling-broker", "hatchling"] {
            let args = if name == "hatchling" {
                vec!["broker", "vault-audit", "log", count]
            } else {
                vec!["vault-audit", "log", count]
            };
            let output = invoke(name, &args, &fixture, true);
            assert_eq!(output.status.code(), Some(64), "{name}: {output:?}");
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8(output.stderr).unwrap().contains("Usage:"));
        }
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "source-audit fixtures must fail if files cannot be read or parsed"
)]
fn source_files(root: &Path, files: &mut Vec<syn::File>) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap());
        }
    }
}

#[test]
fn broker_reads_are_listed_in_help() {
    let fixture = Fixture::new();
    let root = hatchling_dev::workspace_root();
    let mut missing = Vec::new();
    for (binary, minimum) in [
        ("hatchling-broker", 40),
        ("hatchling-agent", 9),
        ("hatchling-dev", 3),
    ] {
        let mut files = Vec::new();
        source_files(&root.join("crates").join(binary).join("src"), &mut files);
        let reads = help_audit::audit(&files);
        assert!(
            reads.unsupported.is_empty(),
            "{binary}: {:?}",
            reads.unsupported
        );
        assert!(
            reads.keys.len() >= minimum,
            "audit missed readers for {binary}: {:?}",
            reads.keys
        );
        let output = invoke(binary, &["--help"], &fixture, true);
        assert_eq!(output.status.code(), Some(0));
        let text = String::from_utf8(output.stdout).unwrap();
        for name in reads.keys {
            if !text.contains(&format!("  {name} (")) {
                missing.push(format!("{binary}: {name}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "environment reads missing from help: {missing:?}"
    );
}
