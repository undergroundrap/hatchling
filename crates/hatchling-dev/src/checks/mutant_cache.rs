//! Content-hash mutation-result cache.
//!
//! The full mutation run is the reference measurement, but re-running
//! every mutant on every task is wasteful: most tasks touch
//! one crate, and the other crates' verdicts cannot have changed. This
//! module keys each mutated source file by everything that can affect
//! its mutants' verdicts and reuses stored verdicts when the key is
//! byte-identical to a previous clean, complete run. The kill floor
//! stays exactly 1.0: the floor is evaluated on the merged report, and
//! the cache can only replay verdicts, never invent them.
//!
//! Key components per file (normative scheme):
//!
//! - `file_hash`: SHA-256 of the file's bytes.
//! - `suite_hash`: SHA-256 over the package hashes of the file's package
//!   and every transitive reverse-dependent package (normal, dev, and
//!   build dependencies all count as edges). A mutant in `P` is observed
//!   by `P`'s tests and by the tests of every crate that depends on `P`
//!   (the runner passes `--test-workspace=true`, so every mutant is
//!   exercised by the whole workspace's test binaries — but a test
//!   binary can only observe `P`'s code through a dependency edge, or
//!   through the agent binary whose own tests live in `hatchling-agent`,
//!   itself a reverse dependent of everything it links). Changing a
//!   downstream crate therefore re-tests upstream files; changing a leaf
//!   crate (nothing depends on `hatchling-dev`) re-tests only that
//!   crate.
//! - `lock_hash`: SHA-256 of the workspace `Cargo.lock` (dependency
//!   versions are verdict-relevant inputs).
//! - `toolchain_id`: cargo-mutants version plus the rustc host and
//!   release lines (an `RUSTUP_TOOLCHAIN` override must not silently
//!   reuse another toolchain's verdicts).
//! - `env_id`: the value of `HATCHLING_REQUIRE_CELL` (it decides whether
//!   the cell tests run or skip, which changes what they kill).
//! - `config_hash`: SHA-256 of `.cargo/mutants.toml`. Exclusion changes
//!   surface through discovery, but option changes (timeouts, extra test
//!   args) do not affect the discovered set — without this field they
//!   would silently reuse verdicts measured under different options.
//!
//! Soundness rules:
//!
//! - The cache is populated only from runs where the tested set equals
//!   the discovered set and no mutant was missed. A dirty run never
//!   poisons the cache.
//! - Seed import refuses a seed whose baseline missed any mutant.
//! - Cached `Timeout` verdicts are never reused: timeouts are the
//!   verdict most sensitive to machine speed, so they always re-test.
//! - A cached `MissedMutant` verdict is never reused (defense in depth;
//!   a clean baseline cannot produce one).
//!
//! The merged `mutants.out/outcomes.json` has the same shape as a full
//! run's report (baseline record first, one record per discovered mutant
//! in discovery order, recomputed summary), so existing validators and
//! the results bundle work unchanged. Cache-served records carry
//! `"cached": true`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::Finding;
use super::sha256::Sha256;
use crate::Error;
use crate::environment as env;

/// Cache format version. Bump when the on-disk shape changes; older
/// caches are then rejected rather than misread.
const CACHE_VERSION: u32 = 1;

/// Seed format version.
const SEED_VERSION: u32 = 1;

/// Machine-local cache. Run state, not source: gitignored and excluded
/// from tree pushes.
fn cache_path(root: &Path) -> PathBuf {
    root.join(".mutation-cache").join("cache.json")
}

/// The checked-in seed: the sanitized official baseline.
fn seed_path(root: &Path) -> PathBuf {
    root.join("crates")
        .join("hatchling-dev")
        .join("seeds")
        .join("wo013-official.json")
}

/// Fixed fixture for the seed-check gate: a tiny synthetic workspace
/// plus the seed `scripts/gen_seed_fixture.py` computed from it. Both
/// are committed and never change outside a deliberate hash-scheme
/// change, so no task can break this check.
fn fixture_seed_path(root: &Path) -> PathBuf {
    root.join("crates")
        .join("hatchling-dev")
        .join("seeds")
        .join("fixture-seed.json")
}

/// Root of the fixed fixture workspace the fixture seed was generated
/// from.
fn fixture_tree_path(root: &Path) -> PathBuf {
    root.join("crates")
        .join("hatchling-dev")
        .join("seeds")
        .join("fixture-tree")
}

/// Verify a seed's per-file keys against a tree: the key scheme must
/// compute identical keys for every seeded file. Any divergence is a
/// finding instead of silently invalidating (or wrongly validating)
/// the cache.
///
/// The `toolchain_id` is excluded from the comparison: it captures the
/// active Rust toolchain, which legitimately differs across machines
/// and toolchains (MSRV 1.85.0 vs stable). The other four fields must
/// match exactly.
///
/// # Errors
///
/// Returns [`Error`] if the seed cannot be read or parsed, or a key
/// cannot be computed.
fn verify_seed_keys(
    seed_path: &Path,
    tree_root: &Path,
    require_cell: Option<&str>,
) -> Result<Vec<Finding>, Error> {
    let seed_text = std::fs::read_to_string(seed_path).map_err(Error::Io)?;
    let seed: Value = serde_json::from_str(&seed_text).map_err(|e| Error::Parse {
        path: seed_path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let files = seed
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Parse {
            path: seed_path.to_path_buf(),
            reason: String::from("seed has no files object"),
        })?;
    if files.is_empty() {
        return Err(Error::Parse {
            path: seed_path.to_path_buf(),
            reason: String::from("seed has no files"),
        });
    }
    let state = tree_state_with_env(tree_root, require_cell)?;
    let mut findings = Vec::new();
    for (rel, entry) in files {
        let package = entry
            .get("package")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Parse {
                path: seed_path.to_path_buf(),
                reason: format!("seed entry {rel} needs a package"),
            })?;
        let expected = entry
            .get("key")
            .and_then(key_from_json)
            .ok_or_else(|| Error::Parse {
                path: seed_path.to_path_buf(),
                reason: format!("seed entry {rel} needs a key"),
            })?;
        let computed = input_key(&state, tree_root, rel, package)?;
        for (field, computed_val, expected_val) in [
            ("file_hash", &computed.file_hash, &expected.file_hash),
            ("suite_hash", &computed.suite_hash, &expected.suite_hash),
            ("lock_hash", &computed.lock_hash, &expected.lock_hash),
            ("env_id", &computed.env_id, &expected.env_id),
        ] {
            if computed_val != expected_val {
                findings.push(Finding::new(
                    "seed-consistency",
                    format!("{field} mismatch for {rel}"),
                ));
            }
        }
    }
    Ok(findings)
}

/// Verify the fixed fixture seed against the fixed fixture tree.
///
/// This pins Python↔Rust hash-scheme equivalence: the fixture seed was
/// computed by the independent Python implementation in
/// `scripts/gen_seed_fixture.py`, so any divergence between the two
/// implementations fails here. The fixture never changes outside a
/// deliberate scheme change, so no task can break this check —
/// unlike the previous live-tree version, which flagged every seeded
/// file a WO touched and forced a full regeneration run per push.
///
/// The cell-gating value is pinned to `Some("1")` (the fixture seed
/// was generated with `HATCHLING_REQUIRE_CELL=1`) instead of reading
/// the ambient environment (`set_var` is `unsafe` in edition 2024,
/// which this crate denies).
///
/// # Errors
///
/// Returns [`Error`] if the fixture seed cannot be read or parsed, or
/// a key cannot be computed.
pub fn check_seed_consistency(root: &Path) -> Result<Vec<Finding>, Error> {
    verify_seed_keys(
        &fixture_seed_path(root),
        &fixture_tree_path(root),
        Some("1"),
    )
}

/// Cargo binary for mutation runs. Overridable via `HATCHLING_CARGO` so
/// the pipeline can be pointed at a wrapper, and via a test-only static
/// so tests can drive the whole `run` → `discover` → `run_misses`
/// pipeline against a fake-cargo script (canned `--list --json`, canned
/// `outcomes.json`) instead of a real nested cargo-mutants.
fn cargo_bin() -> String {
    if let Ok(guard) = CARGO_OVERRIDE.lock() {
        if let Some(path) = guard.clone() {
            return path.to_string_lossy().into_owned();
        }
    }
    std::env::var(env::HATCHLING_CARGO.name).unwrap_or_else(|_| String::from("cargo"))
}

/// Test-only override for the cargo binary. Production code always uses
/// the real cargo (or `HATCHLING_CARGO`).
static CARGO_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn set_cargo_override(path: Option<PathBuf>) {
    if let Ok(mut guard) = CARGO_OVERRIDE.lock() {
        *guard = path;
    }
}

/// Sentinel toolchain id used by unit tests. Production code always
/// shells out to `cargo mutants --version`; tests default to this
/// sentinel so the suite stays hermetic on machines without
/// cargo-mutants installed (CI installs it, but a fresh clone should
/// still run `cargo test`).
#[cfg(test)]
const TEST_TOOLCHAIN_ID: &str = "test-toolchain";

/// Test-only override for [`toolchain_id`]. Defaults to
/// `Some(TEST_TOOLCHAIN_ID)`; only `toolchain_id_contains_mutants_version`
/// clears it (to `None`) to exercise the real shell-out, holding the
/// lock for the whole test so parallel tests cannot observe the
/// cleared override.
#[cfg(test)]
static TOOLCHAIN_OVERRIDE: std::sync::LazyLock<std::sync::Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Some(String::from(TEST_TOOLCHAIN_ID))));

/// SHA-256 of `data`, lowercase hex. Implemented in
/// [`super::sha256`] rather than via an external crate: a new direct
/// dependency would rewrite `Cargo.lock`'s `hatchling-dev` entry and
/// invalidate every cache key through the global `lock_hash`.
fn sha256_hex(data: &[u8]) -> String {
    super::sha256::sha256_hex(data)
}

/// Directory names that never contribute to a package hash: build
/// output, VCS metadata, and the cache machinery itself. `seeds/` is
/// tool input data, not test-relevant source — including it would
/// invalidate the cache every time a seed is regenerated, for no change
/// in any verdict.
fn skipped_dir(name: &str) -> bool {
    matches!(
        name,
        "target" | ".git" | ".mutation-cache" | "mutants.scratch" | "seeds"
    ) || name.starts_with("mutants.out")
}

/// Collect `(posix-relative-path, absolute-path)` for every file under
/// `dir`, depth-first with sorted entries so the order is deterministic.
/// Collect `(relpath, full_path)` for every file under `dir`, in
/// `os.walk` order: the current directory's files first (sorted by
/// name), then each subdirectory (sorted by name), recursively. The
/// order matters because the package hash streams the files; it must
/// match the Python seed generator byte-for-byte.
fn collect_files(
    dir: &Path,
    rel_prefix: &str,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), Error> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(Error::Io)?
        .collect::<Result<_, _>>()
        .map_err(Error::Io)?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut subdirs: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if rel_prefix.is_empty() {
            name.clone()
        } else {
            format!("{rel_prefix}/{name}")
        };
        if path.is_dir() {
            if skipped_dir(&name) {
                continue;
            }
            subdirs.push((rel, path));
        } else if path.is_file() {
            out.push((rel, path));
        }
    }
    for (rel, path) in subdirs {
        collect_files(&path, &rel, out)?;
    }
    Ok(())
}

/// Hash every file under a package directory: `relpath + 0x00 + bytes +
/// 0x00` per file, paths sorted. Covers code, tests, and data files —
/// anything a test binary in the package could read.
fn hash_package_dir(dir: &Path) -> Result<String, Error> {
    let mut files = Vec::new();
    collect_files(dir, "", &mut files)?;
    // No sort: `collect_files` already yields `os.walk` order, which the
    // seed generator hashes as-is. Sorting here would break
    // byte-for-byte compatibility with the checked-in seed.
    let mut hasher = Sha256::new();
    for (rel, full) in &files {
        hasher.update(rel.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(&std::fs::read(full).map_err(Error::Io)?);
        hasher.update(&[0u8]);
    }
    Ok(hasher.hexdigest())
}

/// Resolve a dependency entry to a workspace member name, if it is one.
/// Matches by dependency key, by `package =` rename, or by `path =`
/// pointing into a member directory.
fn resolve_dep_target(
    dep_key: &str,
    spec: &Value,
    members: &HashMap<String, PathBuf>,
    member_dir: &Path,
) -> Option<String> {
    if members.contains_key(dep_key) {
        return Some(dep_key.to_owned());
    }
    let table = spec.as_object()?;
    if let Some(pkg) = table.get("package").and_then(Value::as_str) {
        if members.contains_key(pkg) {
            return Some(pkg.to_owned());
        }
    }
    if let Some(path) = table.get("path").and_then(Value::as_str) {
        let resolved = member_dir.join(path);
        // `canonicalize` also normalizes the `..` in `../hatchling-core`.
        let canonical = std::fs::canonicalize(&resolved).unwrap_or(resolved);
        for (name, dir) in members {
            let dir_canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
            if canonical == dir_canonical {
                return Some(name.clone());
            }
        }
    }
    None
}

/// Map each workspace member to the set of members that transitively
/// depend on it (through normal, dev, or build dependencies).
fn reverse_deps(root: &Path) -> Result<HashMap<String, HashSet<String>>, Error> {
    let member_list = super::members(root)?;
    let mut members: HashMap<String, PathBuf> = HashMap::new();
    for member in &member_list {
        members.insert(member.name.clone(), member.dir.clone());
    }
    // Direct edges: dependency -> its direct dependents.
    let mut dependents: HashMap<String, HashSet<String>> = HashMap::new();
    for name in members.keys() {
        dependents.entry(name.clone()).or_default();
    }
    for member in &member_list {
        let table: toml::Table = super::read_toml(&member.dir.join("Cargo.toml"))?;
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let Some(deps) = table.get(section).and_then(toml::Value::as_table) else {
                continue;
            };
            for (dep_key, spec) in deps {
                let spec_json = toml_value_to_json(spec);
                if let Some(target) = resolve_dep_target(dep_key, &spec_json, &members, &member.dir)
                {
                    if target != member.name {
                        dependents
                            .entry(target)
                            .or_default()
                            .insert(member.name.clone());
                    }
                }
            }
        }
    }
    // Transitive closure.
    let mut closure: HashMap<String, HashSet<String>> = HashMap::new();
    for name in members.keys() {
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = dependents
            .get(name)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(next) = dependents.get(&current) {
                stack.extend(next.iter().cloned());
            }
        }
        closure.insert(name.clone(), seen);
    }
    Ok(closure)
}

