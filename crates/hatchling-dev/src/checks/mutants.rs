//! Mutation-testing runner: scheduled / on-demand, never per-push.
//!
//! Runs `cargo mutants --workspace`, parses the outcome summary, and
//! compares the viable-mutant kill rate against
//! [`MUTANT_KILL_FLOOR`](crate::config::MUTANT_KILL_FLOOR). Timeouts
//! count as kills: the suite observably does not pass under those
//! mutants.

use std::path::Path;
use std::time::Duration;

use super::Finding;
use crate::Error;
use crate::config::MUTANT_KILL_FLOOR;
use crate::environment as env;

/// Bound for the agent prebuild (`cargo build -p hatchling-agent`,
/// ): a debug build of one binary, 10 minutes is generous even
/// on a cold cache.
const PREBUILD_TIMEOUT: Duration = Duration::from_secs(600);

/// Bound for the full mutation run (`cargo mutants --workspace`,
/// ): a normal full pass takes ~15 minutes; an hour is the
/// fail-closed outer bound. A DRIVER TIMEOUT is an error — the tool
/// never finished — while a non-zero exit is deliberately ignored
/// (timeouts count as kills; the verdict comes from the report).
const FULL_RUN_TIMEOUT: Duration = Duration::from_secs(3600);

/// Run the full mutation suite and report whether the kill floor held.
///
/// `cargo-mutants` copies the whole workspace into a scratch directory
/// under `TMPDIR` once per mutant. The system `/tmp` is a small tmpfs on
/// the machines this tool runs on; pointing `TMPDIR` at
/// [`scratch_dir`] keeps the run on the large filesystem and avoids
/// `ENOSPC` mid-run, which otherwise corrupts the outcome report (a
/// failed worker can masquerade as missed mutants).
///
/// The scratch directory lives under the workspace `target/` dir:
/// `cargo-mutants` excludes `target/` from its per-mutant tree copies,
/// so old scratch contents can never nest recursively into new copies
/// (which otherwise explodes into "file name too long" failures).
///
/// # Errors
///
/// Returns [`Error`](crate::Error) if the scratch directory cannot be
/// created, if `cargo mutants` cannot run, or if its `outcomes.json`
/// cannot be read and parsed.
pub fn run(root: &Path) -> Result<Vec<Finding>, Error> {
    let scratch = scratch_dir(root)?;
    std::fs::create_dir_all(&scratch).map_err(Error::Io)?;
    let outcomes_path = root.join("mutants.out").join("outcomes.json");
    // A crashed or killed run must never be evaluated against a stale
    // report from an earlier run.
    let _ = std::fs::remove_file(&outcomes_path);
    // Cell boundary integration tests spawn the real agent binary. Build
    // it in the source tree first, then point the scratch tests at it via
    // HATCHLING_AGENT_BIN. The test prefers a binary next to its own
    // executable (the mutated scratch build during hatchling-agent
    // mutation runs) and uses this env var only as a fallback for runs
    // whose scratch tree never builds the binary.
    let agent_bin = root.join("target").join("debug").join("hatchling-agent");
    // Bounded prebuild: the exit status is deliberately
    // ignored — a failed prebuild surfaces through the outcome report
    // — but a driver-level error (spawn failure, timeout) aborts the
    // run.
    super::driver::drive(&super::driver::Spec {
        program: "cargo",
        args: &["build", "-p", "hatchling-agent", "--offline"],
        current_dir: Some(root),
        env: &[],
        timeout: PREBUILD_TIMEOUT,
    })?;
    // Bounded full run: the exit status is deliberately
    // ignored (cargo-mutants exits non-zero when mutants time out,
    // and timeouts count as kills — module docs). The verdict comes
    // from the outcome report; a missing report means the tool never
    // finished, which is the real failure. A DRIVER TIMEOUT is also
    // an error for the same reason.
    super::driver::drive(&super::driver::Spec {
        program: "cargo",
        args: &["mutants", "--workspace"],
        current_dir: Some(root),
        env: &[
            ("TMPDIR", scratch.as_path()),
            ("HATCHLING_AGENT_BIN", agent_bin.as_path()),
        ],
        timeout: FULL_RUN_TIMEOUT,
    })?;
    // A missing report means the tool never finished, which is the
    // real failure.
    if !outcomes_path.exists() {
        return Err(Error::ToolFailed {
            tool: String::from("cargo mutants"),
            status: String::from("no outcomes.json written"),
        });
    }
    let summary = read_summary(root)?;
    let missed = missed_names(root)?;
    Ok(evaluate(&summary, missed))
}

