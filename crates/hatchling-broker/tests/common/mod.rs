//! Shared helpers for the hatchling-broker integration tests that
//! spawn real cells: unique temp dirs, broker configs, binary lookup,
//! cell configs, and the cell-pipeline availability guard.
//!
//! Every test that spawns a cell binds its own broker socket and runs
//! the real `spawn_cell` pipeline (user, mount, and pid namespaces,
//! minimal read-only root, seccomp). Tests that cannot build a cell in
//! the current environment skip honestly via
//! [`guard_cell_available`]; `HATCHLING_REQUIRE_CELL=1` turns the skip
//! into a hard failure for mutation runs.
//!
//! The helpers are `pub(crate)` because each integration-test target
//! includes this file as a private `mod common`; the parent test
//! module is the only consumer. The `redundant_pub_crate` allow lives
//! on the `mod common;` declaration in each test target.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use hatchling_broker::{CellConfig, Config, EgressConfig, VaultConfig};
use hatchling_core::{LogLevel, PeerUid, SocketPath};

/// A temp dir unique to this test invocation, removed when the test
/// ends, pass or fail.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn test_dir(name: &str) -> TestDir {
    TestDir(test_path(name))
}

/// The path [`test_dir`] guards, without the guard: for a path a
/// config names but no test creates.
pub(crate) fn test_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hatchling-cell-test-{}-{name}", std::process::id()))
}

/// A test's scratch directory, or a socket file, removed on drop so a
/// failing assertion or an early return cannot leak it.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) struct TestDir(PathBuf);

impl std::ops::Deref for TestDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl TestDir {
    /// Guards `path`, a directory or (for a socket under `/tmp`) a file
    /// . What is there already is the caller's to clear.
    #[allow(
        dead_code,
        reason = "shared test helper: used by only some test targets"
    )]
    pub(crate) const fn guard(path: PathBuf) -> Self {
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if std::fs::remove_dir_all(&self.0).is_err() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// A broker [`Config`] serving `socket`, authorizing `agent_uid`, with
/// an empty vault and egress grant set (ungranted ops are denied).
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn broker_config(socket: &Path, agent_uid: u32) -> Result<Config, String> {
    let socket: SocketPath = socket
        .to_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| String::from("test socket path is not UTF-8 or does not parse"))?;
    let vault_root = test_path("vault");
    Ok(Config {
        socket,
        agent_uid: PeerUid::from_kernel(agent_uid),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: hatchling_broker::SchedulerConfig::disabled(),
        log_level: LogLevel::Error,
        vault: VaultConfig {
            key_file: VaultConfig::default_key_file(&vault_root),
            root: vault_root,
            grants: Vec::new(),
        },
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::default(),
            grants: Vec::new(),
        },
        channels: Vec::new(),
        channel_accounts: Vec::new(),
        channel_ingress: None,
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: hatchling_broker::MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        web: hatchling_broker::WebConfig::off(),
        goals: hatchling_broker::GoalsConfig::disabled(),
    })
}

/// Locate a compiled binary next to the test executable
/// (`target/debug/deps/` -> `target/debug/`), falling back to the
/// named environment variable. The workspace must be built before
/// running these tests (`cargo build --workspace` in
/// scripts/check.sh).
///
/// Lookup order matters for mutation testing. When cargo-mutants
/// mutates the binary's crate, the scratch tree's build puts the
/// mutated binary next to the test executable, and the tests must
/// spawn that one — preferring the harness-provided variable (the
/// unmutated source-tree binary) would mask mutants. When other
/// packages are mutated the scratch tree never builds the binary, so
/// the variable remains the fallback there.
fn find_binary(name: &str, env_var: &str) -> Result<PathBuf, String> {
    let test_exe = std::env::current_exe().map_err(|e| format!("test executable path: {e}"))?;
    let deps = test_exe
        .parent()
        .ok_or_else(|| String::from("test executable has no parent"))?;
    let target_debug = deps
        .parent()
        .ok_or_else(|| String::from("test deps dir has no parent"))?;
    let next_to_exe = target_debug.join(name);
    if next_to_exe.is_file() {
        return Ok(next_to_exe);
    }
    if let Ok(path) = std::env::var(env_var) {
        let bin = PathBuf::from(path);
        if bin.is_file() {
            return Ok(bin);
        }
    }
    Err(format!(
        "{name} binary not found at {}: run `cargo build --workspace` first",
        next_to_exe.display()
    ))
}

