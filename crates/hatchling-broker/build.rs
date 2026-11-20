//! Records how the broker was built, and from which commit, for
//! `--version` (`src/build_kind.rs` decides; `src/version.rs` says it).
//!
//! Only a release-profile build reads anything: every other build records
//! no commit, runs no git and is never rerun for a commit, so tests, checks
//! and mutation runs cost nothing more. A release-profile build is rerun
//! when the commit or any file it's built from changes, so it never says
//! "unmodified" of a tree edited after it ran.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

// The lib uses what this doesn't, and its items are `pub(crate)` there,
// where `pub` would be unreachable.
#[allow(dead_code, clippy::redundant_pub_crate)]
#[path = "src/build_kind.rs"]
mod build_kind;

use build_kind::{COMMIT_VAR, Checkout, KIND_VAR, RELEASE_VAR, Seen, decide};

fn main() -> ExitCode {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "cargo:rerun-if-changed=build.rs");
    let _ = writeln!(out, "cargo:rerun-if-env-changed={RELEASE_VAR}");
    let release_profile = std::env::var("PROFILE").is_ok_and(|profile| profile == "release");
    let release_build = std::env::var(RELEASE_VAR).is_ok_and(|value| value == "release");
    let here = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let package = release_profile
        .then(|| std::fs::read_to_string(here.join(".cargo_vcs_info.json")).ok())
        .flatten();
    let read = (release_profile && package.is_none())
        .then(|| read_checkout(&here, &mut out))
        .flatten();
    let seen = Seen {
        release_profile,
        release_build,
        package: package.as_deref(),
        checkout: read.as_ref().map(|(prefix, head, modified)| Checkout {
            prefix,
            head,
            modified: *modified,
        }),
    };
    match decide(&seen) {
        Ok((kind, commit)) => {
            let _ = writeln!(out, "cargo:rustc-env={KIND_VAR}={}", kind.name());
            let _ = writeln!(out, "cargo:rustc-env={COMMIT_VAR}={}", commit.unwrap_or(""));
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(std::io::stderr().lock(), "hatchling-broker: {why}");
            ExitCode::FAILURE
        }
    }
}

/// The checkout the package is in: its prefix, its commit, and whether its
/// tracked files differ from it. Each file that changes any of these reruns
/// the build script.
fn read_checkout(here: &Path, out: &mut impl std::io::Write) -> Option<(String, String, bool)> {
    let prefix = git(here, &["rev-parse", "--show-prefix"])?;
    let head = git(here, &["rev-parse", "HEAD"])?;
    let top = PathBuf::from(git(here, &["rev-parse", "--show-toplevel"])?);
    let mut watched: Vec<PathBuf> = ["crates", "Cargo.toml", "Cargo.lock"]
        .iter()
        .map(|path| top.join(path))
        .collect();
    let mut git_paths = vec![
        String::from("HEAD"),
        String::from("index"),
        String::from("packed-refs"),
    ];
    git_paths.extend(
        git(here, &["rev-parse", "--symbolic-full-name", "HEAD"])
            .filter(|name| name.starts_with("refs/")),
    );
    for name in git_paths {
        if let Some(path) = git(
            here,
            &["rev-parse", "--path-format=absolute", "--git-path", &name],
        ) {
            watched.push(PathBuf::from(path));
        }
    }
    // A path that doesn't exist would rerun every build; a ref packed
    // later deletes the loose one, which reruns it then.
    for path in watched.iter().filter(|path| path.exists()) {
        let _ = writeln!(out, "cargo:rerun-if-changed={}", path.display());
    }
    let status = git(here, &["status", "--porcelain", "--untracked-files=no"])?;
    Some((prefix, head, !status.is_empty()))
}

/// A git command's output, trimmed, or `None` when it fails.
fn git(here: &Path, args: &[&str]) -> Option<String> {
    // Without optional locks, `status` never rewrites the index, which
    // would rerun the next build.
    let output = Command::new("git")
        .args(args)
        .current_dir(here)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|text| text.trim().to_owned())
}
