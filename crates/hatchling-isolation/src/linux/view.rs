//! A cell's view on Linux, and the mount plan built from it.
//!
//! The view is resolved in the order the broker's cell has always
//! resolved it: the program, its shared libraries (`ldd`), the endpoint
//! directory, then each granted directory. The mount plan binds it in
//! the view's order, so the cell holds exactly what
//! [`view`] reports: the plan is built from nothing else.

use std::collections::HashSet;
use std::io::Read as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use hatchling_sys::{BindMount, Child, ChildPid, MountPlan};

use crate::{CellSpec, CellView, ENDPOINT_DIR, Error, Origin, PROGRAM, PlatformError, ViewEntry};

/// The devices a CLI cell writes, at their own paths.
pub(super) const DEVICES: [&str; 3] = ["/dev/null", "/dev/zero", "/dev/urandom"];

/// Bound for `ldd` while building the view: it only reads ELF headers,
/// so anything past this is a hang.
const LDD_TIMEOUT: Duration = Duration::from_secs(30);

/// Everything a cell built from `spec` sees: the endpoint directory (if
/// granted), the program, each granted directory, then the program's
/// shared libraries. All read-only but the granted writable directories.
///
/// # Errors
///
/// Returns [`Error::Spawn`] if a path does not exist or `ldd` fails.
pub(super) fn view(spec: &CellSpec) -> Result<CellView, Error> {
    let program = canonicalize(&spec.program, "agent binary")?;
    let mut libs = shared_libraries(&program)?;
    let mut entries = Vec::new();
    // The broker endpoint's directory: only for a spec that grants one.
    if let Some(endpoint) = &spec.endpoint {
        entries.push(ViewEntry {
            source: canonicalize(&endpoint.dir, "broker socket dir")?,
            name: in_root(ENDPOINT_DIR),
            dir: true,
            writable: false,
            origin: Origin::Endpoint,
        });
    }
    entries.push(ViewEntry {
        source: program,
        name: in_root(PROGRAM),
        dir: false,
        writable: false,
        origin: Origin::Program,
    });
    // The granted directories, exactly as the spec grants them: never
    // influenced by anything the cell supplies.
    for exposure in &spec.exposures {
        entries.push(ViewEntry {
            source: canonicalize(&exposure.source, exposure.label)?,
            name: exposure.name.clone(),
            dir: true,
            writable: exposure.writable,
            origin: Origin::Granted,
        });
    }
    // The tools, each at its own path, and what they need to start.
    for tool in &spec.tools {
        let source = canonicalize(&tool.source, "tool")?;
        for lib in shared_libraries(&source)? {
            if !libs.iter().any(|known| known.target == lib.target) {
                libs.push(lib);
            }
        }
        entries.push(ViewEntry {
            source,
            name: tool.name.clone(),
            dir: false,
            writable: false,
            origin: Origin::Tool,
        });
    }
    for lib in libs {
        entries.push(ViewEntry {
            source: lib.source,
            name: lib.target,
            dir: false,
            writable: false,
            origin: Origin::Runtime,
        });
    }
    let cli = spec.filter.runs_processes();
    if cli {
        for device in DEVICES {
            entries.push(ViewEntry {
                source: canonicalize(Path::new(device), "device")?,
                name: in_root(device),
                dir: false,
                writable: true,
                origin: Origin::Device,
            });
        }
    }
    Ok(CellView {
        entries,
        relay: spec.relay,
        processes: cli,
        scratch: spec.scratch,
    })
}

/// A cell-layout path as a name relative to the cell's root.
fn in_root(path: &str) -> String {
    path.trim_start_matches('/').to_owned()
}

/// The validated mount plan for `view`, rooted at `new_root`: every
/// entry's source allowlisted explicitly and bound at its name, in the
/// view's order.
///
/// # Errors
///
/// Returns [`Error::View`] if `hatchling-sys` rejects the plan.
pub(super) fn mount_plan(view: CellView, new_root: &Path) -> Result<MountPlan, Error> {
    let allowlist: HashSet<PathBuf> = view
        .entries
        .iter()
        .map(|entry| entry.source.clone())
        .collect();
    let processes = view.processes;
    let scratch = view.scratch;
    let binds = view
        .entries
        .into_iter()
        .map(|entry| BindMount {
            device: entry.origin == Origin::Device,
            source: entry.source,
            target: entry.name,
            is_dir: entry.dir,
            writable: entry.writable,
        })
        .collect();
    MountPlan::new(new_root.to_path_buf(), &allowlist, binds)
        .map(|plan| if processes { plan.with_proc() } else { plan })
        .and_then(|plan| match scratch {
            Some(bytes) => plan.with_scratch(bytes),
            None => Ok(plan),
        })
        .map_err(|source| Error::View(PlatformError::new(source)))
}