/// Path to the compiled `hatchling-agent` binary, which performs the
/// genuine handshake and exits 0.
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
pub(crate) fn agent_binary() -> Result<PathBuf, String> {
    find_binary("hatchling-agent", "HATCHLING_AGENT_BIN")
}

/// Path to the compiled `hatchling-probe` binary (the
/// adversarial probe).
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
pub(crate) fn probe_binary() -> Result<PathBuf, String> {
    find_binary("hatchling-probe", "HATCHLING_PROBE_BIN")
}

/// The host uid the cell's user maps to: what the broker's peer gate
/// sees. Mirrors the spawn's id-map choice — a root broker maps
/// in-cell 0 to an unprivileged uid; an unprivileged broker can only
/// map its own uid.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn mapped_agent_uid() -> u32 {
    if hatchling_sys::euid() == 0 {
        65534
    } else {
        hatchling_sys::euid()
    }
}

/// Builds a [`CellConfig`] for an arbitrary binary and cell id.
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
pub(crate) fn cell_config_for(
    binary: &Path,
    cell_id: &str,
    socket: &Path,
    agent_uid: u32,
) -> Result<CellConfig, String> {
    let socket_dir = socket
        .parent()
        .ok_or_else(|| String::from("socket has no parent"))?
        .to_path_buf();
    CellConfig::parse_test(binary, cell_id, &socket_dir, agent_uid)
        .map_err(|e| format!("test cell config: {e}"))
}

/// Builds a [`CellConfig`] for the real agent binary: the compiled
/// `hatchling-agent` test binary, which performs the genuine
/// handshake and exits 0.
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
pub(crate) fn cell_config(socket: &Path, agent_uid: u32) -> Result<CellConfig, String> {
    let agent_bin = agent_binary()?;
    cell_config_for(&agent_bin, "boundary-test", socket, agent_uid)
}