/// Kill verdict from a parsed outcome summary: [`Vec<Finding>`] is empty
/// exactly when the kill floor holds. Split out so the timeout-as-kill
/// policy is unit-testable without spawning the tool.
///
/// `pub(crate)` so the content-hash cache runner
/// ([`super::mutant_cache`]) evaluates its merged report with the same
/// policy as the full run.
pub(crate) fn evaluate(summary: &Summary, missed: Vec<String>) -> Vec<Finding> {
    let viable = summary.total.saturating_sub(summary.unviable);
    let killed = summary.caught.saturating_add(summary.timeout);
    let mut findings = Vec::new();
    if below_floor(killed, viable) {
        findings.push(Finding::new(
            "mutants",
            format!(
                "kill rate below the floor {MUTANT_KILL_FLOOR:.2} \
                 ({killed}/{viable} viable mutants killed; {} missed)",
                summary.missed,
            ),
        ));
        for name in missed {
            findings.push(Finding::new("mutants", format!("missed mutant: {name}")));
        }
    }
    findings
}

/// Scratch directory for `cargo-mutants`' per-mutant workspace copies.
///
/// `$HOME/mutants.scratch`: disk-backed and outside the repository.
/// Never the system temp dir (which may be a small tmpfs), never the
/// workspace root or its `target/` (cargo-mutants copies the workspace
/// into the scratch dir and would recurse into itself). Split out so
/// the location is unit-testable: the tool must never silently fall
/// back to `/tmp` or to a repository-local directory.
///
/// `pub(crate)` so the cache runner uses the same scratch location.
pub(crate) fn scratch_dir(_root: &Path) -> Result<std::path::PathBuf, Error> {
    // Mutation scratch must be disk-backed and outside the repository:
    // `$HOME/mutants.scratch`. Never under `target/` (cargo-mutants
    // copies the workspace into TMPDIR and would recurse into itself)
    // and never under `/tmp` (tmpfs, ENOSPC on large runs).
    // Validated HOME lookup, no deprecated APIs.
    let home = std::env::var(env::HOME.name).map_err(|_| Error::MissingEnv("HOME"))?;
    if home.is_empty() {
        return Err(Error::MissingEnv("HOME"));
    }
    Ok(std::path::PathBuf::from(home).join("mutants.scratch"))
}

/// Whether `killed` of `viable` viable mutants is below
/// [`MUTANT_KILL_FLOOR`]. Split out so the boundary is unit-testable:
/// exactly-at-the-floor passes, one below fails.
fn below_floor(killed: u32, viable: u32) -> bool {
    f64::from(killed) < MUTANT_KILL_FLOOR * f64::from(viable)
}

#[derive(Debug, Default)]
pub(crate) struct Summary {
    pub(crate) total: u32,
    pub(crate) caught: u32,
    pub(crate) missed: u32,
    pub(crate) timeout: u32,
    pub(crate) unviable: u32,
}

