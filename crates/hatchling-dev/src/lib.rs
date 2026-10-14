//! Development-only audit and conformance tooling.
//!
//! This crate is never linked into runtime code: the dependency-direction
//! check (`checks::dep_direction`) fails the gate if any other workspace
//! member depends on it. Fast checks run as unit tests inside this crate,
//! so they execute in the normal per-push gate; the slow checks
//! (mutation testing, coverage) run via the `hatchling-dev` binary on a
//! schedule or on demand.

pub mod checks;
pub mod cli;
pub mod config;
pub mod environment;
pub mod error;

pub use error::Error;

use std::path::{Path, PathBuf};

/// Workspace root, anchored at compile time so the checks work no
/// matter which directory the test runner or binary is invoked from.
#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Dispatch a CLI command to its checks.
///
/// `audit` runs the fast checks; `mutants` and `coverage` run the slow
/// scheduled checks; `mutants-cached` runs the mutation gate through
/// the content-hash cache; `seed-cache --from <outcomes.json> --out
/// <seed.json>` builds a sanitized cache seed from an official
/// baseline; `protocol-schema [--write]` checks (or regenerates) the
/// frontend protocol schema ; `preflight --base <commit>
/// [--root <dir>]` runs the
/// mutation pre-flight gate on the changes since `<commit>`.
///
/// # Errors
///
/// Returns [`Error::ToolFailed`](crate::Error::ToolFailed) for an
/// unknown command, and [`Error`](crate::Error) if a check itself
/// could not run. Check *violations* come back as the `Vec<Finding>`,
/// not as an error.
pub fn dispatch(
    command: &str,
    args: &[String],
    root: &Path,
) -> Result<Vec<checks::Finding>, Error> {
    match command {
        "audit" => {
            let mut all = Vec::new();
            all.extend(checks::dep_direction::run(root)?);
            all.extend(checks::pins::run(root)?);
            all.extend(checks::arch::run(root)?);
            Ok(all)
        }
        "mutants" => checks::mutants::run(root),
        "mutants-cached" => checks::mutant_cache::run(root),
        "seed-cache" => {
            let from = flag(args, "--from").ok_or_else(|| Error::ToolFailed {
                tool: String::from("seed-cache"),
                status: String::from("missing --from <outcomes.json>"),
            })?;
            let out = flag(args, "--out").ok_or_else(|| Error::ToolFailed {
                tool: String::from("seed-cache"),
                status: String::from("missing --out <seed.json>"),
            })?;
            checks::mutant_cache::seed_cache(root, Path::new(from), Path::new(out))?;
            Ok(Vec::new())
        }
        "coverage" => checks::coverage::run(root),
        "preflight" => {
            let base = flag(args, "--base").ok_or_else(|| Error::ToolFailed {
                tool: String::from("preflight"),
                status: String::from("missing --base <commit>"),
            })?;
            let root = flag(args, "--root").map_or_else(|| root.to_path_buf(), PathBuf::from);
            checks::preflight::run(&root, base)
        }
        "protocol-schema" => {
            checks::protocol_schema::run(root, args.iter().any(|arg| arg == "--write"))
        }
        // Binary-only: verifies the fixed Python-generated fixture seed
        // against the fixed fixture tree, pinning Python<->Rust
        // hash-scheme equivalence. It reads no live-tree files, so no
        // task can break it (and under cargo-mutants the fixture
        // is never mutated — it is data, not a workspace member).
        // The check.sh gate runs it.
        "seed-check" => checks::mutant_cache::check_seed_consistency(root),
        other => Err(Error::ToolFailed {
            tool: format!(
                "unknown command `{other}` (expected audit|mutants|mutants-cached|seed-cache|coverage|preflight|protocol-schema|seed-check)"
            ),
            status: String::from("usage error"),
        }),
    }
}

/// Value of `--name value` in an argument list.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let mut iter = args.iter();
    for arg in iter.by_ref() {
        if arg == name {
            return iter.next().map(String::as_str);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_rejects_unknown_commands() {
        let root = workspace_root();
        let err = dispatch("bogus", &[], &root).unwrap_err();
        assert!(err.to_string().contains("unknown command"));
    }

    #[test]
    fn dispatch_audit_passes_on_this_tree() {
        let root = workspace_root();
        assert!(dispatch("audit", &[], &root).unwrap().is_empty());
    }

    #[test]
    fn dispatch_seed_cache_requires_flags() {
        // Missing --from/--out is a usage error, not a silent no-op.
        let root = workspace_root();
        let err = dispatch("seed-cache", &[], &root).unwrap_err();
        assert!(
            err.to_string().contains("--from"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn dispatch_protocol_schema_checks_the_tree() {
        // The checked-in schema is current, so the check finds nothing.
        let root = workspace_root();
        assert!(dispatch("protocol-schema", &[], &root).unwrap().is_empty());
    }

    #[test]
    fn dispatch_protocol_schema_writes_only_when_asked() {
        // `/tmp` directly: under cargo-mutants `temp_dir()` follows TMPDIR
        // into the workspace's scratch copy.
        let root = Path::new("/tmp").join(format!(
            "hatchling-dev-dispatch-schema-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let schema = root.join(checks::protocol_schema::SCHEMA_PATH);
        let other = [String::from("--verbose")];
        let findings = dispatch("protocol-schema", &other, &root).unwrap();
        assert_eq!(
            findings.len(),
            1,
            "check mode names the missing schema: {findings:?}"
        );
        assert!(!schema.exists(), "check mode never writes");
        let write = [String::from("--write")];
        assert!(
            dispatch("protocol-schema", &write, &root)
                .unwrap()
                .is_empty()
        );
        assert!(schema.exists(), "--write writes it");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dispatch_preflight_requires_a_base() {
        // `--root` is pinned against a fixture repository by
        // `tests/preflight.rs`, through the real binary.
        let root = workspace_root();
        let err = dispatch("preflight", &[], &root).unwrap_err();
        assert!(
            err.to_string().contains("missing --base"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn flag_value_cases() {
        // Kills: ==/!= inversion and next()/map() mutants in `flag`.
        let args = vec![
            String::from("--from"),
            String::from("a.json"),
            String::from("--out"),
            String::from("b.json"),
        ];
        // Present: returns the following value.
        assert_eq!(flag(&args, "--from"), Some("a.json"));
        assert_eq!(flag(&args, "--out"), Some("b.json"));
        // Missing: returns None.
        assert_eq!(flag(&args, "--missing"), None);
        assert_eq!(flag(&[], "--from"), None);
        // Flag at end with no value: returns None, not a panic.
        let trailing = vec![String::from("--from")];
        assert_eq!(flag(&trailing, "--from"), None);
        // First occurrence wins; a later same-named flag is not consulted.
        let repeated = vec![
            String::from("--from"),
            String::from("first.json"),
            String::from("--from"),
            String::from("second.json"),
        ];
        assert_eq!(flag(&repeated, "--from"), Some("first.json"));
    }
}