/// Staging dirs left behind by guard probe invocations (in-flight or
/// leaked): anything starting with `hatchling-probe-` in the temp dir.
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
pub(crate) fn probe_staging_dirs() -> std::collections::HashSet<PathBuf> {
    std::env::temp_dir()
        .read_dir()
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("hatchling-probe-"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Can this environment build a cell? Without it the boundary tests
/// cannot run; skip honestly.
///
/// Probing namespace *creation* alone is not enough: some environments
/// allow `unshare(user)`/`unshare(mount)` but block the mount half of
/// the spawn pipeline (id-map writes, tmpfs mounts, `pivot_root`). The
/// probe therefore runs the real pipeline the spawn uses — the id maps
/// and a bind-less `MountPlan::apply` — in a forked child, so the
/// parent's mount table is untouched. If any step fails, the tests skip
/// instead of failing halfway through a doomed spawn.
///
/// Each probe invocation gets a unique staging dir: parallel boundary
/// tests share the process (and its pid), so a fixed
/// `hatchling-probe-{pid}` name collides between them, the guard fails,
/// and the tests silently skip. Thread id plus an atomic counter makes
/// every invocation's dir unique.
pub(crate) fn userns_available() -> bool {
    // In a nested user namespace (container), tmpfs with non-identity
    // uid mappings hits a kernel EOVERFLOW limitation on mkdir. The
    // full cell spawn requires a non-nested userns.
    if let Ok(map) = std::fs::read_to_string("/proc/self/uid_map") {
        let trimmed = map.trim();
        // Initial userns has a huge range; nested has a smaller one.
        // Heuristic: if the map doesn't start with "0 0" (identity),
        // we're nested.
        if !trimmed.starts_with("0          0") && !trimmed.starts_with("0 0 ") {
            return false;
        }
    }
    let stage = probe_stage();
    // Best-effort: a stale dir from a killed earlier run must not fail
    // the probe (a reused pid colliding with it is near-impossible,
    // but the cleanup is free).
    let _ = std::fs::remove_dir(&stage);
    match hatchling_sys::fork() {
        Ok(hatchling_sys::ForkResult::Parent { child }) => {
            let ok = matches!(
                child.wait_deadline(Duration::from_secs(120)),
                Ok(status) if hatchling_sys::child_exit_code(status) == 0
            );
            // The child's mounts lived in its own namespace; here the
            // staging dir is a plain empty dir. Remove it so probes
            // never accumulate debris in the temp dir.
            let _ = std::fs::remove_dir(&stage);
            ok
        }
        Ok(hatchling_sys::ForkResult::Child) => {
            let ok = probe_spawn_pipeline(&stage);
            hatchling_sys::exit_child(i32::from(!ok));
        }
        Err(_) => false,
    }
}

static PROBE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A unique staging dir for guard probe invocations.
pub(crate) fn probe_stage() -> PathBuf {
    std::env::temp_dir().join(format!(
        "hatchling-probe-{}-{:?}-{}",
        std::process::id(),
        std::thread::current().id(),
        PROBE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// Runs the environment-sensitive half of the cell spawn in the probe
/// child: unshare the user, mount, and pid namespaces, write the id
/// maps the real spawn uses, then apply a bind-less `MountPlan`
/// (fresh tmpfs root, `pivot_root`, detach old root) — the exact code
/// path `spawn_cell` takes through `child_main`.
///
/// The split is the spawn's too: a namespace child unshares its
/// user namespace and this side, still outside it, writes the maps. From
/// inside, a process can map only its own uid, so a root broker's map
/// (in-cell 0 to 65534) would be refused there.
///
/// Returns `true` only if every step succeeded. Everything the child
/// mounts is confined to its mount namespace, which dies with it; the
/// parent sees only the empty staging dir. The plan ends as the cell's
/// user, as the spawn's does;
/// seccomp, and exec need no further environment capability beyond
/// what the maps grant, so they are not probed.
///
/// Test helper: runs the pipeline directly (in a forked child — never
/// in the multi-threaded test process) to assert the guard's probe
/// tracks the real spawn path.
pub(crate) fn probe_spawn_pipeline(stage: &Path) -> bool {
    // Mirror the real spawn's map choice: a root broker maps in-cell 0
    // to an unprivileged uid; an unprivileged broker can only map its
    // own uid (the only map the kernel accepts from it).
    let broker_euid = hatchling_sys::euid();
    let base = if broker_euid == 0 { 65534 } else { broker_euid };
    let Ok(map) = hatchling_sys::IdMap::new(base, 1) else {
        return false;
    };
    // Its gid: the same number for a root broker, its own gid otherwise.
    let group = if broker_euid == 0 {
        base
    } else {
        hatchling_sys::egid()
    };
    let Ok(group_map) = hatchling_sys::IdMap::new(group, 1) else {
        return false;
    };
    if map.validate_for_euid(broker_euid).is_err() {
        return false;
    }
    let Ok((mut ours, mut theirs)) = std::os::unix::net::UnixStream::pair() else {
        return false;
    };
    match hatchling_sys::fork() {
        Ok(hatchling_sys::ForkResult::Parent { child }) => {
            drop(theirs);
            // Writing the maps is the step restrictive environments most
            // often block.
            let mut byte = [0u8; 1];
            let mapped = ours.read_exact(&mut byte).is_ok()
                && byte == [1]
                && hatchling_sys::idmap::write_maps(child.pid().get(), &map, &group_map).is_ok();
            let _ = ours.write_all(&[u8::from(mapped)]);
            drop(ours);
            let exited = matches!(
                child.wait_deadline(Duration::from_secs(120)),
                Ok(status) if hatchling_sys::child_exit_code(status) == 0
            );
            mapped && exited
        }
        Ok(hatchling_sys::ForkResult::Child) => {
            drop(ours);
            let mut byte = [0u8; 1];
            let ok = hatchling_sys::unshare_user().is_ok()
                && theirs.write_all(&[1]).is_ok()
                && theirs.read_exact(&mut byte).is_ok()
                && byte == [1]
                && probe_mapped_pipeline(stage);
            hatchling_sys::exit_child(i32::from(!ok));
        }
        Err(_) => false,
    }
}

/// The probe's namespace child, once its maps are written: the mount,
/// network, IPC and pid steps of the spawn, and its mount plan.
fn probe_mapped_pipeline(stage: &Path) -> bool {
    if hatchling_sys::unshare_mount().is_err() {
        return false;
    }
    // The spawn's network and IPC namespaces: an environment that
    // allows a user namespace but not these must skip, not fail midway.
    if hatchling_sys::unshare_net().is_err() || hatchling_sys::unshare_ipc().is_err() {
        return false;
    }
    if std::fs::create_dir(stage).is_err() {
        return false;
    }
    let Ok(plan) = hatchling_sys::MountPlan::new(
        stage.to_path_buf(),
        &std::collections::HashSet::new(),
        Vec::new(),
    ) else {
        return false;
    };
    if plan.apply().is_err() {
        return false;
    }
    hatchling_sys::unshare_pid().is_ok()
}

/// Opt-in strict mode for mutation runs: when `HATCHLING_REQUIRE_CELL=1`,
/// an unavailable cell pipeline is a hard failure instead of an honest
/// skip. Without it, a mutant that breaks the probe (anything in the
/// `MountPlan::apply` path the probe exercises) silently disables every
/// boundary test, and the mutant is missed. Normal runs keep the honest
/// skip; mutation runs on a known-good environment set the variable.
pub(crate) fn require_cell() -> bool {
    std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1")
}

/// The cell-test guard: `Ok(true)` when the cell pipeline is
/// available, `Ok(false)` for an honest skip — or a hard failure when
/// `HATCHLING_REQUIRE_CELL=1` refuses the skip.
///
/// The probe exercises the real pipeline: where the guard passes, a
/// direct probe run must succeed too. A regression here (wrong map
/// base, a plan change the probe didn't track) would otherwise surface
/// only as mysteriously skipped tests.
pub(crate) fn guard_cell_available() -> Result<bool, String> {
    if userns_available() {
        return Ok(true);
    }
    if require_cell() {
        return Err(String::from(
            "HATCHLING_REQUIRE_CELL=1 but the cell spawn pipeline is unavailable: \
             refusing to skip (a broken probe would hide mutants)",
        ));
    }
    // The SKIP notice goes to stdout so it is visible in captured
    // test output. (`println!` is denied workspace-wide, so this
    // writes via `io::Write` directly.)
    let _ = writeln!(
        std::io::stdout(),
        "SKIP: cell spawn pipeline unavailable in this environment"
    );
    Ok(false)
}

/// Path to the compiled `hatchling-skill-fixture` binary: a real
/// executable skill whose input picks its behavior (see its module docs).
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn skill_fixture_binary() -> Result<PathBuf, String> {
    find_binary("hatchling-skill-fixture", "HATCHLING_SKILL_FIXTURE_BIN")
}

/// Path to the compiled `hatchling-run-fixture` binary: the program the
/// scheduled-run tests start in place of the agent (-2).
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
pub(crate) fn run_fixture_binary() -> Result<PathBuf, String> {
    find_binary("hatchling-run-fixture", "HATCHLING_RUN_FIXTURE_BIN")
}

/// Path to the compiled `hatchling-harness-fixture` binary: the stub
/// coding harness the tests configure as Claude Code and the
/// Codex CLI (see its module docs).
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
pub(crate) fn harness_fixture_binary() -> Result<PathBuf, String> {
    find_binary("hatchling-harness-fixture", "HATCHLING_HARNESS_FIXTURE_BIN")
}

/// The id the fixture skill registers under.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) const FIXTURE_SKILL: &str = "fixture@1.0.0";

/// The capability the fixture skill declares (and is granted).
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) const FIXTURE_CAPABILITY: &str = "text.transform";

/// Writes a signed package for the fixture skill under
/// `<root>/skills/fixture` (the fixture binary as its entry point) and
/// returns the skill config that grants it, under a fixed test trust
/// root.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn fixture_skill_config(root: &Path) -> Result<hatchling_broker::SkillConfig, String> {
    use ed25519_dalek::{Signer, SigningKey};
    let signing = SigningKey::from_bytes(&[0x2c; 32]);
    let capability: hatchling_core::CapabilityName = FIXTURE_CAPABILITY
        .parse()
        .map_err(|e| format!("capability: {e}"))?;
    let manifest = hatchling_skill::Manifest {
        name: String::from("fixture"),
        version: String::from("1.0.0"),
        capabilities: vec![capability.clone()],
        entry_point: String::from("skill"),
    };
    let payload = manifest.canonical_bytes().map_err(|e| e.to_string())?;
    let signed = hatchling_skill::SignedManifest {
        manifest,
        signature: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signing.sign(&payload).to_bytes(),
        )),
    };
    let skills_dir = root.join("skills");
    let package = skills_dir.join("fixture");
    std::fs::create_dir_all(&package).map_err(|e| format!("package dir: {e}"))?;
    let json = signed.to_json().map_err(|e| e.to_string())?;
    std::fs::write(package.join("manifest.json"), json).map_err(|e| format!("manifest: {e}"))?;
    std::fs::copy(skill_fixture_binary()?, package.join("skill"))
        .map_err(|e| format!("entry point: {e}"))?;
    let trust_roots_file = fixture_roots_file(root);
    write_roots_file(&trust_roots_file, &[signing.verifying_key().to_bytes()])?;
    Ok(hatchling_broker::SkillConfig {
        skills_dir,
        trust_roots_file,
        grants: std::iter::once((
            String::from("fixture"),
            std::iter::once(capability).collect(),
        ))
        .collect(),
    })
}

/// The broker executable the tests start spawner helpers with: the
/// compiled `hatchling-broker`, or `HATCHLING_TEST_SPAWNER` when set (CI
/// points it at `hatchling`, to prove the helpers run through the
/// `hatchling-runtime` front door too).
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn spawner() -> PathBuf {
    std::env::var_os("HATCHLING_TEST_SPAWNER").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_hatchling-broker")),
        PathBuf::from,
    )
}