/// Convert a `toml::Value` to `serde_json::Value` (shapes line up for
/// the dependency tables this resolver reads).
fn toml_value_to_json(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => Value::from(*f),
        toml::Value::Boolean(b) => Value::from(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_value_to_json).collect()),
        toml::Value::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_value_to_json(v)))
                .collect(),
        ),
    }
}

/// Everything about the current tree that feeds key computation,
/// computed once per run.
struct TreeState {
    rdeps: HashMap<String, HashSet<String>>,
    package_hashes: HashMap<String, String>,
    lock_hash: String,
    toolchain_id: String,
    env_id: String,
}

fn tree_state(root: &Path) -> Result<TreeState, Error> {
    tree_state_with_env(
        root,
        std::env::var(env::HATCHLING_REQUIRE_CELL.name)
            .ok()
            .as_deref(),
    )
}

/// [`tree_state`] with an explicit cell-gating value, for tests: the
/// checked-in seed was generated with the variable set, so the
/// differential test pins the value instead of reading the ambient
/// environment (and `std::env::set_var` is `unsafe` in edition 2024,
/// which this crate denies).
fn tree_state_with_env(root: &Path, require_cell: Option<&str>) -> Result<TreeState, Error> {
    let rdeps = reverse_deps(root)?;
    let mut package_hashes = HashMap::new();
    for member in super::members(root)? {
        package_hashes.insert(
            member.name.clone(),
            hash_package_dir(&member.dir).map_err(|e| Error::Parse {
                path: member.dir.clone(),
                reason: format!("could not hash package dir: {e}"),
            })?,
        );
    }
    let lock_path = root.join("Cargo.lock");
    if !lock_path.is_file() {
        return Err(Error::MissingFile(lock_path));
    }
    let lock_hash = sha256_hex(&std::fs::read(&lock_path).map_err(Error::Io)?);
    Ok(TreeState {
        rdeps,
        package_hashes,
        lock_hash,
        toolchain_id: toolchain_id()?,
        env_id: env_id_from(require_cell),
    })
}

/// Identify the toolchain that will run the tests: the cargo-mutants
/// version (it controls mutant discovery) and the actual rustc host and
/// release (an `RUSTUP_TOOLCHAIN` override must not silently reuse
/// another toolchain's verdicts).
///
/// In test builds the id defaults to [`TEST_TOOLCHAIN_ID`] so unit
/// tests stay hermetic on machines without cargo-mutants installed
/// (CI installs it, but a fresh clone should still run `cargo test`);
/// only `toolchain_id_contains_mutants_version` clears the override to
/// exercise the real shell-out.
fn toolchain_id() -> Result<String, Error> {
    #[cfg(test)]
    {
        if let Ok(guard) = TOOLCHAIN_OVERRIDE.lock() {
            if let Some(id) = guard.clone() {
                return Ok(id);
            }
            // Override cleared: fall through to the real shell-out.
            // The guard drops here, so the clearing test (which holds
            // the lock itself) cannot deadlock against this path.
        }
    }
    toolchain_id_from_tools()
}

/// The real `toolchain_id` shell-out: `cargo mutants --version` plus
/// the rustc host/release lines. Split out so the one test that
/// exercises it can call it directly while holding the
/// [`TOOLCHAIN_OVERRIDE`] lock (calling [`toolchain_id`] there would
/// deadlock on the held guard).
fn toolchain_id_from_tools() -> Result<String, Error> {
    let mutants = command_output("cargo", &["mutants", "--version"])?;
    let rustc = command_output("rustc", &["-vV"])?;
    let mut host = None;
    let mut release = None;
    for line in rustc.lines() {
        if let Some(rest) = line.strip_prefix("host:") {
            host = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("release:") {
            release = Some(rest.trim().to_owned());
        }
    }
    let (Some(host), Some(release)) = (host, release) else {
        return Err(Error::ToolFailed {
            tool: String::from("rustc -vV"),
            status: String::from("could not parse host/release lines"),
        });
    };
    Ok(format!(
        "{}\nhost: {host}\nrelease: {release}",
        mutants.trim()
    ))
}

/// Bounded `--version` probe for toolchain identification.
/// The probe has nothing to write, so a 60s bound is generous; a
/// probe that cannot answer in a minute is broken, not slow.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

fn command_output(program: &str, args: &[&str]) -> Result<String, Error> {
    super::driver::drive_output(&super::driver::Spec {
        program,
        args,
        current_dir: None,
        env: &[],
        timeout: PROBE_TIMEOUT,
    })
}

/// The one environmental verdict factor the tool does not control:
/// whether the cell tests run (`1`) or skip (unset/other).
fn env_id_from(value: Option<&str>) -> String {
    format!("HATCHLING_REQUIRE_CELL={}", value.unwrap_or("unset"))
}

/// The verdict-relevant inputs for one mutated source file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InputKey {
    file_hash: String,
    suite_hash: String,
    lock_hash: String,
    toolchain_id: String,
    env_id: String,
    /// SHA-256 of `.cargo/mutants.toml`. Exclusion changes surface
    /// through discovery (different mutant sets), but option changes
    /// (timeouts, extra test args) do not — without this field a
    /// config change would silently reuse verdicts measured under
    /// different options.
    config_hash: String,
}

/// SHA-256 of `.cargo/mutants.toml`, or of the empty string when the
/// file does not exist. Part of every [`InputKey`]: mutant options are
/// verdict-relevant inputs.
fn config_hash(root: &Path) -> String {
    let path = root.join(".cargo").join("mutants.toml");
    let bytes = std::fs::read(&path).unwrap_or_default();
    sha256_hex(&bytes)
}

fn input_key(
    state: &TreeState,
    root: &Path,
    file_rel: &str,
    package: &str,
) -> Result<InputKey, Error> {
    let file_hash = sha256_hex(&std::fs::read(root.join(file_rel)).map_err(Error::Io)?);
    let mut packages: Vec<&str> = state
        .rdeps
        .get(package)
        .map(|set| set.iter().map(String::as_str).collect())
        .unwrap_or_default();
    packages.push(package);
    packages.sort_unstable();
    // NOTE: no dedup here. This mirrors the seed generator's
    // `sorted([pkg] + sorted(rdeps[pkg]))` exactly: on a dependency
    // cycle the package appears twice, and the checked-in seed was
    // generated that way. Byte-for-byte compatibility with the seed
    // beats aesthetic dedup.
    let mut hasher = Sha256::new();
    for name in packages {
        hasher.update(name.as_bytes());
        hasher.update(&[0u8]);
        let Some(pkg_hash) = state.package_hashes.get(name) else {
            return Err(Error::Parse {
                path: root.to_path_buf(),
                reason: format!("no package hash for reverse dependent `{name}`"),
            });
        };
        hasher.update(pkg_hash.as_bytes());
        hasher.update(&[0u8]);
    }
    Ok(InputKey {
        file_hash,
        suite_hash: hasher.hexdigest(),
        lock_hash: state.lock_hash.clone(),
        toolchain_id: state.toolchain_id.clone(),
        env_id: state.env_id.clone(),
        config_hash: config_hash(root),
    })
}

/// One mutant from `cargo mutants --list --json`.
#[derive(Debug, Clone)]
struct Discovered {
    name: String,
    package: String,
    /// Workspace-relative posix path, as listed.
    file: String,
}

/// Discover every mutant without running anything. Fast (static
/// analysis only); the returned order is cargo-mutants' discovery
/// order, which the merged report preserves.
///
/// # Errors
///
/// Returns [`Error`](crate::Error) if the tool cannot run or its JSON
/// cannot be parsed.
/// Bound for `cargo mutants --list --json`: discovery only
/// lists mutants without compiling, so 5 minutes is generous.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(300);

fn discover(root: &Path) -> Result<Vec<Discovered>, Error> {
    let cargo = cargo_bin();
    let driven = super::driver::drive(&super::driver::Spec {
        program: &cargo,
        args: &["mutants", "--list", "--json"],
        current_dir: Some(root),
        env: &[],
        timeout: DISCOVER_TIMEOUT,
    })?;
    if !driven.status.success() {
        return Err(Error::ToolFailed {
            tool: String::from("cargo mutants --list"),
            status: driven.status.to_string(),
        });
    }
    let text = String::from_utf8(driven.stdout).map_err(|e| Error::Parse {
        path: PathBuf::from("<cargo mutants --list --json>"),
        reason: e.to_string(),
    })?;
    parse_discovered(&text)
}

/// Parse the `cargo mutants --list --json` output. Split out so the
/// duplicate-rejection logic is unit-testable without running cargo.
fn parse_discovered(text: &str) -> Result<Vec<Discovered>, Error> {
    let json: Value = serde_json::from_str(text).map_err(|e| Error::Parse {
        path: PathBuf::from("<cargo mutants --list --json>"),
        reason: e.to_string(),
    })?;
    let Some(list) = json.as_array() else {
        return Err(Error::Parse {
            path: PathBuf::from("<cargo mutants --list --json>"),
            reason: String::from("expected a JSON array"),
        });
    };
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let field = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Parse {
                    path: PathBuf::from("<cargo mutants --list --json>"),
                    reason: format!("mutant entry missing `{key}`"),
                })
        };
        out.push(Discovered {
            name: field("name")?.to_owned(),
            package: field("package")?.to_owned(),
            file: field("file")?.to_owned(),
        });
    }
    // Discovery must be a set: duplicate mutant names would make the
    // cache ambiguous (which verdict belongs to which?).
    let mut seen = HashSet::new();
    for mutant in &out {
        if !seen.insert(mutant.name.as_str()) {
            return Err(Error::Parse {
                path: PathBuf::from("<cargo mutants --list --json>"),
                reason: format!("duplicate discovered mutant `{}`", mutant.name),
            });
        }
    }
    Ok(out)
}

/// Stored verdicts for one file: its key plus mutant name -> verdict.
#[derive(Debug, Clone)]
struct FileEntry {
    key: InputKey,
    mutants: BTreeMap<String, String>,
}

/// The machine-local cache: workspace-relative file path -> entry.
#[derive(Debug, Clone, Default)]
struct Cache {
    files: BTreeMap<String, FileEntry>,
}

fn key_to_json(key: &InputKey) -> Value {
    Value::Object(
        [
            ("file_hash", key.file_hash.as_str()),
            ("suite_hash", key.suite_hash.as_str()),
            ("lock_hash", key.lock_hash.as_str()),
            ("toolchain_id", key.toolchain_id.as_str()),
            ("env_id", key.env_id.as_str()),
            ("config_hash", key.config_hash.as_str()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), Value::String(v.to_owned())))
        .collect(),
    )
}

fn key_from_json(value: &Value) -> Option<InputKey> {
    let get = |k: &str| value.get(k)?.as_str().map(str::to_owned);
    Some(InputKey {
        file_hash: get("file_hash")?,
        suite_hash: get("suite_hash")?,
        lock_hash: get("lock_hash")?,
        toolchain_id: get("toolchain_id")?,
        env_id: get("env_id")?,
        config_hash: get("config_hash")?,
    })
}

fn cache_to_json(cache: &Cache) -> Value {
    let files = cache
        .files
        .iter()
        .map(|(path, entry)| {
            let mutants = entry
                .mutants
                .iter()
                .map(|(n, v)| (n.clone(), Value::String(v.clone())))
                .collect();
            (
                path.clone(),
                Value::Object(
                    [
                        ("key".to_owned(), key_to_json(&entry.key)),
                        ("mutants".to_owned(), Value::Object(mutants)),
                    ]
                    .into_iter()
                    .collect(),
                ),
            )
        })
        .collect();
    Value::Object(
        [
            ("version".to_owned(), Value::from(CACHE_VERSION)),
            ("files".to_owned(), Value::Object(files)),
        ]
        .into_iter()
        .collect(),
    )
}