/// `pub(crate)` so the cache runner can evaluate its merged report.
pub(crate) fn read_summary(root: &Path) -> Result<Summary, Error> {
    let path = root.join("mutants.out").join("outcomes.json");
    let text = std::fs::read_to_string(&path).map_err(Error::Io)?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Parse {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    // Counts saturate at u32::MAX; real suites never come close.
    let get = |key: &str| {
        u32::try_from(
            json.get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        )
        .unwrap_or(u32::MAX)
    };
    Ok(Summary {
        total: get("total_mutants"),
        caught: get("caught"),
        missed: get("missed"),
        timeout: get("timeout"),
        unviable: get("unviable"),
    })
}

/// Names of the missed mutants, for the report.
///
/// `pub(crate)` so the cache runner can report misses from its merged
/// report.
pub(crate) fn missed_names(root: &Path) -> Result<Vec<String>, Error> {
    let path = root.join("mutants.out").join("outcomes.json");
    let text = std::fs::read_to_string(&path).map_err(Error::Io)?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Parse {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    let mut names = Vec::new();
    if let Some(outcomes) = json.get("outcomes").and_then(|o| o.as_array()) {
        for outcome in outcomes {
            let is_missed = outcome
                .get("summary")
                .and_then(|s| s.as_str())
                .is_some_and(|s| s == "MissedMutant");
            if !is_missed {
                continue;
            }
            let name = outcome
                .pointer("/scenario/Mutant/name")
                .and_then(|n| n.as_str())
                .unwrap_or("<unnamed mutant>");
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_root(name: &str) -> std::path::PathBuf {
        // Never `std::env::temp_dir()`: it follows TMPDIR, and under
        // `cargo mutants` TMPDIR points inside the workspace scratch dir.
        // `cargo mutants --workspace` walks *up* from its working dir, so a
        // temp_dir()-based fake root is actually inside the real workspace:
        // the tool finds the real Cargo.toml and starts a nested full
        // mutation run against the same `mutants.out`, wedging the outer
        // run's baseline forever (observed 2026-09-22). `/tmp` directly can
        // never be inside a workspace; the dir stays empty so the small
        // tmpfs is no concern (unlike the per-mutant tree copies, which is
        // why `scratch_dir` must never fall back here).
        let root =
            Path::new("/tmp").join(format!("hatchling-dev-mut-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn fake_root_is_not_inside_a_cargo_workspace() {
        // Pins the precondition every `run()` test relies on: no ancestor
        // of the fake root may contain a Cargo.toml, or the spawned tool
        // resolves the real workspace instead of failing. Kills a
        // `temp_dir()`-based `fake_root` when TMPDIR points into the
        // workspace (exactly the `cargo mutants` baseline case).
        let root = fake_root("workspace-check");
        let mut dir: Option<&Path> = Some(&root);
        while let Some(d) = dir {
            assert!(
                !d.join("Cargo.toml").exists(),
                "fake root {root:?} sits inside a cargo workspace"
            );
            dir = d.parent();
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scratch_dir_lives_under_the_workspace_root() {
        // The whole point of `scratch_dir` is that `cargo-mutants` never
        // uses the system temp dir NOR the repository-local target/.
        // It must be `$HOME/mutants.scratch`: disk-backed and outside
        // the repository, so tree copies never nest scratch inside
        // scratch. A mutant that returns `temp_dir()` or a
        // repository-local path fails here.
        let Ok(home) = std::env::var(env::HOME.name) else {
            // Without HOME the function errors; nothing to assert.
            return;
        };
        let root = Path::new("/workspace/hatchling");
        let scratch = scratch_dir(root).unwrap();
        assert_eq!(
            scratch,
            std::path::PathBuf::from(&home).join("mutants.scratch")
        );
        // Must NOT be under the workspace root.
        assert!(
            !scratch.starts_with(root),
            "scratch must be outside the repository, got: {scratch:?}"
        );
    }

    #[test]
    fn run_fails_when_the_tool_cannot_run() {
        // `cargo mutants` cannot succeed in a directory that is not a
        // cargo workspace; the failure must surface as an error, never
        // as an empty finding list. Kills `run -> Ok(vec![])` and the
        // dropped `!` on the status check.
        let root = fake_root("no-workspace");
        let err = run(&root).unwrap_err();
        assert!(
            err.to_string().contains("cargo mutants"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn below_floor_boundary_cases() {
        // The floor is 1.0: every viable mutant must be killed.
        // Each assertion pins one operator mutant on the comparison.
        assert!(below_floor(9, 10)); // `<` -> `>`: 9 > 10 is false
        assert!(!below_floor(10, 10)); // `<` -> `==`/`<=`: equality must pass
        assert!(below_floor(1, 2)); // `*` -> `/`: 1 < 0.5 is false
        assert!(!below_floor(2, 2)); // `*` -> `+`: 2 < 3.0 is true
        assert!(!below_floor(0, 0)); // vacuous: no viable mutants
    }

    #[test]
    fn timeout_counts_as_kill() {
        // 1 caught + 1 timeout of 2 viable = 1.0: at the floor, no
        // findings. This is the policy that lets the gate pass when
        // cargo-mutants exits non-zero on timeouts.
        let summary = Summary {
            total: 3,
            caught: 1,
            missed: 0,
            timeout: 1,
            unviable: 1,
        };
        assert!(evaluate(&summary, Vec::new()).is_empty());
    }

    #[test]
    fn missed_mutant_below_floor_is_reported() {
        // 1 caught of 2 viable = 0.5: below the floor, and the missed
        // mutant is named in the findings.
        let summary = Summary {
            total: 3,
            caught: 1,
            missed: 1,
            timeout: 0,
            unviable: 1,
        };
        let findings = evaluate(&summary, vec![String::from("the-missed-one")]);
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("the-missed-one")),
            "missed mutant not reported: {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("below the floor")),
            "floor breach not reported: {findings:?}"
        );
    }

    /// Writes a synthetic `mutants.out/outcomes.json` and returns the
    /// fake root.
    fn synthetic_outcomes(name: &str, body: &str) -> std::path::PathBuf {
        let root = fake_root(name);
        std::fs::create_dir_all(root.join("mutants.out")).unwrap();
        std::fs::write(root.join("mutants.out/outcomes.json"), body).unwrap();
        root
    }

    #[test]
    fn read_summary_parses_counts() {
        // A `read_summary -> Ok(Default::default())` mutant returns all
        // zeros and fails here.
        let root = synthetic_outcomes(
            "summary",
            r#"{"total_mutants": 10, "caught": 7, "missed": 2, "timeout": 1, "unviable": 3, "outcomes": []}"#,
        );
        let summary = read_summary(&root).unwrap();
        assert_eq!(summary.total, 10);
        assert_eq!(summary.caught, 7);
        assert_eq!(summary.missed, 2);
        assert_eq!(summary.timeout, 1);
        assert_eq!(summary.unviable, 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missed_names_collects_only_missed() {
        // One missed, one caught, one timeout: only the missed name
        // comes back. Kills the `missed_names -> Ok(...)` mutants, the
        // `==` -> `!=`, and the dropped `!` on the filter.
        let root = synthetic_outcomes(
            "names",
            r#"{
                "outcomes": [
                    {"summary": "MissedMutant", "scenario": {"Mutant": {"name": "the-missed-one"}}},
                    {"summary": "CaughtMutant", "scenario": {"Mutant": {"name": "the-caught-one"}}},
                    {"summary": "TimeoutMutant", "scenario": {"Mutant": {"name": "the-timeout-one"}}}
                ]
            }"#,
        );
        let names = missed_names(&root).unwrap();
        assert_eq!(names, vec![String::from("the-missed-one")]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