/// The fixture skill loaded exactly as broker startup loads it, with the
/// real runtime: the compiled broker binary as the spawner helper.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn fixture_skills(
    root: &Path,
    timeout: Duration,
) -> Result<hatchling_broker::Skills, String> {
    let config = fixture_skill_config(root)?;
    let runtime = hatchling_broker::SkillRuntime::new(spawner()).with_timeout(timeout);
    let skills = hatchling_broker::Skills::load(&config)
        .map_err(|e| format!("load: {e}"))?
        .with_runtime(runtime);
    if skills.snapshot().registry().len() != 1 {
        return Err(String::from("the fixture skill did not load"));
    }
    Ok(skills)
}

/// The id the fixture builder registers under.
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
pub(crate) const FIXTURE_BUILDER: &str = "builder@1.0.0";

/// The fixed key the fixture skills are signed with.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) const FIXTURE_SIGNING_KEY: [u8; 32] = [0x2c; 32];

/// Writes one signed skill package, `<skills_dir>/<name>`, declaring
/// `capability`, with `binary` as its entry point.
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
fn write_skill_package(
    skills_dir: &Path,
    name: &str,
    capability: &hatchling_core::CapabilityName,
    binary: &Path,
) -> Result<(), String> {
    use ed25519_dalek::{Signer, SigningKey};
    let signing = SigningKey::from_bytes(&FIXTURE_SIGNING_KEY);
    let manifest = hatchling_skill::Manifest {
        name: name.to_owned(),
        version: String::from("1.0.0"),
        capabilities: vec![capability.clone()],
        entry_point: String::from("skill"),
    };
    let payload = manifest.canonical_bytes().map_err(|e| e.to_string())?;
    let signed = hatchling_skill::SignedManifest {
        manifest,
        signature: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signing.sign(&payload).to_bytes(),
        )),
    };
    let package = skills_dir.join(name);
    std::fs::create_dir_all(&package).map_err(|e| format!("package dir: {e}"))?;
    let json = signed.to_json().map_err(|e| e.to_string())?;
    std::fs::write(package.join("manifest.json"), json).map_err(|e| format!("manifest: {e}"))?;
    std::fs::copy(binary, package.join("skill")).map_err(|e| format!("entry point: {e}"))?;
    Ok(())
}