/// Canonicalizes a path, failing closed if it does not exist.
fn canonicalize(path: &Path, what: &str) -> Result<PathBuf, Error> {
    std::fs::canonicalize(path).map_err(|source| Error::Spawn {
        reason: format!("cannot canonicalize {what} {}: {source}", path.display()),
    })
}

/// A shared library the agent binary needs: the canonical host
/// source (allowlisted, what gets bind-mounted) and the
/// loader-visible target (the path the ELF loader will request inside
/// the cell, e.g. `/lib64/ld-linux-x86-64.so.2`).
struct SharedLib {
    /// Canonical host path of the library file.
    source: PathBuf,
    /// In-cell path the loader requests, relative to the cell root
    /// (no leading `/`).
    target: String,
}

/// Where `ldd` is: the first of [`LDD`] that is a file. Never found on
/// `PATH`, which is whoever started the cell's maker's to set.
///
/// # Errors
///
/// `NotFound` when none is.
fn ldd() -> std::io::Result<&'static Path> {
    LDD.iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no ldd at {}", LDD.join(" or ")),
            )
        })
}

/// Where `ldd` may be, in order.
const LDD: [&str; 2] = ["/usr/bin/ldd", "/bin/ldd"];

/// Who `ldd` runs as on `binary`, when this process is root: whoever
/// could change what that path names, the first owner other than root
/// of the binary or a directory above it (WO-017). The dynamic loader
/// then never parses, as root, an ELF that another user wrote, such as
/// the browser in the broker's snapshot: a loader bug would be root's.
/// None, to run as this process, when it isn't root or root owns them
/// all; one that can't be looked at is left to `ldd` to fail on.
fn ldd_identity(binary: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt as _;
    if hatchling_sys::euid() != 0 {
        return None;
    }
    binary
        .ancestors()
        .filter_map(|entry| std::fs::symlink_metadata(entry).ok())
        .find(|meta| meta.uid() != 0)
        .map(|meta| (meta.uid(), meta.gid()))
}

/// Runs `ldd` on the agent binary with a hard timeout: `ldd` only
/// reads ELF headers, so anything past [`LDD_TIMEOUT`] is a hang.
///
/// Stdout/stderr are drained on dedicated threads while the child
/// runs: waiting for exit before reading would wedge `ldd` if it
/// ever filled the pipe buffers (pipe-fill deadlock), and an
/// unbounded `Command::output()` would wedge the broker before the
/// cell even exists. Supervision goes through a pidfd
/// [`Child`]: validated pid, bounded wait, kill-on-drop — no raw-pid
/// signalling, no unbounded wait.
fn ldd_output(agent_bin: &Path) -> Result<std::process::Output, Error> {
    let spawn_err = |source: std::io::Error| Error::Spawn {
        reason: format!("cannot run ldd on {}: {source}", agent_bin.display()),
    };
    let mut command = Command::new(ldd().map_err(spawn_err)?);
    command
        .arg(agent_bin)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((uid, gid)) = ldd_identity(agent_bin) {
        // Root setting another uid also drops every supplementary group.
        std::os::unix::process::CommandExt::uid(&mut command, uid);
        std::os::unix::process::CommandExt::gid(&mut command, gid);
    }
    let mut std_child = command.spawn().map_err(spawn_err)?;
    // Wrap the validated pid in pidfd supervision before anything
    // else: from here on the child is never unsupervised.
    let pid = i32::try_from(std_child.id()).map_err(|_| Error::Spawn {
        reason: format!("ldd child pid {} does not fit in i32", std_child.id()),
    })?;
    let pid = ChildPid::new(pid).ok_or_else(|| Error::Spawn {
        reason: String::from("ldd child pid failed validation"),
    })?;
    let child = match Child::new(pid) {
        Ok(child) => child,
        Err(source) => {
            // Cannot supervise: fail closed. The child is just-spawned
            // and unreaped, so signalling it by pid has no reuse
            // window — kill and reap it rather than leave it
            // unsupervised.
            let _ = std_child.kill();
            let _ = std_child.wait();
            return Err(Error::Spawn {
                reason: format!("cannot supervise ldd via pidfd: {source}"),
            });
        }
    };
    // The std handle is now redundant: drop it. (`std::process::Child`
    // Drop neither kills nor waits; `child` owns supervision.)
    let stdout_drain = std_child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_drain = std_child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            buf
        })
    });
    drop(std_child);
    // Bounded wait: on timeout the child is already SIGKILLed and
    // reaped — fail closed, never a hung broker.
    let Ok(status) = child.wait_deadline(LDD_TIMEOUT) else {
        // Join the drains so no detached thread outlives the call:
        // their pipes are at EOF after the kill, so joining cannot
        // hang.
        for drain in [stdout_drain, stderr_drain].into_iter().flatten() {
            let _ = drain.join();
        }
        return Err(Error::Spawn {
            reason: format!(
                "ldd on {} timed out after {LDD_TIMEOUT:?}",
                agent_bin.display()
            ),
        });
    };
    // The child exited: the drains' pipes are at EOF, so joining
    // cannot hang.
    let stdout = stdout_drain.map_or_else(Vec::new, |handle| handle.join().unwrap_or_default());
    let stderr = stderr_drain.map_or_else(Vec::new, |handle| handle.join().unwrap_or_default());
    Ok(std::process::Output {
        status: std::process::ExitStatus::from_raw(status),
        stdout,
        stderr,
    })
}