fn cache_from_json(value: &Value, path: &Path) -> Result<Cache, Error> {
    let bad = |reason: String| Error::Parse {
        path: path.to_owned(),
        reason,
    };
    if value.get("version").and_then(Value::as_u64) != Some(u64::from(CACHE_VERSION)) {
        return Err(bad(format!(
            "unsupported cache version (expected {CACHE_VERSION})"
        )));
    }
    let Some(files) = value.get("files").and_then(Value::as_object) else {
        return Err(bad(String::from("cache has no `files` object")));
    };
    let mut cache = Cache::default();
    for (rel, entry) in files {
        let Some(entry) = entry.as_object() else {
            return Err(bad(format!("cache entry for `{rel}` is not an object")));
        };
        let Some(key) = entry.get("key").and_then(key_from_json) else {
            return Err(bad(format!("cache entry for `{rel}` has no key")));
        };
        let Some(mutants) = entry.get("mutants").and_then(Value::as_object) else {
            return Err(bad(format!("cache entry for `{rel}` has no mutants")));
        };
        let mut map = BTreeMap::new();
        for (name, verdict) in mutants {
            let Some(verdict) = verdict.as_str() else {
                return Err(bad(format!("mutant `{name}` in `{rel}` has no verdict")));
            };
            map.insert(name.clone(), verdict.to_owned());
        }
        cache
            .files
            .insert(rel.clone(), FileEntry { key, mutants: map });
    }
    Ok(cache)
}