/// The id the real document builder registers under.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) const DOCUMENT_BUILDER: &str = "document@1.0.0";

/// The id the real page builder registers under.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) const PAGE_BUILDER: &str = "page@1.0.0";

/// The skills an artifact test runs, loaded as broker
/// startup loads them, with the real runtime: the fixture builder
/// ([`FIXTURE_BUILDER`]), the real document and page builders
/// ([`DOCUMENT_BUILDER`], [`PAGE_BUILDER`]), all three declaring
/// `artifact.build`, and the plain fixture skill ([`FIXTURE_SKILL`],
/// declaring only `text.transform`).
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn artifact_skills(
    root: &Path,
    timeout: Duration,
) -> Result<hatchling_broker::Skills, String> {
    let parse = |raw: &str| -> Result<hatchling_core::CapabilityName, String> {
        raw.parse().map_err(|e| format!("capability: {e}"))
    };
    let build = parse(hatchling_artifact::ARTIFACT_BUILD_CAPABILITY)?;
    let transform = parse(FIXTURE_CAPABILITY)?;
    let skills_dir = root.join("skills");
    let builders = [
        ("builder", builder_fixture_binary()?),
        (
            "document",
            find_binary("hatchling-build-document", "HATCHLING_BUILD_DOCUMENT_BIN")?,
        ),
        (
            "page",
            find_binary("hatchling-build-page", "HATCHLING_BUILD_PAGE_BIN")?,
        ),
    ];
    for (name, binary) in &builders {
        write_skill_package(&skills_dir, name, &build, binary)?;
    }
    write_skill_package(&skills_dir, "fixture", &transform, &skill_fixture_binary()?)?;
    let trust_roots_file = fixture_roots_file(root);
    let verifying = ed25519_dalek::SigningKey::from_bytes(&FIXTURE_SIGNING_KEY)
        .verifying_key()
        .to_bytes();
    write_roots_file(&trust_roots_file, &[verifying])?;
    let mut grants: std::collections::HashMap<String, std::collections::HashSet<_>> = builders
        .iter()
        .map(|(name, _)| ((*name).to_owned(), std::iter::once(build.clone()).collect()))
        .collect();
    grants.insert(
        String::from("fixture"),
        std::iter::once(transform).collect(),
    );
    let config = hatchling_broker::SkillConfig {
        skills_dir,
        trust_roots_file,
        grants,
    };
    let runtime = hatchling_broker::SkillRuntime::new(spawner()).with_timeout(timeout);
    let skills = hatchling_broker::Skills::load(&config)
        .map_err(|e| format!("load: {e}"))?
        .with_runtime(runtime);
    if skills.snapshot().registry().len() != 4 {
        return Err(String::from("the artifact skills did not all load"));
    }
    Ok(skills)
}