/// Resolves the shared libraries an agent binary needs via `ldd`.
///
/// Each returned [`SharedLib`] carries the canonical host source
/// (allowlisted as a read-only bind source) and separately the
/// loader-visible target path. The two differ when the loader path is
/// a symlink: `ldd` prints `/lib64/ld-linux-x86-64.so.2`, but the
/// canonical source may be `/usr/lib64/ld-linux-x86-64.so.2`. The
/// loader requests the *original* path, so the bind mount must appear
/// at the original target — mounting only the canonical path would
/// leave the loader's request unmapped and `exec` would fail.
///
/// A statically linked binary yields an empty set. `ldd` failing is
/// fail-closed: the cell cannot be built without knowing what the
/// loader will map.
fn shared_libraries(agent_bin: &Path) -> Result<Vec<SharedLib>, Error> {
    let output = ldd_output(agent_bin)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "not a dynamic executable": statically linked; no libraries
        // to bind-mount.
        if stderr.contains("not a dynamic executable") {
            return Ok(Vec::new());
        }
        return Err(Error::Spawn {
            reason: format!("ldd failed on {}: {stderr}", agent_bin.display()),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut libs = Vec::new();
    for line in stdout.lines() {
        let path = if let Some((_, rest)) = line.split_once("=>") {
            rest.split_whitespace().next().unwrap_or("")
        } else {
            // The interpreter line: "/lib64/ld-linux-x86-64.so.2 (0x...)".
            line.split_whitespace().next().unwrap_or("")
        };
        // Skip the vdso (not a file), empty entries, and anything not
        // an absolute path. A single `!starts_with('/')` covers all
        // three: neither "" nor "linux-vdso.so.1" starts with '/'.
        // (Written as one clause so there is no redundant `||` for
        // mutants to target.)
        if !path.starts_with('/') {
            continue;
        }
        let source = canonicalize(Path::new(path), "shared library")?;
        let target = match path.strip_prefix('/') {
            Some(relative) => relative.to_owned(),
            None => {
                return Err(Error::Spawn {
                    reason: format!("loader path is not absolute: {path}"),
                });
            }
        };
        if !libs.iter().any(|lib: &SharedLib| lib.source == source) {
            libs.push(SharedLib { source, target });
        }
    }
    Ok(libs)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{Endpoint, Exposure, Filter};

    /// Upper bound for one call that runs on a production deadline
    /// (`LDD_TIMEOUT`). The call returns in milliseconds; a mutant that
    /// makes it hang must fail the test well inside the mutation
    /// runner's per-test timeout.
    const WATCHDOG: Duration = Duration::from_secs(10);

    /// Runs `f` on a helper thread and fails the test if it has not
    /// returned within [`WATCHDOG`]. A hung call is abandoned; its thread
    /// dies with the test process, so the verdict is a fast failure.
    pub(in crate::linux) fn within_watchdog<T: Send + 'static>(
        what: &str,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        let got = rx.recv_timeout(WATCHDOG);
        assert!(
            got.is_ok(),
            "{what} did not return within {WATCHDOG:?}: a bounded wait is hanging"
        );
        got.expect("checked by the assert above")
    }

    /// A scratch directory with `code`, `work`, `scratch` and `sock`
    /// subdirectories, removed when dropped.
    pub(in crate::linux) struct Scratch(pub(in crate::linux) PathBuf);

    impl Scratch {
        pub(in crate::linux) fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hatchling-isolation-view-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            for sub in ["code", "work", "scratch", "sock", "root"] {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
            }
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A skill-like spec running `/bin/true` with its code at `skill`,
    /// and its harness twin with a writable work tree and scratch.
    pub(in crate::linux) fn specs(scratch: &Scratch) -> (CellSpec, CellSpec) {
        let skill = CellSpec {
            identity: crate::IsolationBackend::identity(&crate::linux::Linux, None, 1).unwrap(),
            program: PathBuf::from("/bin/true"),
            argv: vec![String::from(PROGRAM)],
            env: Vec::new(),
            workdir: None,
            endpoint: None,
            connection: false,
            vault_channel: false,
            exposures: vec![Exposure {
                source: scratch.0.join("code"),
                name: String::from("skill"),
                writable: false,
                label: "skill code dir",
            }],
            tools: Vec::new(),
            relay: None,
            filter: Filter::Program,
            limits: crate::Limits::default(),
            cgroup: None,
            scratch: None,
        };
        let mut harness = skill.clone();
        for name in ["work", "scratch"] {
            harness.exposures.push(Exposure {
                source: scratch.0.join(name),
                name: String::from(name),
                writable: true,
                label: "harness directory",
            });
        }
        harness.filter = Filter::WorkTree;
        (skill, harness)
    }

    #[test]
    fn canonicalize_rejects_missing_paths() {
        // Mutant: canonicalize replaced with Ok(Default::default()).
        // A missing path must fail; an existing one must return its
        // canonical form (not empty).
        let err = canonicalize(
            Path::new("/nonexistent/hatchling-canonicalize-probe"),
            "probe",
        )
        .unwrap_err();
        assert!(matches!(err, Error::Spawn { .. }));
        assert!(err.to_string().starts_with("cannot canonicalize probe "));
        let ok = canonicalize(Path::new("/bin/true"), "probe").unwrap();
        assert!(ok.is_absolute());
        assert!(
            ok.as_os_str().len() > 1,
            "canonicalize must not return empty"
        );
    }

    #[test]
    fn shared_libraries_lists_ldd_dependencies() {
        // Mutants: shared_libraries replaced with Ok(vec![]), or the
        // vdso/absolute-path filters inverted. /bin/true is dynamic;
        // its libraries must be reported with absolute canonical
        // sources and relative targets.
        let libs = within_watchdog("shared_libraries", || {
            shared_libraries(Path::new("/bin/true"))
        })
        .unwrap();
        assert!(
            !libs.is_empty(),
            "shared_libraries must not return empty for a dynamic binary"
        );
        // Mutant: dedup `==` changed to `!=` (only push when a
        // *different* source already exists). /bin/true links two
        // distinct libraries; the mutant would return just the first.
        assert_eq!(
            libs.len(),
            2,
            "expected 2 distinct libraries for /bin/true, got {}",
            libs.len()
        );
        for lib in &libs {
            assert!(
                lib.source.is_absolute(),
                "library source must be absolute: {}",
                lib.source.display()
            );
            assert!(
                !lib.target.starts_with('/'),
                "library target must be relative: {}",
                lib.target
            );
            assert!(!lib.target.is_empty(), "library target must not be empty");
        }
        // No duplicates by source.
        let mut seen = HashSet::new();
        for lib in &libs {
            assert!(seen.insert(lib.source.clone()), "duplicate library source");
        }
    }

    #[test]
    fn shared_libraries_empty_for_static_or_missing() {
        // A nonexistent binary makes ldd fail -> fail-closed error
        // (not an empty vec masquerading as success).
        let result = within_watchdog("shared_libraries", || {
            shared_libraries(Path::new("/nonexistent/hatchling-ldd-probe"))
        });
        assert!(
            matches!(result, Err(Error::Spawn { .. })),
            "shared_libraries must fail for a missing binary"
        );
    }

    /// `ldd` is run from a fixed path, never one `PATH` names.
    #[test]
    fn ldd_is_found_at_a_fixed_path() {
        let found = ldd().unwrap();
        assert!(
            LDD.contains(&found.to_str().unwrap()),
            "{}",
            found.display()
        );
    }

    /// `ldd` runs as root only on what root alone could have changed;
    /// on a binary in a directory of another user's, as that user, and
    /// it still reads what it must. Not as root, as this process.
    #[test]
    fn ldd_runs_as_whoever_could_change_the_binary() {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(ldd_identity(Path::new("/bin/true")), None);
        let dir = std::env::temp_dir().join(format!("hatchling-ldd-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let binary = dir.join("true");
        std::fs::copy("/bin/true", &binary).unwrap();
        if hatchling_sys::euid() != 0 {
            // Not root: as this process, even on its own file.
            assert_eq!(ldd_identity(&binary), None);
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        // Root's file, in another user's directory: that user's to swap.
        std::os::unix::fs::chown(&dir, Some(4242), Some(4343)).unwrap();
        assert_eq!(ldd_identity(&binary), Some((4242, 4343)));
        // That user's file, in root's directory: that user's to rewrite.
        std::os::unix::fs::chown(&dir, Some(0), Some(0)).unwrap();
        std::os::unix::fs::chown(&binary, Some(4444), Some(4545)).unwrap();
        assert_eq!(ldd_identity(&binary), Some((4444, 4545)));
        // And ldd, run as that user, still reads it and its libraries.
        let owned = binary.clone();
        let output = within_watchdog("ldd_output", move || ldd_output(&owned)).unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("=>"));
        // Root's own binary, 0700, in another user's directory: as that
        // user, ldd can't read it, which shows it isn't run as root.
        std::os::unix::fs::chown(&dir, Some(4242), Some(4343)).unwrap();
        std::os::unix::fs::chown(&binary, Some(0), Some(0)).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = within_watchdog("ldd_output", move || ldd_output(&binary)).unwrap();
        assert!(!output.status.success(), "{output:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ldd_output_reports_the_tools_own_output() {
        let output = within_watchdog("ldd_output", || ldd_output(Path::new("/bin/true"))).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("=>"));
    }

    #[test]
    fn layout_paths_are_named_from_the_root() {
        assert_eq!(in_root(PROGRAM), "agent");
        assert_eq!(in_root(ENDPOINT_DIR), "sock");
        assert_eq!(in_root("plain"), "plain");
    }

    #[test]
    fn a_tool_brings_every_library_it_needs_once() {
        // bash needs libraries /bin/true does not (libtinfo); each
        // is in the view, under its own target, exactly once.
        let scratch = Scratch::new("tool-libs");
        let (mut skill, _) = specs(&scratch);
        skill.tools = vec![crate::Tool {
            source: PathBuf::from("/bin/bash"),
            name: String::from("bin/bash"),
        }];
        let needs = within_watchdog("ldd bash", || {
            shared_libraries(&canonicalize(Path::new("/bin/bash"), "tool")?)
        })
        .unwrap();
        let view = within_watchdog("view", move || view(&skill)).unwrap();
        let runtime: Vec<&str> = view
            .entries
            .iter()
            .filter(|entry| entry.origin == Origin::Runtime)
            .map(|entry| entry.name.as_str())
            .collect();
        for lib in &needs {
            assert_eq!(
                runtime.iter().filter(|name| **name == lib.target).count(),
                1,
                "{} in {runtime:?}",
                lib.target
            );
        }
    }

    #[test]
    fn a_view_lists_endpoint_program_grants_then_runtime() {
        let scratch = Scratch::new("order");
        let (_, mut harness) = specs(&scratch);
        harness.endpoint = Some(Endpoint {
            socket: scratch.0.join("sock").join("broker.sock"),
            dir: scratch.0.join("sock"),
        });
        let view = within_watchdog("view", move || view(&harness)).unwrap();
        let origins: Vec<Origin> = view.entries.iter().map(|entry| entry.origin).collect();
        let runtime = origins.len() - 5;
        assert_eq!(
            origins.get(..5).unwrap(),
            [
                Origin::Endpoint,
                Origin::Program,
                Origin::Granted,
                Origin::Granted,
                Origin::Granted
            ]
        );
        assert_eq!(runtime, 2, "/bin/true links two libraries");
        let names: Vec<&str> = view
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(
            names.get(..5).unwrap(),
            ["sock", "agent", "skill", "work", "scratch"]
        );
        let canonical = |name: &str| std::fs::canonicalize(scratch.0.join(name)).unwrap();
        assert_eq!(view.entries.first().unwrap().source, canonical("sock"));
        assert_eq!(
            view.entries.get(1).unwrap().source,
            std::fs::canonicalize("/bin/true").unwrap()
        );
        for entry in &view.entries {
            let granted_writable = matches!(entry.name.as_str(), "work" | "scratch");
            assert_eq!(entry.writable, granted_writable, "{}", entry.name);
            let is_dir = matches!(entry.origin, Origin::Endpoint | Origin::Granted);
            assert_eq!(entry.dir, is_dir, "{}", entry.name);
        }
    }

    #[test]
    fn a_harness_cell_writes_only_its_work_tree_and_scratch() {
        let scratch = Scratch::new("plan");
        let (skill, harness) = specs(&scratch);
        let root = scratch.0.join("root");
        let plan = mount_plan(view(&harness).unwrap(), &root).unwrap();
        let writable: Vec<(&str, &Path)> = plan
            .binds()
            .iter()
            .filter(|bind| bind.writable)
            .map(|bind| (bind.target.as_str(), bind.source.as_path()))
            .collect();
        let work = std::fs::canonicalize(scratch.0.join("work")).unwrap();
        let tmp = std::fs::canonicalize(scratch.0.join("scratch")).unwrap();
        assert_eq!(
            writable,
            [("work", work.as_path()), ("scratch", tmp.as_path())]
        );
        assert!(
            plan.binds()
                .iter()
                .filter(|bind| bind.target == "work" || bind.target == "scratch")
                .all(|bind| bind.is_dir)
        );
        let targets = |plan: &MountPlan| -> Vec<String> {
            plan.binds()
                .iter()
                .map(|bind| bind.target.clone())
                .collect()
        };
        let skill_plan = mount_plan(view(&skill).unwrap(), &root).unwrap();
        assert!(
            skill_plan.binds().iter().all(|bind| !bind.writable),
            "no other cell writes anywhere"
        );
        assert!(
            !targets(&skill_plan).contains(&String::from("sock")),
            "a cell without an endpoint has no socket"
        );
        let mut expected = targets(&skill_plan);
        expected.push(String::from("work"));
        expected.push(String::from("scratch"));
        let mut got = targets(&plan);
        got.sort_unstable();
        expected.sort_unstable();
        assert_eq!(
            got, expected,
            "the harness cell adds exactly its two directories"
        );
        let mut missing = harness;
        if let Some(work) = missing.exposures.get_mut(1) {
            work.source = scratch.0.join("gone");
        }
        assert!(
            matches!(view(&missing), Err(Error::Spawn { reason }) if reason.starts_with("cannot canonicalize harness directory ")),
            "a missing work tree fails closed"
        );
    }

    #[test]
    fn a_mount_plan_binds_the_view_in_order_and_refuses_a_bad_one() {
        let scratch = Scratch::new("bind-order");
        let (skill, _) = specs(&scratch);
        let root = scratch.0.join("root");
        let skill_view = view(&skill).unwrap();
        let names: Vec<String> = skill_view
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        let plan = mount_plan(skill_view, &root).unwrap();
        let targets: Vec<String> = plan
            .binds()
            .iter()
            .map(|bind| bind.target.clone())
            .collect();
        assert_eq!(targets, names);
        // A target that climbs out of the root is refused by
        // hatchling-sys, and the refusal is the view's.
        let mut climbing = view(&skill).unwrap();
        if let Some(entry) = climbing.entries.first_mut() {
            entry.name = String::from("../escape");
        }
        assert!(matches!(mount_plan(climbing, &root), Err(Error::View(_))));
    }
}