fn save_cache(root: &Path, cache: &Cache) -> Result<(), Error> {
    let path = cache_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let text = serde_json::to_string_pretty(&cache_to_json(cache)).map_err(|e| Error::Parse {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    std::fs::write(&path, text + "\n").map_err(Error::Io)
}

/// Load the machine-local cache, importing the checked-in seed when no
/// cache exists yet.
fn load_cache(root: &Path) -> Result<Cache, Error> {
    let path = cache_path(root);
    if !path.is_file() {
        return import_seed(root, &seed_path(root));
    }
    let text = std::fs::read_to_string(&path).map_err(Error::Io)?;
    let json: Value = serde_json::from_str(&text).map_err(|e| Error::Parse {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    cache_from_json(&json, &path)
}

/// Import a seed file: keep an entry only if its key, recomputed from
/// the current tree, still matches — i.e. the file's verdict-relevant
/// inputs are byte-identical to the seed baseline's. A seed whose
/// baseline missed any mutant is refused outright: reuse is only ever
/// from clean, complete baselines.
fn import_seed(root: &Path, path: &Path) -> Result<Cache, Error> {
    if !path.is_file() {
        return Err(Error::MissingFile(path.to_owned()));
    }
    let text = std::fs::read_to_string(path).map_err(Error::Io)?;
    let json: Value = serde_json::from_str(&text).map_err(|e| Error::Parse {
        path: path.to_owned(),
        reason: e.to_string(),
    })?;
    let bad = |reason: String| Error::Parse {
        path: path.to_owned(),
        reason,
    };
    if json.get("version").and_then(Value::as_u64) != Some(u64::from(SEED_VERSION)) {
        return Err(bad(format!(
            "unsupported seed version (expected {SEED_VERSION})"
        )));
    }
    let missed = json
        .pointer("/summary/missed")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if missed != 0 {
        return Err(bad(format!(
            "seed baseline missed {missed} mutant(s); refusing to seed the cache from a dirty baseline"
        )));
    }
    let Some(files) = json.get("files").and_then(Value::as_object) else {
        return Err(bad(String::from("seed has no `files` object")));
    };
    let state = tree_state(root)?;
    // The seed stores the package per file (added by the generator);
    // fall back to deriving it from the member dirs for older seeds.
    let member_list = super::members(root)?;
    let mut cache = Cache::default();
    for (rel, entry) in files {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let (Some(key), Some(mutants)) = (
            entry.get("key").and_then(key_from_json),
            entry.get("mutants").and_then(Value::as_object),
        ) else {
            continue;
        };
        let package = entry
            .get("package")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| package_for_file(root, rel, &member_list));
        let Some(package) = package else {
            continue;
        };
        // Recompute the key from the current tree: only a byte-identical
        // input set reuses the seed's verdicts.
        let Ok(current) = input_key(&state, root, rel, &package) else {
            continue;
        };
        if current != key {
            continue;
        }
        let mut map = BTreeMap::new();
        for (name, verdict) in mutants {
            if let Some(verdict) = verdict.as_str() {
                map.insert(name.clone(), verdict.to_owned());
            }
        }
        cache
            .files
            .insert(rel.clone(), FileEntry { key, mutants: map });
    }
    Ok(cache)
}

/// Find the workspace member whose directory contains `file_rel`.
fn package_for_file(root: &Path, file_rel: &str, members: &[super::Member]) -> Option<String> {
    let full = root.join(file_rel);
    members
        .iter()
        .find(|m| full.starts_with(&m.dir))
        .map(|m| m.name.clone())
}

/// Split discovery into cache hits and files to test. A file hits only
/// when its key matches, the discovered mutant set is identical, and no
/// stored verdict is `MissedMutant` or `Timeout` (timeouts always
/// re-test: they are the verdict most sensitive to machine speed).
struct Partition {
    /// file -> Vec<(mutant name, verdict)> served from cache
    hits: BTreeMap<String, Vec<(String, String)>>,
    /// files whose mutants must be tested
    misses: Vec<String>,
}

fn partition(
    state: &TreeState,
    root: &Path,
    discovered: &[Discovered],
    cache: &Cache,
) -> Result<Partition, Error> {
    let mut by_file: BTreeMap<&str, Vec<&Discovered>> = BTreeMap::new();
    for mutant in discovered {
        by_file
            .entry(mutant.file.as_str())
            .or_default()
            .push(mutant);
    }
    let mut hits = BTreeMap::new();
    let mut misses = Vec::new();
    for (file, mutants) in &by_file {
        let Some(first) = mutants.first() else {
            return Err(Error::Parse {
                path: root.to_path_buf(),
                reason: format!("no mutants discovered for `{file}`"),
            });
        };
        let key = input_key(state, root, file, &first.package)?;
        let discovered_names: HashSet<&str> = mutants.iter().map(|m| m.name.as_str()).collect();
        let hit = cache.files.get(*file).filter(|entry| {
            entry.key == key
                && entry
                    .mutants
                    .keys()
                    .map(String::as_str)
                    .collect::<HashSet<_>>()
                    == discovered_names
 // allowlist, not denylist. Only genuine
                // verdicts (`CaughtMutant`, `Unviable`) may be
                // served from cache; anything else (`MissedMutant`,
                // `Timeout`, `Failure`, unknown) forces a re-test.
                // The seed builder never writes those, but a
                // hand-edited seed must not slip through either.
                && entry
                    .mutants
                    .values()
                    .all(|v| super::verdict::is_reusable_verdict(v))
        });
        match hit {
            Some(entry) => {
                hits.insert(
                    (*file).to_owned(),
                    entry
                        .mutants
                        .iter()
                        .map(|(n, v)| (n.clone(), v.clone()))
                        .collect(),
                );
            }
            None => misses.push((*file).to_owned()),
        }
    }
    Ok(Partition { hits, misses })
}

/// Escape a file path for the `--re` mutant-name regex.
fn escape_regex(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if !(c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-')) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Bound for the agent prebuild (`cargo build -p hatchling-agent`,
/// ): a debug build of one binary, 10 minutes is generous even
/// on a cold cache.
const PREBUILD_TIMEOUT: Duration = Duration::from_secs(600);

/// Bound for the subset mutation run (`cargo mutants --re ...`,
/// ). The subset is small, but each mutant still compiles the
/// workspace; an hour is the fail-closed outer bound.
const SUBSET_RUN_TIMEOUT: Duration = Duration::from_secs(3600);

/// Test every mutant in `files` with a single cargo-mutants invocation,
/// selected by a `--re` alternation over the file paths (`-f/--file` is
/// not repeatable). Returns the baseline record plus mutant name ->
/// full outcome record.
///
/// The invocation mirrors the full runner's environment: the scratch
/// directory stays under `target/` (never the system temp dir), and the
/// cell tests find the agent binary via `HATCHLING_AGENT_BIN`. Like the
/// full runner, a non-zero exit is not itself a failure (mutants that
/// time out fail the tool's exit status); the verdicts come from the
/// report, and a missing report is the real failure.
///
/// # Errors
///
/// Returns [`Error`](crate::Error) if the tool cannot run or writes no
/// report.
fn run_misses(root: &Path, files: &[String]) -> Result<(Value, BTreeMap<String, Value>), Error> {
    let pattern = format!(
        "^({}):",
        files
            .iter()
            .map(|f| escape_regex(f))
            .collect::<Vec<_>>()
            .join("|")
    );
    // Same scratch location as the full runner: `$HOME/mutants.scratch`,
    // disk-backed and outside the repository, never nested inside a
    // tree copy that cargo-mutants would recurse into.
    let scratch = super::mutants::scratch_dir(root)?;
    std::fs::create_dir_all(&scratch).map_err(Error::Io)?;
    let outcomes_path = root.join("mutants.out").join("outcomes.json");
    // A crashed or killed run must never be evaluated against a stale
    // report from an earlier run.
    let _ = std::fs::remove_file(&outcomes_path);
    let agent_bin = root.join("target").join("debug").join("hatchling-agent");
    // Bounded prebuild of the agent binary the cell tests need
    // . The exit status is deliberately ignored — a failed
    // prebuild surfaces through the outcome report — but a
    // driver-level error (spawn failure, timeout) aborts the run:
    // `drive` returns Ok on any exit status, so `?` only propagates
    // real failures.
    let cargo = cargo_bin();
    super::driver::drive(&super::driver::Spec {
        program: &cargo,
        args: &["build", "-p", "hatchling-agent", "--offline"],
        current_dir: Some(root),
        env: &[],
        timeout: PREBUILD_TIMEOUT,
    })?;
    // The subset run is likewise status-blind (mutants that time out
    // fail the tool's exit status; the verdicts come from the report),
    // but a DRIVER TIMEOUT is an error — the tool never finished. The
    // "no outcomes.json written" check below stays as the second line
    // of defense.
    super::driver::drive(&super::driver::Spec {
        program: &cargo,
        args: &[
            "mutants",
            "--workspace",
            "--test-workspace=true",
            // No baseline on the subset run: with `--re`, cargo-mutants
            // scopes the baseline to the selected packages, so crates
            // outside the selection (e.g. hatchling-broker) are never
            // built and tests that need their binaries (e.g. the agent
            // e2e) fail the baseline spuriously. The workflow's separate
            // full-workspace baseline remains the real gate.
            "--baseline=skip",
            // Without a baseline there is no measured test time to scale
            // the per-mutant timeout from; use a fixed timeout instead
            // of the default scaling.
            "--timeout",
            "20",
            "--re",
            &pattern,
        ],
        current_dir: Some(root),
        env: &[
            ("TMPDIR", scratch.as_path()),
            ("HATCHLING_AGENT_BIN", agent_bin.as_path()),
        ],
        timeout: SUBSET_RUN_TIMEOUT,
    })?;
    if !outcomes_path.exists() {
        return Err(Error::ToolFailed {
            tool: String::from("cargo mutants"),
            status: String::from("no outcomes.json written"),
        });
    }
    // Keep the raw subset report next to the merged one for debugging.
    let subset_path = root.join("mutants.out").join("outcomes-subset.json");
    std::fs::copy(&outcomes_path, &subset_path).map_err(Error::Io)?;
    let text = std::fs::read_to_string(&outcomes_path).map_err(Error::Io)?;
    let json: Value = serde_json::from_str(&text).map_err(|e| Error::Parse {
        path: outcomes_path.clone(),
        reason: e.to_string(),
    })?;
    let mut baseline = None;
    let mut records = BTreeMap::new();
    let bad = |reason: String| Error::Parse {
        path: outcomes_path.clone(),
        reason,
    };
    for outcome in json
        .get("outcomes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if outcome.get("scenario").and_then(Value::as_str) == Some("Baseline") {
            if baseline.is_some() {
                return Err(bad(String::from(
                    "subset report has duplicate baseline records",
                )));
            }
            baseline = Some(outcome.clone());
            continue;
        }
        if let Some(name) = outcome
            .pointer("/scenario/Mutant/name")
            .and_then(Value::as_str)
        {
            if records.contains_key(name) {
                return Err(bad(format!(
                    "subset report has duplicate mutant record `{name}`"
                )));
            }
            records.insert(name.to_owned(), outcome.clone());
        }
    }
    let baseline = baseline.unwrap_or_else(|| {
        // `--baseline=skip`: the subset run measured no baseline, so its
        // report has no baseline record. Synthesize a labelled one; the
        // workflow's separate full-workspace baseline remains the real
        // gate, and the floor is evaluated on the merged report.
        serde_json::json!({
            "scenario": "Baseline",
            "summary": "Success",
            "synthetic": true,
        })
    });
    Ok((baseline, records))
}

/// Verdict strings cargo-mutants writes into `outcomes.json`.
const CAUGHT: &str = "CaughtMutant";
const MISSED: &str = "MissedMutant";
const TIMEOUT: &str = "Timeout";
const UNVIABLE: &str = "Unviable";

/// Write the merged `mutants.out/outcomes.json`: the baseline record
/// first, then one record per discovered mutant in discovery order —
/// fresh records verbatim, cache-served records synthesized and marked
/// `"cached": true` — with recomputed summary counters. A discovered
/// mutant with no outcome anywhere is an error: the merged report must
/// cover the discovered set exactly, never a subset.
///
/// # Errors
///
/// Returns [`Error`](crate::Error) if any discovered mutant has no
/// outcome or the report cannot be written.
#[allow(clippy::too_many_arguments)]
/// Build the `outcomes.json` record for a cache hit, in the same
/// shape cargo-mutants writes for a fresh mutant.
fn cached_record(mutant: &Discovered, verdict: &str) -> Value {
    let mut mutant_obj = serde_json::Map::new();
    mutant_obj.insert("name".to_owned(), Value::String(mutant.name.clone()));
    mutant_obj.insert("package".to_owned(), Value::String(mutant.package.clone()));
    mutant_obj.insert("file".to_owned(), Value::String(mutant.file.clone()));
    let mut scenario = serde_json::Map::new();
    scenario.insert("Mutant".to_owned(), Value::Object(mutant_obj));
    let mut record = serde_json::Map::new();
    record.insert("scenario".to_owned(), Value::Object(scenario));
    record.insert("summary".to_owned(), Value::String(verdict.to_owned()));
    record.insert("cached".to_owned(), Value::Bool(true));
    Value::Object(record)
}

fn merge_report(
    root: &Path,
    discovered: &[Discovered],
    hits: &BTreeMap<String, Vec<(String, String)>>,
    fresh: &BTreeMap<String, Value>,
    baseline: Option<&Value>,
) -> Result<(), Error> {
    let mut outcomes: Vec<Value> = Vec::with_capacity(discovered.len() + 1);
    if let Some(record) = baseline {
        outcomes.push(record.clone());
    } else {
        // Zero-miss run: nothing was tested, so there is no fresh
        // baseline. The record says exactly that; the workflow's
        // separate baseline step remains the real gate.
        outcomes.push(Value::Object(
            [
                (
                    "scenario".to_owned(),
                    Value::String(String::from("Baseline")),
                ),
                ("summary".to_owned(), Value::String(String::from("Success"))),
                (
                    "note".to_owned(),
                    Value::String(String::from(
                        "no mutants required testing; all outcomes served from cache",
                    )),
                ),
            ]
            .into_iter()
            .collect(),
        ));
    }
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for mutant in discovered {
        let (summary, record) = if let Some(record) = fresh.get(&mutant.name) {
            let summary = record
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("Unknown");
            (summary, record.clone())
        } else {
            let verdict = hits
                .get(&mutant.file)
                .and_then(|v| v.iter().find(|(n, _)| n == &mutant.name))
                .map(|(_, v)| v.as_str());
            let Some(verdict) = verdict else {
                return Err(Error::ToolFailed {
                    tool: String::from("mutant-cache"),
                    status: format!("no outcome for discovered mutant {}", mutant.name),
                });
            };
            (verdict, cached_record(mutant, verdict))
        };
        *counts.entry(summary).or_default() += 1;
        outcomes.push(record);
    }
    let missed = counts.get(MISSED).copied().unwrap_or(0);
    let report = Value::Object(
        [
            ("outcomes".to_owned(), Value::Array(outcomes)),
            (
                "total_mutants".to_owned(),
                Value::from(u64::from(
                    u32::try_from(discovered.len()).unwrap_or(u32::MAX),
                )),
            ),
            (
                "caught".to_owned(),
                Value::from(counts.get(CAUGHT).copied().unwrap_or(0)),
            ),
            ("missed".to_owned(), Value::from(missed)),
            (
                "timeout".to_owned(),
                Value::from(counts.get(TIMEOUT).copied().unwrap_or(0)),
            ),
            (
                "unviable".to_owned(),
                Value::from(counts.get(UNVIABLE).copied().unwrap_or(0)),
            ),
            ("success".to_owned(), Value::Bool(missed == 0)),
        ]
        .into_iter()
        .collect(),
    );
    let path = root.join("mutants.out").join("outcomes.json");
    // A zero-miss run tests nothing, so cargo-mutants never created
    // `mutants.out/`: create it here, as `save_cache` does for its dir.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let text = serde_json::to_string_pretty(&report).map_err(|e| Error::Parse {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    std::fs::write(&path, text + "\n").map_err(Error::Io)
}

/// Run the mutation gate through the content-hash cache.
///
/// Discovers all mutants, reuses cached verdicts for files whose inputs
/// are byte-identical to a previous clean run, tests the rest, merges,
/// and evaluates the kill floor on the merged report.
///
/// The cache is updated only from a clean, complete run (floor held,
/// which at 1.0 implies zero missed and full discovery coverage); a
/// dirty run leaves the cache untouched.
///
/// # Errors
///
/// Returns [`Error`](crate::Error) if discovery, the tool, the merge,
/// or the cache itself fails. Check *violations* (floor breaches) come
/// back as the `Vec<Finding>`, not as an error.
pub fn run(root: &Path) -> Result<Vec<Finding>, Error> {
    let outcomes_path = root.join("mutants.out").join("outcomes.json");
    let _ = std::fs::remove_file(&outcomes_path);
    let discovered = discover(root)?;
    let state = tree_state(root)?;
    let cache = load_cache(root)?;
    let partition = partition(&state, root, &discovered, &cache)?;
    let (baseline, fresh) = if partition.misses.is_empty() {
        (None, BTreeMap::new())
    } else {
        let (baseline, fresh) = run_misses(root, &partition.misses)?;
        // Exact identity: the fresh records must correspond exactly to
        // the discovered mutants for the miss files — no missing, no
        // extra, no duplicates (duplicates already rejected in
        // `run_misses`). A mismatch means the test run did not measure
        // what discovery promised.
        let expected: HashSet<&str> = discovered
            .iter()
            .filter(|d| partition.misses.contains(&d.file))
            .map(|d| d.name.as_str())
            .collect();
        let actual: HashSet<&str> = fresh.keys().map(String::as_str).collect();
        if expected != actual {
            let mut missing: Vec<&str> = expected.difference(&actual).copied().collect();
            let mut extra: Vec<&str> = actual.difference(&expected).copied().collect();
            missing.sort_unstable();
            extra.sort_unstable();
            return Err(Error::ToolFailed {
                tool: String::from("mutant-cache"),
                status: format!(
                    "fresh/tested mutant identity mismatch: missing {missing:?}, extra {extra:?}"
                ),
            });
        }
        (Some(baseline), fresh)
    };
    merge_report(
        root,
        &discovered,
        &partition.hits,
        &fresh,
        baseline.as_ref(),
    )?;
    // Same policy objects as the full runner: the floor is evaluated on
    // the merged report, never on a subset.
    let summary = super::mutants::read_summary(root)?;
    let missed = super::mutants::missed_names(root)?;
    let findings = super::mutants::evaluate(&summary, missed);
    if findings.is_empty() {
        let mut cache = cache;
        for file in &partition.misses {
            let package = discovered
                .iter()
                .find(|d| &d.file == file)
                .map(|d| d.package.clone());
            let Some(package) = package else {
                return Err(Error::ToolFailed {
                    tool: String::from("mutant-cache"),
                    status: format!("miss file `{file}` has no discovered mutants"),
                });
            };
            let mut mutants = BTreeMap::new();
            for mutant in discovered.iter().filter(|d| &d.file == file) {
                let verdict = fresh
                    .get(&mutant.name)
                    .and_then(|r| r.get("summary"))
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown");
                mutants.insert(mutant.name.clone(), verdict.to_owned());
            }
            let key = input_key(&state, root, file, &package)?;
            cache.files.insert(file.clone(), FileEntry { key, mutants });
        }
        // Prune entries for files that no longer have discovered mutants.
        let live: HashSet<&str> = discovered.iter().map(|d| d.file.as_str()).collect();
        cache.files.retain(|file, _| live.contains(file.as_str()));
        save_cache(root, &cache)?;
    }
    Ok(findings)
}

/// Build a sanitized seed file from an official `outcomes.json`: keep
/// only mutant names (workspace-relative), verdicts, the summary, and
/// keys computed from the current tree. Refuses a baseline that missed
/// any mutant.
///
/// # Errors
///
/// Returns [`Error`](crate::Error) if the official report cannot be
/// read or parsed, its baseline was not clean, or the seed cannot be
/// written.
/// Fold one official outcome record into the seed's per-file map.
/// Returns the mutant count contribution (1) or an error describing
/// why the official data is unusable as a seed.
/// How one outcome record contributed to the seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IngestCount {
    /// Records frozen into the seed.
    ingested: u64,
    /// Records omitted as suspicious (non-reusable per the
    /// [`super::verdict`] classifier). Counted so the record-count
    /// validation accounts for them explicitly: a seed with omissions
    /// is smaller than discovery, and that is expected, not an error.
    omitted: u64,
}

fn seed_ingest_outcome(
    outcome: &Value,
    root: &Path,
    state: &TreeState,
    from: &Path,
    files: &mut BTreeMap<String, Value>,
) -> Result<IngestCount, Error> {
    let bad = |reason: String| Error::Parse {
        path: from.to_owned(),
        reason,
    };
    if outcome.get("scenario").and_then(Value::as_str) == Some("Baseline") {
        return Ok(IngestCount {
            ingested: 0,
            omitted: 0,
        });
    }
    let mutant = outcome.pointer("/scenario/Mutant");
    let (Some(name), Some(package), Some(file), Some(verdict)) = (
        mutant.and_then(|m| m.get("name")).and_then(Value::as_str),
        mutant
            .and_then(|m| m.get("package"))
            .and_then(Value::as_str),
        mutant.and_then(|m| m.get("file")).and_then(Value::as_str),
        outcome.get("summary").and_then(Value::as_str),
    ) else {
        return Err(bad(String::from(
            "mutant record missing name/package/file/summary",
        )));
    };
    if !root.join(file).is_file() {
        return Err(bad(format!("mutant file `{file}` not in this tree")));
    }
    // classify BEFORE inserting. A suspicious outcome (signal
    // death, timeout, wrong-crate build failure, malformed record) is
    // NEVER frozen into the seed — it is omitted but counted, so its
    // file misses the partition on the next run (the seed's mutant set
    // no longer covers discovery) and the mutant is re-tested.
    if super::verdict::classify(outcome, &state.rdeps) == super::verdict::VerdictClass::NonReusable
    {
        return Ok(IngestCount {
            ingested: 0,
            omitted: 1,
        });
    }
    let key = input_key(state, root, file, package)?;
    let entry = files.entry(file.to_owned()).or_insert_with(|| {
        Value::Object(
            [
                ("key".to_owned(), key_to_json(&key)),
                ("package".to_owned(), Value::String(package.to_owned())),
                ("mutants".to_owned(), Value::Object(serde_json::Map::new())),
            ]
            .into_iter()
            .collect(),
        )
    });
    if let Some(mutants) = entry.get_mut("mutants").and_then(Value::as_object_mut) {
        if mutants.contains_key(name) {
            return Err(bad(format!(
                "duplicate mutant record `{name}` in official baseline"
            )));
        }
        mutants.insert(name.to_owned(), Value::String(verdict.to_owned()));
    }
    Ok(IngestCount {
        ingested: 1,
        omitted: 0,
    })
}

/// Build a sanitized cache seed from an official baseline.
///
/// Reads the official `outcomes.json` at `from`, validates it is a
/// complete clean baseline (zero missed mutants, record count matches
/// the total), recomputes the input key for every mutant file against
/// the current tree, and writes the seed to `out`.
///
/// # Errors
///
/// Returns [`Error`] if the baseline is missing, malformed,
/// incomplete, or not clean, or if the seed cannot be written.
pub fn seed_cache(root: &Path, from: &Path, out: &Path) -> Result<(), Error> {
    if !from.is_file() {
        return Err(Error::MissingFile(from.to_owned()));
    }
    let text = std::fs::read_to_string(from).map_err(Error::Io)?;
    let json: Value = serde_json::from_str(&text).map_err(|e| Error::Parse {
        path: from.to_owned(),
        reason: e.to_string(),
    })?;
    let bad = |reason: String| Error::Parse {
        path: from.to_owned(),
        reason,
    };
    let summary = |key: &str| json.get(key).and_then(Value::as_u64).unwrap_or(0);
    if summary("missed") != 0 {
        return Err(bad(String::from(
            "official baseline missed mutants; refusing to seed from it",
        )));
    }
    let state = tree_state(root)?;
    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    let mut ingested = 0u64;
    let mut omitted = 0u64;
    for outcome in json
        .get("outcomes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let count = seed_ingest_outcome(outcome, root, &state, from, &mut files)?;
        ingested += count.ingested;
        omitted += count.omitted;
    }
    // Suspicious outcomes are omitted from the seed but still counted:
    // the seed's mutant set is then smaller than discovery, and that
    // is expected (their files will miss the partition and re-test).
    if ingested + omitted != summary("total_mutants") {
        return Err(bad(format!(
            "record count {} (ingested {ingested} + omitted suspicious {omitted}) != total_mutants {}",
            ingested + omitted,
            summary("total_mutants")
        )));
    }
    let seed = Value::Object(
        [
            ("version".to_owned(), Value::from(SEED_VERSION)),
            (
                "created_from".to_owned(),
                Value::String(
                    from.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("<unknown>")
                        .to_owned(),
                ),
            ),
            (
                "summary".to_owned(),
                Value::Object(
                    ["total_mutants", "caught", "missed", "timeout", "unviable"]
                        .into_iter()
                        .map(|k| (k.to_owned(), Value::from(summary(k))))
                        .chain([("omitted_suspicious".to_owned(), Value::from(omitted))])
                        .collect(),
                ),
            ),
            (
                "files".to_owned(),
                Value::Object(files.into_iter().collect()),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let text = serde_json::to_string_pretty(&seed).map_err(|e| Error::Parse {
        path: out.to_owned(),
        reason: e.to_string(),
    })?;
    if text.contains("/home/") || text.contains("/root/") {
        return Err(bad(String::from(
            "seed would contain an absolute home path; refusing to write",
        )));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    std::fs::write(out, text + "\n").map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_bin_reads_declared_override_and_default_in_isolated_processes() {
        const CASE: &str = "HATCHLING_TEST_CARGO_BIN_CASE";
        if let Ok(case) = std::env::var(CASE) {
            assert_eq!(
                cargo_bin(),
                if case == "custom" {
                    "/private/cargo-wrapper"
                } else {
                    "cargo"
                }
            );
            with_fake_cargo(Path::new("/test-only/cargo"), || {
                assert_eq!(cargo_bin(), "/test-only/cargo");
            });
            return;
        }
        for case in ["custom", "default"] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "checks::mutant_cache::tests::cargo_bin_reads_declared_override_and_default_in_isolated_processes", "--nocapture"])
                .env_clear().env(CASE, case);
            if case == "custom" {
                child.env("HATCHLING_CARGO", "/private/cargo-wrapper");
            }
            let output = child.output().unwrap();
            assert!(output.status.success(), "{case}: {output:?}");
            assert!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .contains("1 passed")
            );
        }
    }

    /// Fake workspace root under `/tmp` directly (never `temp_dir()`:
    /// see the `fake_root` rationale in `checks::mutants`).
    ///
    /// Unique per call (pid + atomic counter): parallel tests must never
    /// share a directory. Two tests once shared `fake_root("seed-version")`
    /// and one deleted the dir under the other mid-test, panicking with
    /// `NotFound` and producing a false "caught" in mutation testing.
    fn fake_root(name: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root = Path::new("/tmp").join(format!(
            "hatchling-dev-cache-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// Recursively copy a directory tree.
    fn copy_dir(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let dst_path = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &dst_path);
            } else {
                std::fs::copy(entry.path(), dst_path).unwrap();
            }
        }
    }

    /// The committed fixture, as the gate sees it: `CARGO_MANIFEST_DIR`
    /// is the `hatchling-dev` crate dir in both normal and
    /// cargo-mutants scratch builds, so this is hermetic (no cwd
    /// dependence). The fixture is data, never a workspace member, so
    /// no mutant can touch it — this is not a tripwire.
    fn real_fixture_seeds_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("seeds")
    }

    /// Minimal workspace: `base` (leaf), `mid` depends on `base`,
    /// `top` depends on `mid`, `lone` depends on nothing.
    fn synthetic_workspace(name: &str) -> PathBuf {
        let root = fake_root(name);
        for (crate_name, deps) in [("base", ""), ("mid", "base"), ("top", "mid"), ("lone", "")] {
            let dep_line = if deps.is_empty() {
                String::new()
            } else {
                format!("[dependencies]\n{deps} = {{ path = \"../{deps}\" }}\n")
            };
            write(
                &root.join("crates").join(crate_name).join("Cargo.toml"),
                &format!(
                    "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{dep_line}"
                ),
            );
            write(
                &root
                    .join("crates")
                    .join(crate_name)
                    .join("src")
                    .join("lib.rs"),
                &format!("// {crate_name} lib\npub fn f() -> u32 {{ 1 }}\n"),
            );
        }
        write(&root.join("Cargo.lock"), "# lock\n");
        root
    }

    #[test]
    fn sha256_hex_known_vector() {
        // SHA-256("abc"). A `sha256_hex -> String::new()` mutant and any
        // byte-order/format change fails here.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn escape_regex_escapes_specials() {
        // Dots and other regex metacharacters must be literal: the `--re`
        // pattern is anchored, but `a.rs` must not match `aXrs`.
        assert_eq!(escape_regex("crates/a/src/b.rs"), "crates/a/src/b\\.rs");
        assert_eq!(escape_regex("a+b (c)"), "a\\+b\\ \\(c\\)");
        assert_eq!(escape_regex("plain_1-2/three"), "plain_1-2/three");
    }

    #[test]
    fn skipped_dir_cases() {
        for dir in [
            "target",
            ".git",
            ".mutation-cache",
            "mutants.scratch",
            "seeds",
            "mutants.out",
            "mutants.out.old",
        ] {
            assert!(skipped_dir(dir), "{dir} should be skipped");
        }
        assert!(!skipped_dir("src"));
        assert!(!skipped_dir("tests"));
    }

    #[test]
    fn package_hash_is_deterministic_and_content_sensitive() {
        let root = fake_root("pkg-hash");
        let pkg = root.join("crates").join("a");
        write(&pkg.join("src").join("lib.rs"), "pub fn f() {}\n");
        write(&pkg.join("src").join("sub").join("m.rs"), "pub fn g() {}\n");
        let h1 = hash_package_dir(&pkg).unwrap();
        let h2 = hash_package_dir(&pkg).unwrap();
        assert_eq!(h1, h2, "hash must be deterministic");
        // Kills a `hash_package_dir -> Ok(String::new())` mutant and any
        // mutant that drops a file from the walk.
        write(
            &pkg.join("src").join("sub").join("m.rs"),
            "pub fn g() { 1 }\n",
        );
        assert_ne!(
            hash_package_dir(&pkg).unwrap(),
            h1,
            "content change must move the hash"
        );
        write(&pkg.join("tests").join("t.rs"), "#[test] fn t() {}\n");
        assert_ne!(
            hash_package_dir(&pkg).unwrap(),
            h1,
            "new file must move the hash"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reverse_deps_cover_transitive_and_dev_edges() {
        let root = synthetic_workspace("rdeps");
        // top -> mid -> base, so base's reverse deps are {mid, top}.
        let rdeps = reverse_deps(&root).unwrap();
        let get = |n: &str| rdeps.get(n).cloned().unwrap_or_default();
        assert!(get("base").contains("mid"), "direct reverse dep missing");
        assert!(
            get("base").contains("top"),
            "transitive reverse dep missing"
        );
        assert!(!get("base").contains("lone"), "unrelated crate leaked in");
        assert!(get("lone").is_empty(), "leaf reverse deps must be empty");
        assert!(get("mid").contains("top"));
        assert!(
            !get("top").contains("base"),
            "dependency direction inverted"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reverse_deps_see_dev_dependencies() {
        // A test-helper crate pulled in via [dev-dependencies] is a real
        // edge: its mutants are observed by the dependent's tests.
        let root = fake_root("rdeps-dev");
        write(
            &root.join("crates").join("helper").join("Cargo.toml"),
            "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(
            &root.join("crates").join("user").join("Cargo.toml"),
            "[package]\nname = \"user\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             [dev-dependencies]\nhelper = { path = \"../helper\" }\n",
        );
        let rdeps = reverse_deps(&root).unwrap();
        assert!(
            rdeps.get("helper").is_some_and(|s| s.contains("user")),
            "dev-dependency edge missing from reverse deps"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn suite_hash_invalidates_on_downstream_change_only() {
        // mid depends on base: changing mid's tests must invalidate
        // base's key, changing lone must not.
        let root = synthetic_workspace("suite");
        let state = tree_state(&root).unwrap();
        let before = input_key(&state, &root, "crates/base/src/lib.rs", "base").unwrap();
        // Downstream test change -> suite_hash moves.
        write(
            &root.join("crates").join("mid").join("tests").join("t.rs"),
            "#[test] fn t() {}\n",
        );
        let state = tree_state(&root).unwrap();
        let after = input_key(&state, &root, "crates/base/src/lib.rs", "base").unwrap();
        assert_ne!(
            before.suite_hash, after.suite_hash,
            "downstream change must invalidate"
        );
        assert_eq!(
            before.file_hash, after.file_hash,
            "file_hash must not move for other files"
        );
        // Unrelated crate change -> key identical.
        write(
            &root.join("crates").join("lone").join("src").join("lib.rs"),
            "// changed\n",
        );
        let state = tree_state(&root).unwrap();
        let again = input_key(&state, &root, "crates/base/src/lib.rs", "base").unwrap();
        assert_eq!(after, again, "unrelated change must not invalidate");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn test_key() -> InputKey {
        InputKey {
            file_hash: String::from("fh"),
            suite_hash: String::from("sh"),
            lock_hash: String::from("lh"),
            toolchain_id: String::from("ti"),
            env_id: String::from("ei"),
            config_hash: String::from("ch"),
        }
    }

    fn test_cache() -> Cache {
        let mut cache = Cache::default();
        let mut mutants = BTreeMap::new();
        mutants.insert(String::from("f.rs:1:1: m1"), String::from("CaughtMutant"));
        mutants.insert(String::from("f.rs:2:2: m2"), String::from("Unviable"));
        cache.files.insert(
            String::from("crates/a/src/f.rs"),
            FileEntry {
                key: test_key(),
                mutants,
            },
        );
        cache
    }

    fn test_discovered() -> Vec<Discovered> {
        vec![
            Discovered {
                name: String::from("f.rs:1:1: m1"),
                package: String::from("a"),
                file: String::from("crates/a/src/f.rs"),
            },
            Discovered {
                name: String::from("f.rs:2:2: m2"),
                package: String::from("a"),
                file: String::from("crates/a/src/f.rs"),
            },
            Discovered {
                name: String::from("g.rs:1:1: m3"),
                package: String::from("a"),
                file: String::from("crates/a/src/g.rs"),
            },
        ]
    }

    #[test]
    fn partition_serves_hits_and_reports_misses() {
        // Real `partition` on a synthetic workspace: base's entry hits
        // with a computed key, the uncached file misses.
        let root = synthetic_workspace("partition");
        let state = tree_state(&root).unwrap();
        let key = input_key(&state, &root, "crates/base/src/lib.rs", "base").unwrap();
        let mut mutants = BTreeMap::new();
        mutants.insert(String::from("lib.rs:1:1: m1"), String::from("CaughtMutant"));
        let mut cache = Cache::default();
        cache.files.insert(
            String::from("crates/base/src/lib.rs"),
            FileEntry { key, mutants },
        );
        let discovered = vec![
            Discovered {
                name: String::from("lib.rs:1:1: m1"),
                package: String::from("base"),
                file: String::from("crates/base/src/lib.rs"),
            },
            Discovered {
                name: String::from("lib.rs:2:2: m2"),
                package: String::from("mid"),
                file: String::from("crates/mid/src/lib.rs"),
            },
        ];
        let part = partition(&state, &root, &discovered, &cache).unwrap();
        assert!(
            part.hits.contains_key("crates/base/src/lib.rs"),
            "base should hit"
        );
        assert_eq!(
            part.misses,
            vec![String::from("crates/mid/src/lib.rs")],
            "mid should miss"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn partition_retests_on_any_input_change() {
        // Each key component's invalidation is exercised: file content,
        // downstream tests, lockfile, and (structurally) risky verdicts.
        let root = synthetic_workspace("partition-inv");
        let file = "crates/base/src/lib.rs";
        let key_of = |root: &Path| {
            let state = tree_state(root).unwrap();
            input_key(&state, root, file, "base").unwrap()
        };
        let good_key = key_of(&root);
        let mut mutants = BTreeMap::new();
        mutants.insert(String::from("lib.rs:1:1: m1"), String::from("CaughtMutant"));
        let discovered = vec![Discovered {
            name: String::from("lib.rs:1:1: m1"),
            package: String::from("base"),
            file: String::from(file),
        }];
        let entry_for = |key: InputKey| {
            let mut cache = Cache::default();
            cache.files.insert(
                String::from(file),
                FileEntry {
                    key,
                    mutants: mutants.clone(),
                },
            );
            cache
        };
        let misses_with = |cache: &Cache| {
            // Recomputed per call: each write below changes the tree,
            // and the partition must see the new tree.
            let state = tree_state(&root).unwrap();
            partition(&state, &root, &discovered, cache).unwrap().misses
        };
        // Exact key -> hit.
        assert!(
            misses_with(&entry_for(good_key.clone())).is_empty(),
            "exact key must hit"
        );
        // File content changed -> miss.
        write(
            &root.join(file),
            "// base lib changed\npub fn f() -> u32 { 2 }\n",
        );
        assert_eq!(
            misses_with(&entry_for(good_key.clone())),
            vec![String::from(file)],
            "content change must miss"
        );
        // Downstream (mid) test change -> miss even with file restored.
        write(&root.join(file), "// base lib\npub fn f() -> u32 { 1 }\n");
        write(
            &root.join("crates").join("mid").join("tests").join("t.rs"),
            "#[test] fn t() {}\n",
        );
        assert_eq!(
            misses_with(&entry_for(good_key.clone())),
            vec![String::from(file)],
            "downstream change must miss"
        );
        // Lockfile change -> miss.
        let _ = std::fs::remove_file(root.join("crates").join("mid").join("tests").join("t.rs"));
        write(&root.join("Cargo.lock"), "# lock changed\n");
        assert_eq!(
            misses_with(&entry_for(good_key)),
            vec![String::from(file)],
            "lock change must miss"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn partition_never_reuses_risky_verdicts() {
        // A cached Timeout or MissedMutant forces a retest even when
        // every key component matches: timeouts are machine-sensitive
        // and a missed mutant is a live defect.
        let root = synthetic_workspace("partition-risky");
        let file = "crates/base/src/lib.rs";
        let state = tree_state(&root).unwrap();
        let key = input_key(&state, &root, file, "base").unwrap();
        let discovered = vec![Discovered {
            name: String::from("lib.rs:1:1: m1"),
            package: String::from("base"),
            file: String::from(file),
        }];
        for risky in ["Timeout", "MissedMutant"] {
            let mut mutants = BTreeMap::new();
            mutants.insert(String::from("lib.rs:1:1: m1"), String::from(risky));
            let mut cache = Cache::default();
            cache.files.insert(
                String::from(file),
                FileEntry {
                    key: key.clone(),
                    mutants,
                },
            );
            let part = partition(&state, &root, &discovered, &cache).unwrap();
            assert_eq!(
                part.misses,
                vec![String::from(file)],
                "{risky} must force a miss"
            );
            assert!(part.hits.is_empty());
        }
        // CaughtMutant and Unviable are reusable.
        for safe in ["CaughtMutant", "Unviable"] {
            let mut mutants = BTreeMap::new();
            mutants.insert(String::from("lib.rs:1:1: m1"), String::from(safe));
            let mut cache = Cache::default();
            cache.files.insert(
                String::from(file),
                FileEntry {
                    key: key.clone(),
                    mutants,
                },
            );
            let part = partition(&state, &root, &discovered, &cache).unwrap();
            assert!(part.misses.is_empty(), "{safe} must be reusable");
            assert!(part.hits.contains_key(file));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_omits_suspicious_outcomes_and_forces_partition_miss() {
        // a signal-died build must never be frozen into the
        // seed. The seed omits it (counted as `omitted_suspicious`),
        // and the next partition misses its file — the seed's mutant
        // set no longer covers discovery — forcing a re-test.
        let root = synthetic_workspace("seed-omit");
        let file = "crates/base/src/lib.rs";
        let clean_phases = serde_json::json!([
            {"phase": "Build", "process_status": "Success", "argv": ["cargo", "test", "--no-run"]},
            {"phase": "Test", "process_status": {"Failure": 101}, "argv": ["cargo", "test"]}
        ]);
        let sus_phases = serde_json::json!([
            {"phase": "Build", "process_status": {"Signalled": 9}, "argv": ["cargo", "test", "--no-run"]}
        ]);
        let baseline = serde_json::json!({
            "outcomes": [
                {"scenario": "Baseline", "summary": "Success"},
                {
                    "scenario": {"Mutant": {"name": "m-clean", "package": "base", "file": file}},
                    "summary": "CaughtMutant",
                    "phase_results": clean_phases
                },
                {
                    "scenario": {"Mutant": {"name": "m-sus", "package": "base", "file": file}},
                    "summary": "Failure",
                    "phase_results": sus_phases
                }
            ],
            "total_mutants": 2,
            "caught": 1,
            "missed": 0,
            "timeout": 0,
            "unviable": 0
        });
        let from = root.join("baseline.json");
        std::fs::write(&from, serde_json::to_string(&baseline).unwrap()).unwrap();
        let out = root.join("seed.json");
        seed_cache(&root, &from, &out).unwrap();
        // The seed omits the suspicious mutant but counts the omission.
        let seed_text = std::fs::read_to_string(&out).unwrap();
        let seed: Value = serde_json::from_str(&seed_text).unwrap();
        assert_eq!(
            seed.pointer("/summary/omitted_suspicious")
                .and_then(Value::as_u64),
            Some(1),
            "omitted suspicious outcome must be counted"
        );
        let mutants = seed
            .get("files")
            .and_then(|f| f.get(file))
            .and_then(|e| e.get("mutants"))
            .and_then(Value::as_object)
            .unwrap();
        assert!(
            mutants.contains_key("m-clean"),
            "clean mutant must be seeded"
        );
        assert!(
            !mutants.contains_key("m-sus"),
            "suspicious mutant must be omitted, never frozen"
        );
        // Partition: discovery finds both mutants, but the seed only
        // covers the clean one — the file must miss, forcing a re-test
        // of the suspicious mutant.
        let cache = import_seed(&root, &out).unwrap();
        let state = tree_state(&root).unwrap();
        let discovered = ["m-clean", "m-sus"]
            .into_iter()
            .map(|name| Discovered {
                name: name.to_owned(),
                package: String::from("base"),
                file: String::from(file),
            })
            .collect::<Vec<_>>();
        let part = partition(&state, &root, &discovered, &cache).unwrap();
        assert_eq!(
            part.misses,
            vec![String::from(file)],
            "suspicious omission must force a partition miss"
        );
        assert!(part.hits.is_empty(), "no hits when a mutant was omitted");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn partition_rejects_unknown_verdict_strings() {
        // the reuse predicate is an allowlist. A hand-edited
        // seed smuggling in a "Failure" (or any unknown) verdict must
        // miss, even though the historical denylist only rejected
        // MissedMutant/Timeout.
        let root = synthetic_workspace("partition-allowlist");
        let file = "crates/base/src/lib.rs";
        let state = tree_state(&root).unwrap();
        let key = input_key(&state, &root, file, "base").unwrap();
        let discovered = vec![Discovered {
            name: String::from("lib.rs:1:1: m1"),
            package: String::from("base"),
            file: String::from(file),
        }];
        for bad in ["Failure", "Success", "Bogus"] {
            let mut mutants = BTreeMap::new();
            mutants.insert(String::from("lib.rs:1:1: m1"), String::from(bad));
            let mut cache = Cache::default();
            cache.files.insert(
                String::from(file),
                FileEntry {
                    key: key.clone(),
                    mutants,
                },
            );
            let part = partition(&state, &root, &discovered, &cache).unwrap();
            assert_eq!(
                part.misses,
                vec![String::from(file)],
                "{bad} must force a miss under the allowlist"
            );
            assert!(part.hits.is_empty());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn key_equality_is_fieldwise() {
        // A `PartialEq` derived on all fields: changing any one field
        // breaks equality. Kills mutants that drop a field from the
        // comparison.
        let base = test_key();
        for mutated in [
            InputKey {
                file_hash: String::from("X"),
                ..base.clone()
            },
            InputKey {
                suite_hash: String::from("X"),
                ..base.clone()
            },
            InputKey {
                lock_hash: String::from("X"),
                ..base.clone()
            },
            InputKey {
                toolchain_id: String::from("X"),
                ..base.clone()
            },
            InputKey {
                env_id: String::from("X"),
                ..base.clone()
            },
            InputKey {
                config_hash: String::from("X"),
                ..base.clone()
            },
        ] {
            assert_ne!(base, mutated, "key equality must cover every field");
        }
        assert_eq!(base, base.clone());
    }

    #[test]
    fn config_hash_matches_file_content() {
        // Kills: whole-function replacements of `config_hash`.
        let root = fake_root("config-hash");
        write(&root.join(".cargo/mutants.toml"), "[mutants]\n");
        assert_eq!(config_hash(&root), sha256_hex(b"[mutants]\n"));
        // Missing file hashes as empty.
        let empty_root = fake_root("config-hash-missing");
        assert_eq!(config_hash(&empty_root), sha256_hex(b""));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&empty_root);
    }

    #[test]
    fn import_seed_refuses_dirty_baseline() {
        let root = fake_root("dirty-seed");
        let seed = root.join("seed.json");
        write(
            &seed,
            r#"{"version": 1, "summary": {"total_mutants": 2, "caught": 1, "missed": 1, "timeout": 0, "unviable": 0}, "files": {}}"#,
        );
        let err = import_seed(&root, &seed).unwrap_err();
        assert!(
            err.to_string().contains("dirty baseline"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_seed_rejects_bad_version() {
        let root = fake_root("seed-version");
        let seed = root.join("seed.json");
        write(
            &seed,
            r#"{"version": 999, "summary": {"missed": 0}, "files": {}}"#,
        );
        let err = import_seed(&root, &seed).unwrap_err();
        assert!(
            err.to_string().contains("version"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cache_roundtrip_preserves_entries() {
        let root = fake_root("roundtrip");
        let cache = test_cache();
        save_cache(&root, &cache).unwrap();
        let loaded = load_cache(&root).unwrap();
        assert_eq!(loaded.files.len(), 1);
        let entry = loaded.files.get("crates/a/src/f.rs").unwrap();
        assert_eq!(entry.key, test_key());
        assert_eq!(entry.mutants.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_report_creates_mutants_out_on_a_zero_miss_run() {
        // Regression ( WSL2): on an unchanged tree every file is a
        // cache hit, cargo-mutants never runs, and `mutants.out/` does
        // not exist. The merge must still write the report, with a
        // synthetic baseline, and the floor must hold on it.
        let root = fake_root("merge-zero-miss");
        let discovered = test_discovered();
        let mut hits = BTreeMap::new();
        hits.insert(
            String::from("crates/a/src/f.rs"),
            vec![
                (String::from("f.rs:1:1: m1"), String::from("CaughtMutant")),
                (String::from("f.rs:2:2: m2"), String::from("Unviable")),
            ],
        );
        hits.insert(
            String::from("crates/a/src/g.rs"),
            vec![(String::from("g.rs:1:1: m3"), String::from("CaughtMutant"))],
        );
        assert!(
            !root.join("mutants.out").exists(),
            "precondition: no mutants.out/"
        );
        merge_report(&root, &discovered, &hits, &BTreeMap::new(), None).unwrap();
        let summary = super::super::mutants::read_summary(&root).unwrap();
        assert_eq!(summary.total, 3);
        assert_eq!(summary.caught, 2);
        assert_eq!(summary.unviable, 1);
        assert_eq!(summary.missed, 0);
        let findings = super::super::mutants::evaluate(&summary, Vec::new());
        assert!(
            findings.is_empty(),
            "the floor must hold on a zero-miss merge"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_report_recomputes_summary() {
        let root = fake_root("merge");
        let discovered = test_discovered();
        // f.rs served from cache (1 caught, 1 unviable), g.rs fresh (1 caught).
        let mut hits = BTreeMap::new();
        hits.insert(
            String::from("crates/a/src/f.rs"),
            vec![
                (String::from("f.rs:1:1: m1"), String::from("CaughtMutant")),
                (String::from("f.rs:2:2: m2"), String::from("Unviable")),
            ],
        );
        let mut fresh = BTreeMap::new();
        fresh.insert(
            String::from("g.rs:1:1: m3"),
            serde_json::json!({
                "scenario": {"Mutant": {"name": "g.rs:1:1: m3"}},
                "summary": "CaughtMutant",
            }),
        );
        std::fs::create_dir_all(root.join("mutants.out")).unwrap();
        merge_report(&root, &discovered, &hits, &fresh, None).unwrap();
        let summary = super::super::mutants::read_summary(&root).unwrap();
        assert_eq!(summary.total, 3);
        assert_eq!(summary.caught, 2);
        assert_eq!(summary.unviable, 1);
        assert_eq!(summary.missed, 0);
        assert_eq!(summary.timeout, 0);
        // The floor holds on the merged report.
        let findings = super::super::mutants::evaluate(&summary, Vec::new());
        assert!(findings.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_report_errors_on_missing_outcome() {
        // A discovered mutant with neither a fresh nor a cached outcome
        // is a fail-closed error, never a silent drop.
        let root = fake_root("merge-missing");
        let discovered = test_discovered();
        std::fs::create_dir_all(root.join("mutants.out")).unwrap();
        let err =
            merge_report(&root, &discovered, &BTreeMap::new(), &BTreeMap::new(), None).unwrap_err();
        assert!(
            err.to_string().contains("no outcome"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn env_id_from_maps_require_cell_flag() {
        // Kills: replace env_id_from with String::new(),
        // replace env_id_from with "xyzzy". These were the only real
        // misses in the v3 WSL2 run; the seed-consistency check
        // (now binary-only) was their previous killer.
        assert_eq!(env_id_from(Some("1")), "HATCHLING_REQUIRE_CELL=1");
        assert_eq!(env_id_from(None), "HATCHLING_REQUIRE_CELL=unset");
    }

    #[test]
    fn seed_consistency_passes_on_matching_synthetic_seed() {
        // The check itself never runs against the real tree in unit
        // tests (that would be the tripwire again); a synthetic
        // workspace exercises its logic instead.
        let root = synthetic_workspace("seed-check-ok");
        let state = tree_state_with_env(&root, Some("1")).unwrap();
        let rel = "crates/base/src/lib.rs";
        let key = input_key(&state, &root, rel, "base").unwrap();
        let seed = serde_json::json!({
            "files": { rel: { "package": "base", "key": key_to_json(&key) } },
        });
        write(&seed_path(&root), &seed.to_string());
        let findings = verify_seed_keys(&seed_path(&root), &root, Some("1")).unwrap();
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_consistency_reports_modified_file() {
        // Kills: finding-swallowing mutants in check_seed_consistency
        // (skip the loop, flip the comparison, return Ok(vec![])).
        let root = synthetic_workspace("seed-check-dirty");
        let state = tree_state_with_env(&root, Some("1")).unwrap();
        let rel = "crates/base/src/lib.rs";
        let key = input_key(&state, &root, rel, "base").unwrap();
        let seed = serde_json::json!({
            "files": { rel: { "package": "base", "key": key_to_json(&key) } },
        });
        write(&seed_path(&root), &seed.to_string());
        // Corrupt the file after the seed was built: the hashes must
        // mismatch (file_hash directly, suite_hash via the package).
        write(&root.join(rel), "// base lib\npub fn f() -> u32 { 2 }\n");
        let findings = verify_seed_keys(&seed_path(&root), &root, Some("1")).unwrap();
        assert!(!findings.is_empty(), "expected findings, got none");
        assert!(
            findings.iter().all(|f| f.message.contains(rel)),
            "every finding names the file: {findings:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_consistency_rejects_empty_seed() {
        // Kills: the empty-seed guard's error return replaced with
        // Ok(vec![]).
        let root = synthetic_workspace("seed-check-empty");
        write(&seed_path(&root), r#"{"files": {}}"#);
        let err = verify_seed_keys(&seed_path(&root), &root, Some("1")).unwrap_err();
        assert!(
            err.to_string().contains("no files"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Copy the committed fixture into a temp root mimicking the
    /// workspace layout (`<tmp>/crates/hatchling-dev/seeds/`), so the
    /// thin `check_seed_consistency` wrapper is exercised without
    /// reading the live tree.
    fn fixture_copy(name: &str) -> PathBuf {
        let root = fake_root(name);
        let dst = root.join("crates").join("hatchling-dev").join("seeds");
        std::fs::create_dir_all(&dst).unwrap();
        let src = real_fixture_seeds_dir();
        std::fs::copy(src.join("fixture-seed.json"), dst.join("fixture-seed.json")).unwrap();
        copy_dir(&src.join("fixture-tree"), &dst.join("fixture-tree"));
        root
    }

    #[test]
    fn seed_check_wrapper_accepts_clean_fixture_copy() {
        // Kills: fixture_seed_path/fixture_tree_path ->
        // Default::default() (empty paths fail), and any env-pin
        // mutant (wrong env_id yields findings).
        let root = fixture_copy("seed-check-wrapper-clean");
        let findings = check_seed_consistency(&root).unwrap();
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_check_wrapper_reports_divergent_fixture_copy() {
        // Kills: check_seed_consistency -> Ok(vec![]) — the wrapper
        // must propagate findings, not swallow them.
        let root = fixture_copy("seed-check-wrapper-dirty");
        let victim = root
            .join("crates")
            .join("hatchling-dev")
            .join("seeds")
            .join("fixture-tree")
            .join("crates/fx-base/src/lib.rs");
        write(&victim, "// diverged\n");
        let findings = check_seed_consistency(&root).unwrap();
        assert!(!findings.is_empty(), "expected findings, got none");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Held by the two tests that change the toolchain override, so one
    /// cannot put the default back between the other's write and read.
    static TOOLCHAIN_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn toolchain_id_contains_mutants_version() {
        // Kills: replace toolchain_id with Ok(String::new()),
        // replace toolchain_id with Ok("xyzzy"). This is the one test
        // that exercises the real `cargo mutants --version` shell-out:
        // it clears the override and calls toolchain_id_from_tools
        // directly while holding the lock, so parallel tests keep
        // observing the sentinel (calling toolchain_id here would
        // deadlock on the held guard). The result is captured before
        // the unwrap so a missing tool cannot leave the override
        // cleared for the rest of the suite.
        let _serial = TOOLCHAIN_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut guard = TOOLCHAIN_OVERRIDE.lock().unwrap();
        *guard = None;
        let result = toolchain_id_from_tools();
        *guard = Some(String::from(TEST_TOOLCHAIN_ID));
        drop(guard);
        let id = result.unwrap();
        assert!(
            id.contains("cargo-mutants"),
            "toolchain_id must identify cargo-mutants, got: {id}"
        );
        assert!(!id.is_empty(), "toolchain_id must not be empty");
    }

    #[test]
    fn toolchain_override_is_honored() {
        // Kills: deleting the #[cfg(test)] override branch in
        // toolchain_id (it would shell out instead of returning the
        // sentinel). Read-only: the override's default is the sentinel,
        // so nothing shared changes. Setting a distinct value, even for a
        // moment, broke a parallel cache test that computed one key
        // before that moment and one during it ("exact key must hit").
        let _serial = TOOLCHAIN_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(toolchain_id().unwrap(), TEST_TOOLCHAIN_ID);
    }

    #[test]
    fn resolve_dep_target_matches_canonical_path() {
        // Kills: replace == with != in resolve_dep_target.
        let root = fake_root("resolve-dep");
        write(
            &root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        let mut members = HashMap::new();
        members.insert("a".to_owned(), root.join("crates/a"));
        let member_dir = root.join("crates/b");
        std::fs::create_dir_all(&member_dir).unwrap();
        // Path-based spec: "../a" from crates/b should resolve to "a".
        let spec = serde_json::json!({"path": "../a"});
        let result = resolve_dep_target("renamed", &spec, &members, &member_dir);
        assert_eq!(result, Some("a".to_owned()));
        // Non-matching path should fail.
        let spec = serde_json::json!({"path": "../nonexistent"});
        let result = resolve_dep_target("renamed", &spec, &members, &member_dir);
        assert_eq!(result, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_seed_rejects_dirty_baseline() {
        // Kills: replace != with == in import_seed (missed check).
        let root = fake_root("seed-dirty");
        write(&root.join("Cargo.toml"), "[workspace]\n");
        let seed = serde_json::json!({
            "version": 1,
            "summary": {"missed": 1, "total_mutants": 10},
            "files": {}
        });
        let path = root.join("seed.json");
        std::fs::write(&path, serde_json::to_string(&seed).unwrap()).unwrap();
        let result = import_seed(&root, &path);
        assert!(result.is_err(), "dirty baseline must be rejected");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_seed_rejects_wrong_version() {
        // Kills: replace != with == in import_seed (version check).
        let root = fake_root("seed-version");
        write(&root.join("Cargo.toml"), "[workspace]\n");
        let seed = serde_json::json!({
            "version": 999,
            "summary": {"missed": 0, "total_mutants": 10},
            "files": {}
        });
        let path = root.join("seed.json");
        std::fs::write(&path, serde_json::to_string(&seed).unwrap()).unwrap();
        let result = import_seed(&root, &path);
        assert!(result.is_err(), "wrong version must be rejected");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn package_for_file_finds_owner() {
        // Kills: replace package_for_file with None/Some("")/Some("xyzzy").
        let root = fake_root("pkg-for-file");
        let members = vec![
            super::super::Member {
                name: "alpha".to_owned(),
                dir: root.join("crates/alpha"),
            },
            super::super::Member {
                name: "beta".to_owned(),
                dir: root.join("crates/beta"),
            },
        ];
        assert_eq!(
            package_for_file(&root, "crates/alpha/src/lib.rs", &members),
            Some("alpha".to_owned())
        );
        assert_eq!(
            package_for_file(&root, "crates/beta/src/main.rs", &members),
            Some("beta".to_owned())
        );
        assert_eq!(package_for_file(&root, "unknown/file.rs", &members), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn toml_value_to_json_converts_types() {
        // Kills: replace toml_value_to_json with Default::default().
        let toml = toml::Value::String("hello".to_owned());
        let json = toml_value_to_json(&toml);
        assert_eq!(json, Value::String("hello".to_owned()));
        let toml = toml::Value::Integer(42);
        let json = toml_value_to_json(&toml);
        assert_eq!(json, Value::Number(42.into()));
    }

    #[test]
    fn seed_ingest_outcome_skips_baseline() {
        // Kills: replace == with != in seed_ingest_outcome (baseline check).
        let root = fake_root("ingest-baseline");
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        );
        write(
            &root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(&root.join("Cargo.lock"), "# fake lock\n");
        let state = tree_state_with_env(&root, Some("1")).unwrap();
        let mut files = BTreeMap::new();
        let baseline = serde_json::json!({
            "scenario": "Baseline",
            "summary": "Success"
        });
        let count =
            seed_ingest_outcome(&baseline, &root, &state, &root.join("f"), &mut files).unwrap();
        assert_eq!(
            count,
            IngestCount {
                ingested: 0,
                omitted: 0
            },
            "baseline must contribute 0"
        );
        assert!(files.is_empty(), "baseline must not create file entry");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_ingest_outcome_rejects_missing_file() {
        // Kills: delete ! in seed_ingest_outcome (file existence check).
        let root = fake_root("ingest-missing");
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        );
        write(
            &root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(&root.join("Cargo.lock"), "# fake lock\n");
        let state = tree_state_with_env(&root, Some("1")).unwrap();
        let mut files = BTreeMap::new();
        let outcome = serde_json::json!({
            "scenario": {"Mutant": {"name": "m", "package": "p", "file": "nonexistent.rs"}},
            "summary": "CaughtMutant"
        });
        let result = seed_ingest_outcome(&outcome, &root, &state, &root.join("f"), &mut files);
        assert!(result.is_err(), "missing file must be rejected");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_cache_rejects_missing_baseline() {
        // Kills: delete ! in seed_cache (from.is_file check).
        // Must be MissingFile, not Io: the mutant would let it through to
        // read_to_string which fails with Io instead.
        let root = fake_root("seed-missing");
        let result = seed_cache(
            &root,
            &root.join("nonexistent.json"),
            &root.join("out.json"),
        );
        assert!(
            matches!(result, Err(Error::MissingFile(_))),
            "missing baseline must be MissingFile, got: {result:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_cache_rejects_dirty_baseline() {
        // Kills: replace != with == in seed_cache (missed check).
        let root = fake_root("seed-dirty2");
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        );
        write(
            &root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(&root.join("Cargo.lock"), "# fake lock\n");
        let baseline = serde_json::json!({
            "outcomes": [
                {"scenario": "Baseline", "summary": "Success"}
            ],
            "total_mutants": 0,
            "caught": 0,
            "missed": 1,
            "timeout": 0,
            "unviable": 0
        });
        let from = root.join("baseline.json");
        std::fs::write(&from, serde_json::to_string(&baseline).unwrap()).unwrap();
        let result = seed_cache(&root, &from, &root.join("out.json"));
        assert!(result.is_err(), "dirty baseline must be rejected");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_cache_accepts_correct_count() {
        // Kills: replace += with *= in seed_cache (count accumulation).
        // With *=, count stays 0 and 0 != 1 fails; with +=, count=1 matches.
        let root = fake_root("seed-count-ok");
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        );
        write(
            &root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(&root.join("Cargo.lock"), "# fake lock\n");
        write(&root.join("crates/a/src/lib.rs"), "pub fn f() {}\n");
        // Realistic phases: a clean CaughtMutant must be ingested, not
        // omitted as suspicious (the classifier requires phase_results).
        let phases = serde_json::json!([
            {"phase": "Build", "process_status": "Success", "argv": ["cargo", "test", "--no-run"]},
            {"phase": "Test", "process_status": {"Failure": 101}, "argv": ["cargo", "test"]}
        ]);
        let baseline = serde_json::json!({
            "outcomes": [
                {"scenario": "Baseline", "summary": "Success"},
                {
                    "scenario": {"Mutant": {"name": "m1", "package": "a", "file": "crates/a/src/lib.rs"}},
                    "summary": "CaughtMutant",
                    "phase_results": phases
                }
            ],
            "total_mutants": 1,
            "caught": 1,
            "missed": 0,
            "timeout": 0,
            "unviable": 0
        });
        let from = root.join("baseline.json");
        std::fs::write(&from, serde_json::to_string(&baseline).unwrap()).unwrap();
        let out = root.join("out.json");
        let result = seed_cache(&root, &from, &out);
        assert!(
            result.is_ok(),
            "correct count must be accepted, got: {result:?}"
        );
        assert!(out.is_file(), "seed output must be written");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_cache_rejects_home_path() {
        // Kills: replace || with && in seed_cache (path leak check).
        // A seed containing /home/ (but not /root/) must still be rejected.
        // The path "src/home/x.rs" contains the literal string "/home/".
        let root = fake_root("seed-home");
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        );
        write(
            &root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(&root.join("Cargo.lock"), "# fake lock\n");
        // File path contains literal "/home/" string.
        write(&root.join("crates/a/src/home/x.rs"), "pub fn f() {}\n");
        // Realistic phases so the outcome is ingested (not omitted as
        // suspicious): the /home/ path must reach the serialized seed.
        let phases = serde_json::json!([
            {"phase": "Build", "process_status": "Success", "argv": ["cargo", "test", "--no-run"]},
            {"phase": "Test", "process_status": {"Failure": 101}, "argv": ["cargo", "test"]}
        ]);
        let baseline = serde_json::json!({
            "outcomes": [
                {"scenario": "Baseline", "summary": "Success"},
                {
                    "scenario": {"Mutant": {"name": "m1", "package": "a", "file": "crates/a/src/home/x.rs"}},
                    "summary": "CaughtMutant",
                    "phase_results": phases
                }
            ],
            "total_mutants": 1,
            "caught": 1,
            "missed": 0,
            "timeout": 0,
            "unviable": 0
        });
        let from = root.join("baseline.json");
        std::fs::write(&from, serde_json::to_string(&baseline).unwrap()).unwrap();
        let result = seed_cache(&root, &from, &root.join("out.json"));
        // Original (||): text contains "/home/" -> error. Mutant (&&):
        // text lacks "/root/" -> no error -> test fails, killing mutant.
        assert!(result.is_err(), "seed with /home/ path must be rejected");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_cache_rejects_count_mismatch() {
        // Kills: replace != with == in seed_cache (count validation).
        let root = fake_root("seed-count");
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        );
        write(
            &root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(&root.join("Cargo.lock"), "# fake lock\n");
        // Baseline claims 10 mutants but has only 1.
        let baseline = serde_json::json!({
            "outcomes": [
                {"scenario": "Baseline", "summary": "Success"},
                {
                    "scenario": {"Mutant": {"name": "m1", "package": "a", "file": "crates/a/src/lib.rs"}},
                    "summary": "CaughtMutant"
                }
            ],
            "total_mutants": 10,
            "caught": 1,
            "missed": 0,
            "timeout": 0,
            "unviable": 0
        });
        let from = root.join("baseline.json");
        std::fs::write(&from, serde_json::to_string(&baseline).unwrap()).unwrap();
        // Create the file so it exists.
        write(&root.join("crates/a/src/lib.rs"), "pub fn f() {}\n");
        let result = seed_cache(&root, &from, &root.join("out.json"));
        assert!(result.is_err(), "count mismatch must be rejected");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_report_uses_correct_verdict_for_name() {
        // Kills: replace == with != in merge_report (verdict lookup).
        let root = fake_root("merge-verdict");
        std::fs::create_dir_all(root.join("mutants.out")).unwrap();
        let discovered = vec![
            Discovered {
                name: "mut_a".to_owned(),
                package: "p".to_owned(),
                file: "f.rs".to_owned(),
            },
            Discovered {
                name: "mut_b".to_owned(),
                package: "p".to_owned(),
                file: "f.rs".to_owned(),
            },
        ];
        let mut hits = BTreeMap::new();
        hits.insert(
            "f.rs".to_owned(),
            vec![
                ("mut_a".to_owned(), "CaughtMutant".to_owned()),
                ("mut_b".to_owned(), "Unviable".to_owned()),
            ],
        );
        let fresh = BTreeMap::new();
        merge_report(&root, &discovered, &hits, &fresh, None).unwrap();
        let text = std::fs::read_to_string(root.join("mutants.out").join("outcomes.json")).unwrap();
        let report: Value = serde_json::from_str(&text).unwrap();
        let outcomes = report.get("outcomes").and_then(Value::as_array).unwrap();
        // Find the records for each mutant and verify verdicts.
        let mut found_a = false;
        let mut found_b = false;
        for o in outcomes {
            if let Some(name) = o.pointer("/scenario/Mutant/name").and_then(Value::as_str) {
                let summary = o.get("summary").and_then(Value::as_str).unwrap();
                if name == "mut_a" {
                    assert_eq!(summary, "CaughtMutant");
                    found_a = true;
                } else if name == "mut_b" {
                    assert_eq!(summary, "Unviable");
                    found_b = true;
                }
            }
        }
        assert!(found_a && found_b, "both mutants must be in report");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_report_success_reflects_missed() {
        // Kills: replace == with != in merge_report (success flag).
        let root = fake_root("merge-success");
        std::fs::create_dir_all(root.join("mutants.out")).unwrap();
        let discovered = vec![Discovered {
            name: "mut_a".to_owned(),
            package: "p".to_owned(),
            file: "f.rs".to_owned(),
        }];
        let mut hits = BTreeMap::new();
        // With a missed mutant, success must be false.
        hits.insert(
            "f.rs".to_owned(),
            vec![("mut_a".to_owned(), "MissedMutant".to_owned())],
        );
        let fresh = BTreeMap::new();
        merge_report(&root, &discovered, &hits, &fresh, None).unwrap();
        let text = std::fs::read_to_string(root.join("mutants.out").join("outcomes.json")).unwrap();
        let report: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            report.get("success").and_then(Value::as_bool),
            Some(false),
            "missed mutant must make success false"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_discovered_rejects_duplicates() {
        // Kills: delete ! in parse_discovered (duplicate check).
        let json = r#"[
            {"name": "m1", "package": "p", "file": "f.rs"},
            {"name": "m1", "package": "p", "file": "f.rs"}
        ]"#;
        let result = parse_discovered(json);
        assert!(result.is_err(), "duplicate mutant names must be rejected");
    }

    #[test]
    fn parse_discovered_accepts_unique() {
        // Verifies the happy path for parse_discovered.
        let json = r#"[
            {"name": "m1", "package": "p", "file": "f.rs"},
            {"name": "m2", "package": "p", "file": "f.rs"}
        ]"#;
        let result = parse_discovered(json).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn import_seed_skips_stale_key() {
        // Kills: replace != with == in import_seed (key mismatch check).
        // If the recomputed key differs from the seed's key, the file
        // must be skipped (not imported). The mutant would import it.
        let root = fake_root("seed-stale");
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        );
        write(
            &root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(&root.join("Cargo.lock"), "# fake lock\n");
        write(&root.join("crates/a/src/lib.rs"), "pub fn f() {}\n");
        // Create a seed with a deliberately wrong key for the file.
        // Kills: ==/!= inversion in import_seed's key comparison.
        let fake_key = serde_json::json!({
            "file_hash": "0000000000000000000000000000000000000000000000000000000000000000",
            "suite_hash": "0000000000000000000000000000000000000000000000000000000000000000",
            "lock_hash": "0000000000000000000000000000000000000000000000000000000000000000",
            "toolchain_id": "fake",
            "env_id": "1",
            "config_hash": "0000000000000000000000000000000000000000000000000000000000000000"
        });
        let seed = serde_json::json!({
            "version": 1,
            "summary": {"missed": 0, "total_mutants": 1},
            "files": {
                "crates/a/src/lib.rs": {
                    "package": "a",
                    "key": fake_key,
                    "mutants": {"m1": "CaughtMutant"}
                }
            }
        });
        let path = root.join("seed.json");
        std::fs::write(&path, serde_json::to_string(&seed).unwrap()).unwrap();
        let cache = import_seed(&root, &path).unwrap();
        // The stale file must NOT be in the cache.
        assert!(
            !cache.files.contains_key("crates/a/src/lib.rs"),
            "stale key file must be skipped"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Serializes `HATCHLING_CARGO` manipulation: the variable is
    /// process-global, so tests that override it must not interleave
    /// set/run/restore with each other.
    /// Serializes fake-cargo tests: the override is process-global, so
    /// tests that set it must not interleave set/run/clear with each
    /// other.
    static CARGO_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard: clears the cargo override on drop, even if the test
    /// panics, so a failing test cannot leak the override into another.
    struct CargoOverrideGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for CargoOverrideGuard {
        fn drop(&mut self) {
            set_cargo_override(None);
        }
    }

    /// Run `f` with the cargo binary overridden to `script`, clearing
    /// the override afterwards (even on panic).
    fn with_fake_cargo<R>(script: &Path, f: impl FnOnce() -> R) -> R {
        let _guard = CargoOverrideGuard {
            // Recover from a poisoned lock: a panicked test already
            // cleared the override via the guard's Drop.
            _lock: CARGO_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        };
        set_cargo_override(Some(script.to_path_buf()));
        f()
    }

    /// Minimal workspace (package `a`, one source file) plus a
    /// fake-cargo script. The script answers `mutants --list --json`
    /// with `discovery`, answers any other `mutants` invocation by
    /// writing `outcomes` to `mutants.out/outcomes.json`, and treats
    /// `build` as a no-op success. Runs in milliseconds; no nested
    /// cargo-mutants.
    fn fake_cargo_workspace(name: &str, discovery: &Value, outcomes: &Value) -> PathBuf {
        let root = fake_root(name);
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        );
        write(
            &root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(&root.join("Cargo.lock"), "# fake lock\n");
        write(&root.join("crates/a/src/lib.rs"), "pub fn f() {}\n");
        // Minimal seed: `load_cache` falls back to the checked-in seed
        // path when no cache exists. An empty files map means no hits,
        // so everything is tested fresh.
        write(
            &root.join("crates/hatchling-dev/seeds/wo013-official.json"),
            r#"{"version": 1, "summary": {"total_mutants": 0, "caught": 0, "missed": 0, "timeout": 0, "unviable": 0}, "files": {}}"#,
        );
        write(
            &root.join("fake-discovery.json"),
            &serde_json::to_string(discovery).unwrap(),
        );
        write(
            &root.join("fake-outcomes.json"),
            &serde_json::to_string(outcomes).unwrap(),
        );
        let script = root.join("fake-cargo.sh");
        write(
            &script,
            "#!/bin/sh\n\
             # Fake cargo for the end-to-end cache test. Runs with the\n\
             # workspace root as cwd, so relative paths resolve there.\n\
             case \"$*\" in\n\
             *--list*) cat fake-discovery.json ;;\n\
             *build*) exit 0 ;;\n\
             *) mkdir -p mutants.out; cat fake-outcomes.json > mutants.out/outcomes.json ;;\n\
             esac\n\
             exit 0\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        root
    }

    fn fake_discovery() -> Value {
        serde_json::json!([
            {"name": "fake-m1", "package": "a", "file": "crates/a/src/lib.rs"},
            {"name": "fake-m2", "package": "a", "file": "crates/a/src/lib.rs"},
        ])
    }

    fn fake_outcomes(m2_verdict: &str) -> Value {
        serde_json::json!({
            "outcomes": [
                {"scenario": "Baseline", "summary": "Success"},
                {"scenario": {"Mutant": {"name": "fake-m1", "package": "a", "file": "crates/a/src/lib.rs"}}, "summary": "CaughtMutant"},
                {"scenario": {"Mutant": {"name": "fake-m2", "package": "a", "file": "crates/a/src/lib.rs"}}, "summary": m2_verdict},
            ],
            "total_mutants": 2,
            "caught": 1,
            "missed": 1,
            "timeout": 0,
            "unviable": 0,
        })
    }

    #[test]
    fn run_with_miss_reports_finding_through_fake_cargo() {
        // Kills: discover/run_misses/run whole-function replacements and
        // the !/==/!= inversions in their control flow. Fake cargo
        // reports one caught and one missed mutant; the floor must fail
        // with a finding naming the miss, and a dirty run must not
        // populate the cache.
        let root = fake_cargo_workspace(
            "e2e-miss",
            &fake_discovery(),
            &fake_outcomes("MissedMutant"),
        );
        let script = root.join("fake-cargo.sh");
        let findings = with_fake_cargo(&script, || run(&root).unwrap());
        // A missed mutant breaks the 1.0 floor: at least the floor
        // finding and the per-mutant finding, one naming the miss.
        assert!(
            !findings.is_empty(),
            "a missed mutant must produce findings"
        );
        assert!(
            findings.iter().any(|f| f.message.contains("fake-m2")),
            "a finding must name the missed mutant: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
        // The merged report covers the discovered set exactly, baseline
        // first, and the baseline is the real one, not synthetic.
        let merged: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("mutants.out").join("outcomes.json")).unwrap(),
        )
        .unwrap();
        let scenarios = merged
            .get("outcomes")
            .and_then(Value::as_array)
            .expect("merged report must have an outcomes array");
        let first = scenarios.first().expect("merged report must not be empty");
        assert_eq!(
            first.get("scenario"),
            Some(&Value::String(String::from("Baseline")))
        );
        assert_eq!(first.get("synthetic"), None);
        let names: Vec<&str> = scenarios
            .iter()
            .filter_map(|o| o.pointer("/scenario/Mutant/name").and_then(Value::as_str))
            .collect();
        assert_eq!(names, vec!["fake-m1", "fake-m2"]);
        assert!(
            !root.join(".mutation-cache").join("cache.json").exists(),
            "dirty run must not write the cache"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn run_with_all_caught_updates_cache_through_fake_cargo() {
        // Kills: the ==/!= inversions in run's cache-update path (find
        // and filter on the miss file). All mutants caught -> no
        // findings -> the cache is populated and must contain exactly
        // the tested mutants with their verdicts.
        let root = fake_cargo_workspace(
            "e2e-clean",
            &fake_discovery(),
            &fake_outcomes("CaughtMutant"),
        );
        let script = root.join("fake-cargo.sh");
        let findings = with_fake_cargo(&script, || run(&root).unwrap());
        assert!(findings.is_empty(), "clean run must produce no findings");
        let cache: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".mutation-cache").join("cache.json")).unwrap(),
        )
        .unwrap();
        let mutants = cache
            .pointer("/files/crates~1a~1src~1lib.rs/mutants")
            .expect("cache must have the miss file entry");
        // Note: JSON pointer `~1` escapes `/` in the file path.
        assert_eq!(
            mutants.get("fake-m1"),
            Some(&Value::String(String::from("CaughtMutant")))
        );
        assert_eq!(
            mutants.get("fake-m2"),
            Some(&Value::String(String::from("CaughtMutant")))
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