/// Path to the compiled `hatchling-builder-fixture` binary.
#[allow(dead_code, reason = "shared test helper: used by only one test target")]
pub(crate) fn builder_fixture_binary() -> Result<PathBuf, String> {
    find_binary("hatchling-builder-fixture", "HATCHLING_BUILDER_FIXTURE_BIN")
}

/// Where [`fixture_skill_config`] writes the fixture's trust-roots file.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn fixture_roots_file(root: &Path) -> PathBuf {
    root.join("trust-roots")
}

/// Writes a v1 trust-roots file for `keys` (primary first), owner-only.
#[allow(
    dead_code,
    reason = "shared test helper: used by only some test targets"
)]
pub(crate) fn write_roots_file(path: &Path, keys: &[[u8; 32]]) -> Result<(), String> {
    use std::fmt::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let mut text = format!("{}\n", hatchling_broker::trust_roots::TRUST_ROOTS_HEADER);
    for (role, key) in ["primary", "previous"].iter().zip(keys) {
        let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key);
        writeln!(text, "{role} {key}").map_err(|e| format!("roots file: {e}"))?;
    }
    text.push_str("end\n");
    std::fs::write(path, text).map_err(|e| format!("roots file: {e}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("roots file mode: {e}"))
}

/// Replays real helper lifecycle cases in a fresh process with a private
/// delegation, without changing the parallel test process's environment.
#[allow(dead_code, reason = "shared by the skill and scheduled-run suites")]
pub(crate) fn cgroup_lifecycle_cases(tag: &str, cases: &[&str]) -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let Some(base) = std::env::var_os("HATCHLING_TEST_CGROUP") else {
        if std::env::var("HATCHLING_REQUIRE_CGROUP").is_ok_and(|v| v == "1") {
            return Err(String::from("HATCHLING_REQUIRE_CGROUP=1 but no delegation"));
        }
        let _ = writeln!(std::io::stderr(), "skipping helper cgroups: no delegation");
        return Ok(());
    };
    let mut parent = hatchling_sys::cgroup::CellCgroup::create(
        Path::new(&base),
        hatchling_sys::cgroup::CgroupLimits::default(),
    )
    .map_err(|e| e.to_string())?;
    for case in cases {
        let output =
            std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                .args(["--exact", case, "--nocapture"])
                .env("HATCHLING_CELL_CGROUP", parent.path())
                .env("HATCHLING_REQUIRE_CELL", "1")
                .output()
                .map_err(|e| e.to_string())?;
        if !output.status.success() || !String::from_utf8_lossy(&output.stdout).contains("1 passed")
        {
            return Err(format!(
                "{tag} case {case}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let leaves = std::fs::read_dir(parent.path())
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .count();
        if leaves != 0 {
            return Err(format!(
                "{tag} case {case} stranded {leaves} cgroup directories"
            ));
        }
    }
    parent.finish().map_err(|e| e.to_string())
}
