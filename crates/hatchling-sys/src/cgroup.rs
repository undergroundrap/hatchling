//! Delegated cgroup v2 leaves: configure before placement, kill before removal.
//!
//! The caller supplies a delegated parent. This module never moves the caller
//! or provisions a delegation. The cell must not see the host cgroup mount.

use std::fs::{self, OpenOptions};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::Error;

static NEXT_CELL: AtomicU64 = AtomicU64::new(0);
const EMPTY_TIMEOUT: Duration = Duration::from_secs(5);

/// The limits for the whole cell, including its supervisor and init.
#[derive(Debug, Clone, Copy, Default)]
pub struct CgroupLimits {
    /// Maximum tasks, or the parent's bound when unset.
    pub processes: Option<u64>,
    /// Maximum memory in bytes, or the parent's bound when unset.
    pub memory: Option<u64>,
    /// CPU allowance in thousandths of a core, or the parent's when unset.
    pub cpu_millis: Option<u64>,
}

/// A cell's owned cgroup. Drop kills its remaining tasks and removes its leaf.
#[derive(Debug)]
pub struct CellCgroup {
    path: PathBuf,
    removed: bool,
}

fn failed(operation: &'static str, path: &Path, source: std::io::Error) -> Error {
    Error::Cgroup {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn invalid(path: &Path, reason: &str) -> Error {
    failed(
        "validate delegation",
        path,
        std::io::Error::new(std::io::ErrorKind::InvalidInput, reason),
    )
}

/// The controllers cells need that `parent` doesn't have. A cgroup that
/// isn't there has no answer, and nothing is said to be missing.
#[must_use]
pub fn missing_controllers(parent: &Path) -> Vec<&'static str> {
    let Ok(available) = fs::read_to_string(parent.join("cgroup.controllers")) else {
        return Vec::new();
    };
    ["pids", "memory", "cpu"]
        .into_iter()
        .filter(|controller| !available.split_whitespace().any(|name| name == *controller))
        .collect()
}

/// Why cells can't be given a cgroup under a parent, as far as can be
/// seen without changing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationProblem {
    /// The parent isn't in a cgroup v2 tree.
    NotCgroup2,
    /// The parent is there, and lacks these controllers.
    ParentLacks(Vec<&'static str>),
    /// The parent isn't there yet, and the root of the tree it would be
    /// made in doesn't offer these controllers: nothing below a root has
    /// one the root lacks.
    TreeLacks(Vec<&'static str>),
}

/// What's wrong with giving cells a cgroup under `parent`, as far as can
/// be seen without changing anything; `None` when nothing that can be
/// seen is.
///
/// [`prepare_parent`] is the check that counts, and it changes things: it
/// enables controllers and makes a group. This one is for saying, before
/// a broker is started, that [`prepare_parent`] can't pass. A `parent`
/// that isn't there yet (a service's cgroup, before the service has ever
/// started) is judged by the tree it will be made in.
#[must_use]
pub fn delegation_problem(parent: &Path) -> Option<DelegationProblem> {
    delegation_problem_in(parent, &is_cgroup2)
}

/// Whether `path` is on a cgroup v2 filesystem.
fn is_cgroup2(path: &Path) -> bool {
    rustix::fs::statfs(path).is_ok_and(|stat| stat.f_type == libc::CGROUP2_SUPER_MAGIC)
}

/// [`delegation_problem`], where `in_tree` says whether a path is in a
/// cgroup v2 tree.
fn delegation_problem_in(
    parent: &Path,
    in_tree: &dyn Fn(&Path) -> bool,
) -> Option<DelegationProblem> {
    let nearest = parent.ancestors().find(|path| path.exists())?;
    if !in_tree(nearest) {
        return Some(DelegationProblem::NotCgroup2);
    }
    if nearest == parent {
        let missing = missing_controllers(parent);
        return (!missing.is_empty()).then_some(DelegationProblem::ParentLacks(missing));
    }
    let root = nearest
        .ancestors()
        .take_while(|path| in_tree(path))
        .last()?;
    let missing = missing_controllers(root);
    (!missing.is_empty()).then_some(DelegationProblem::TreeLacks(missing))
}

/// Checks an empty cgroup v2 parent and enables its required controllers.
///
/// # Errors
///
/// Refuses a non-cgroup path, a parent holding processes, unavailable
/// controllers, or a delegation the caller cannot write or create leaves in.
pub fn prepare_parent(parent: &Path) -> Result<(), Error> {
    if !parent.is_absolute() {
        return Err(invalid(parent, "HATCHLING_CELL_CGROUP must be absolute"));
    }
    let stat =
        rustix::fs::statfs(parent).map_err(|source| failed("statfs", parent, source.into()))?;
    if stat.f_type != libc::CGROUP2_SUPER_MAGIC {
        return Err(invalid(parent, "HATCHLING_CELL_CGROUP is not cgroup v2"));
    }
    if !read(parent, "cgroup.procs")?.trim().is_empty() {
        return Err(invalid(
            parent,
            "HATCHLING_CELL_CGROUP holds processes; cells need a cgroup of their own",
        ));
    }
    let available = read(parent, "cgroup.controllers")?;
    let enabled = read(parent, "cgroup.subtree_control")?;
    let mut missing = Vec::new();
    for controller in ["pids", "memory", "cpu"] {
        if !available.split_whitespace().any(|name| name == controller) {
            return Err(invalid(
                parent,
                "delegation needs pids, memory and cpu controllers",
            ));
        }
        if !enabled.split_whitespace().any(|name| name == controller) {
            missing.push(format!("+{controller}"));
        }
    }
    for file in ["cgroup.procs", "cgroup.subtree_control"] {
        OpenOptions::new()
            .write(true)
            .open(parent.join(file))
            .map_err(|source| failed("open delegation for writing", parent, source))?;
    }
    if !missing.is_empty() {
        write(parent, "cgroup.subtree_control", &missing.join(" "))?;
    }
    // This also proves directory creation and cgroup.kill support before a
    // broker starts. No process is placed in this temporary leaf.
    let mut probe = CellCgroup::new_leaf(parent)?;
    OpenOptions::new()
        .write(true)
        .open(probe.path.join("cgroup.kill"))
        .map_err(|source| failed("open cgroup.kill", &probe.path, source))?;
    probe.finish()
}

impl CellCgroup {
    fn new_leaf(parent: &Path) -> Result<Self, Error> {
        let path = parent.join(format!(
            "cell-{}-{}",
            std::process::id(),
            NEXT_CELL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).map_err(|source| failed("create leaf", &path, source))?;
        Ok(Self {
            path,
            removed: false,
        })
    }

    /// Creates an empty leaf with every limit set before any task can enter.
    ///
    /// # Errors
    ///
    /// Returns the first rejected delegation, limit, or filesystem operation.
    pub fn create(parent: &Path, limits: CgroupLimits) -> Result<Self, Error> {
        prepare_parent(parent)?;
        let group = Self::new_leaf(parent)?;
        write(&group.path, "pids.max", &bound(limits.processes))?;
        write(&group.path, "memory.max", &bound(limits.memory))?;
        let quota = cpu_max(limits.cpu_millis)?;
        write(&group.path, "cpu.max", &quota)?;
        Ok(group)
    }

    /// Creates a leaf whose tasks cannot use swap.
    ///
    /// # Errors
    ///
    /// Returns an error if the leaf or any limit, including the swap limit,
    /// cannot be configured before placement.
    pub fn create_without_swap(parent: &Path, limits: CgroupLimits) -> Result<Self, Error> {
        let group = Self::create(parent, limits)?;
        write(&group.path, "memory.swap.max", "0")?;
        Ok(group)
    }

    /// The owner's diagnostic path. It is never mounted inside the cell.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Places the still-blocked setup child into this leaf before release.
    ///
    /// # Errors
    ///
    /// Fails closed if delegation or a process limit prevents placement.
    pub fn place(&self, pid: i32) -> Result<(), Error> {
        write(&self.path, "cgroup.procs", &pid.to_string())
    }

    /// Places a command in this leaf before it can exec or fork descendants.
    /// The owned leaf must stay live until the command and its descendants end.
    ///
    /// # Errors
    ///
    /// Returns an error if placement cannot be prepared. A placement failure
    /// in the child makes `Command::spawn` fail before the program runs.
    pub fn join_on_exec(&self, command: &mut std::process::Command) -> Result<(), Error> {
        let procs = OpenOptions::new()
            .write(true)
            .open(self.path.join("cgroup.procs"))
            .map_err(|source| failed("open cgroup.procs", &self.path, source))?;
        // Keep the pinned fd outside the stdio slots Command may replace.
        let procs = rustix::io::fcntl_dupfd_cloexec(procs, 3)
            .map_err(|source| failed("pin cgroup.procs", &self.path, source.into()))?;
        // SAFETY: the closure only writes a fixed byte through the pinned fd.
        // Zero places the calling child without looking up a numeric PID.
        unsafe {
            command.pre_exec(move || match rustix::io::write(&procs, b"0")? {
                1 => Ok(()),
                _ => Err(std::io::Error::from_raw_os_error(libc::EIO)),
            });
        }
        Ok(())
    }

    /// Kills all remaining tasks, waits for emptiness, and removes this leaf.
    ///
    /// # Errors
    ///
    /// Reports failed killing, a bounded emptiness timeout, or failed removal.
    pub fn finish(&mut self) -> Result<(), Error> {
        if self.removed {
            return Ok(());
        }
        write(&self.path, "cgroup.kill", "1")?;
        let started = Instant::now();
        while !read(&self.path, "cgroup.events")?
            .lines()
            .any(|line| line == "populated 0")
        {
            if started.elapsed() >= EMPTY_TIMEOUT {
                return Err(failed(
                    "wait for empty leaf",
                    &self.path,
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "cgroup remains populated"),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        remove_descendants(&self.path)?;
        fs::remove_dir(&self.path).map_err(|source| failed("remove leaf", &self.path, source))?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for CellCgroup {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// Remove only directories in this already-killed, broker-owned subtree.
fn remove_descendants(path: &Path) -> Result<(), Error> {
    for entry in fs::read_dir(path).map_err(|source| failed("list descendants", path, source))? {
        let entry = entry.map_err(|source| failed("read descendant", path, source))?;
        let kind = entry
            .file_type()
            .map_err(|source| failed("inspect descendant", path, source))?;
        if kind.is_dir() {
            remove_descendants(&entry.path())?;
            fs::remove_dir(entry.path())
                .map_err(|source| failed("remove descendant", path, source))?;
        }
    }
    Ok(())
}

fn bound(value: Option<u64>) -> String {
    value.map_or_else(|| String::from("max"), |value| value.to_string())
}

fn cpu_max(millis: Option<u64>) -> Result<String, Error> {
    millis.map_or_else(
        || Ok(String::from("max 100000")),
        |value| {
            value
                .checked_mul(100)
                .filter(|quota| *quota >= 1000)
                .map(|quota| format!("{quota} 100000"))
                .ok_or_else(|| {
                    invalid(
                        Path::new("cpu.max"),
                        "CPU limit is below 0.01 cores or overflows",
                    )
                })
        },
    )
}

fn read(path: &Path, file: &'static str) -> Result<String, Error> {
    fs::read_to_string(path.join(file)).map_err(|source| failed(file, path, source))
}

fn write(path: &Path, file: &'static str, value: &str) -> Result<(), Error> {
    fs::write(path.join(file), value).map_err(|source| failed(file, path, source))
}

/// Half the host's physical memory, the default for a configured cell cgroup.
///
/// # Errors
///
/// Returns a typed failure if the kernel's memory total cannot be read.
pub fn default_memory() -> Result<u64, Error> {
    let path = Path::new("/proc/meminfo");
    let text = fs::read_to_string(path).map_err(|source| failed("read MemTotal", path, source))?;
    half_memory(&text).ok_or_else(|| invalid(path, "missing or invalid MemTotal"))
}

fn half_memory(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .strip_suffix(" kB")?
        .trim()
        .parse::<u64>()
        .ok()?
        .checked_mul(512)
        .filter(|bytes| *bytes > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// What a cgroup lacks is named; one that isn't there lacks nothing
    /// anyone can say.
    /// A tree of plain files standing for a cgroup tree: `root` offers
    /// `offered`, and holds an empty `system.slice`.
    fn tree(name: &str, offered: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-delegation-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("system.slice")).unwrap();
        fs::write(root.join("cgroup.controllers"), offered).unwrap();
        root
    }

    /// A service's cgroup isn't there until the service has started, and a
    /// check that looked only at it found nothing wrong on a machine whose
    /// cgroups offer no controllers: the installer said cells were bounded,
    /// and the broker then stopped at every start. Before it's there, it's
    /// judged by the root of the tree it will be made in.
    #[test]
    fn a_cgroup_not_made_yet_is_judged_by_its_trees_root() {
        let root = tree("root", "cpuset io hugetlb rdma\n");
        let inside = |path: &Path| path.starts_with(&root);
        let parent = root.join("system.slice").join("hatchling.service");
        assert_eq!(
            delegation_problem_in(&parent, &inside),
            Some(DelegationProblem::TreeLacks(vec!["pids", "memory", "cpu"]))
        );
        fs::write(root.join("cgroup.controllers"), "cpu io memory\n").unwrap();
        assert_eq!(
            delegation_problem_in(&parent, &inside),
            Some(DelegationProblem::TreeLacks(vec!["pids"]))
        );
        // A root that offers them all: nothing to see. What a slice between
        // has enabled so far doesn't count: systemd enables them as it
        // starts the service.
        fs::write(
            root.join("cgroup.controllers"),
            "cpuset cpu io memory pids\n",
        )
        .unwrap();
        fs::write(
            root.join("system.slice").join("cgroup.controllers"),
            "memory pids\n",
        )
        .unwrap();
        assert_eq!(delegation_problem_in(&parent, &inside), None);
        fs::remove_dir_all(&root).unwrap();
    }

    /// One that's there is judged by what it has, whatever its root offers.
    #[test]
    fn a_cgroup_that_is_there_is_judged_by_what_it_has() {
        let root = tree("there", "cpuset cpu io memory pids\n");
        let inside = |path: &Path| path.starts_with(&root);
        let parent = root.join("system.slice").join("hatchling.service");
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("cgroup.controllers"), "memory pids\n").unwrap();
        assert_eq!(
            delegation_problem_in(&parent, &inside),
            Some(DelegationProblem::ParentLacks(vec!["cpu"]))
        );
        fs::write(parent.join("cgroup.controllers"), "cpu memory pids\n").unwrap();
        assert_eq!(delegation_problem_in(&parent, &inside), None);
        // A root that offers none doesn't matter to one that has them.
        fs::write(root.join("cgroup.controllers"), "\n").unwrap();
        assert_eq!(delegation_problem_in(&parent, &inside), None);
        fs::remove_dir_all(&root).unwrap();
    }

    /// A path that isn't in a cgroup tree at all is said to be that, there
    /// or not; a relative one names nothing that can be looked at.
    #[test]
    fn a_path_outside_a_cgroup_tree_is_said_to_be() {
        let root = tree("outside", "cpu memory pids\n");
        let nowhere = |_: &Path| false;
        for parent in [root.clone(), root.join("not").join("made")] {
            assert_eq!(
                delegation_problem_in(&parent, &nowhere),
                Some(DelegationProblem::NotCgroup2)
            );
            // By the filesystem itself: a temporary directory is no cgroup.
            assert_eq!(
                delegation_problem(&parent),
                Some(DelegationProblem::NotCgroup2)
            );
        }
        assert_eq!(
            delegation_problem_in(Path::new("not/absolute/and/not/there"), &nowhere),
            None
        );
        fs::remove_dir_all(&root).unwrap();
        // And a real cgroup v2 mount is one, where this machine has one.
        let mount = Path::new("/sys/fs/cgroup");
        if mount.join("cgroup.controllers").exists() {
            assert!(is_cgroup2(mount));
            assert_ne!(
                delegation_problem(&mount.join("hatchling-not-made")),
                Some(DelegationProblem::NotCgroup2)
            );
        }
        assert!(!is_cgroup2(Path::new("/nonexistent/hatchling")));
    }

    #[test]
    fn missing_controllers_are_named() {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-missing-controllers-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        assert!(missing_controllers(&dir).is_empty());
        fs::write(dir.join("cgroup.controllers"), "memory pids\n").unwrap();
        assert_eq!(missing_controllers(&dir), ["cpu"]);
        fs::write(dir.join("cgroup.controllers"), "cpuset cpu memory pids\n").unwrap();
        assert!(missing_controllers(&dir).is_empty());
        fs::write(dir.join("cgroup.controllers"), "cpuset\n").unwrap();
        assert_eq!(missing_controllers(&dir), ["pids", "memory", "cpu"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bounds_and_cpu_quota_preserve_configured_values() {
        assert_eq!(bound(None), "max");
        assert_eq!(bound(Some(64)), "64");
        assert_eq!(cpu_max(None).unwrap(), "max 100000");
        assert_eq!(cpu_max(Some(1500)).unwrap(), "150000 100000");
        assert_eq!(cpu_max(Some(10)).unwrap(), "1000 100000");
        assert!(cpu_max(Some(9)).is_err());
        assert!(cpu_max(Some(u64::MAX)).is_err());
    }

    #[test]
    fn memory_default_is_half_memtotal_in_bytes() {
        assert_eq!(
            half_memory("MemFree: 9 kB\nMemTotal:  1234 kB\n"),
            Some(631_808)
        );
        for bad in [
            "",
            "MemTotal: 0 kB",
            "MemTotal: 12 B",
            "MemTotal: lots kB",
            "MemTotal: 18446744073709551615 kB",
        ] {
            assert_eq!(half_memory(bad), None);
        }
        let actual = default_memory().unwrap();
        assert_eq!(
            Some(actual),
            half_memory(&fs::read_to_string("/proc/meminfo").unwrap())
        );
    }

    #[test]
    fn delegation_rejects_missing_relative_and_ordinary_paths() {
        for path in ["relative", "/no-such-hatchling-cgroup", "/tmp"] {
            assert!(prepare_parent(Path::new(path)).is_err());
            assert!(CellCgroup::create(Path::new(path), CgroupLimits::default()).is_err());
        }
        assert!(read(Path::new("/no-such-hatchling-cgroup"), "cgroup.events").is_err());
        assert!(write(Path::new("/no-such-hatchling-cgroup"), "cgroup.kill", "1").is_err());
        assert!(matches!(
            failed("test", Path::new("/x"), std::io::Error::other("failure")),
            Error::Cgroup { .. }
        ));
        assert!(
            invalid(Path::new("/x"), "refused")
                .to_string()
                .contains("refused")
        );
    }

    fn test_parent() -> Option<PathBuf> {
        let base = std::env::var_os("HATCHLING_TEST_CGROUP")
            .map(PathBuf::from)
            .or_else(|| (crate::euid() == 0).then(|| PathBuf::from("/sys/fs/cgroup")));
        let Some(base) = base else {
            assert!(
                !std::env::var("HATCHLING_REQUIRE_CGROUP").is_ok_and(|value| value == "1"),
                "HATCHLING_REQUIRE_CGROUP=1 but no delegation"
            );
            let _ = writeln!(
                std::io::stderr(),
                "skipping cgroup test: no delegation; set HATCHLING_TEST_CGROUP"
            );
            return None;
        };
        prepare_parent(&base).unwrap();
        let path = base.join(format!(
            "sys-test-{}-{}",
            std::process::id(),
            NEXT_CELL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Some(path)
    }

    struct Sleeper(std::process::Child);
    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_swapless_command_joins_before_exec_and_refuses_a_removed_leaf() {
        let Some(parent) = test_parent() else {
            return;
        };
        let mut group = CellCgroup::create_without_swap(&parent, CgroupLimits::default()).unwrap();
        let leaf = group
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(read(group.path(), "memory.swap.max").unwrap().trim(), "0");
        let mut command = std::process::Command::new("/bin/cat");
        command.arg("/proc/self/cgroup");
        group.join_on_exec(&mut command).unwrap();
        let output = command.output().unwrap();
        assert!(output.status.success());
        let membership = std::str::from_utf8(&output.stdout).unwrap();
        assert!(
            membership.trim_end().ends_with(&format!("/{leaf}")),
            "{membership}"
        );

        let mut late = std::process::Command::new("/bin/true");
        group.join_on_exec(&mut late).unwrap();
        group.finish().unwrap();
        assert!(late.status().is_err(), "a command entered a removed leaf");
        assert!(group.join_on_exec(&mut command).is_err());
        fs::remove_dir(parent).unwrap();
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "simulates SIGKILL of a helper, which cannot run its cgroup destructor"
    )]
    fn owned_subtree_cleanup_survives_a_helpers_lost_destructor() {
        let Some(parent) = test_parent() else {
            return;
        };
        let mut outer = CellCgroup::create(&parent, CgroupLimits::default()).unwrap();
        let inner = CellCgroup::create(outer.path(), CgroupLimits::default()).unwrap();
        let inner_path = inner.path().to_path_buf();
        let outer_path = outer.path().to_path_buf();
        let mut sleeper = Sleeper(
            std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        inner.place(i32::try_from(sleeper.0.id()).unwrap()).unwrap();
        // SIGKILL of a helper loses its Rust guard. The living broker's outer
        // guard must still kill and remove this entire private hierarchy.
        std::mem::forget(inner);
        outer.finish().unwrap();
        assert!(!sleeper.0.wait().unwrap().success());
        assert!(!inner_path.exists());
        assert!(!outer_path.exists());
        remove_descendants(&parent).unwrap();
        fs::remove_dir(parent).unwrap();
    }

    #[test]
    fn real_cgroups_hold_limits_place_tasks_and_kill_before_removal() {
        let Some(parent) = test_parent() else {
            return;
        };
        prepare_parent(&parent).unwrap();
        let limits = CgroupLimits {
            processes: Some(32),
            memory: Some(64 << 20),
            cpu_millis: Some(1500),
        };
        let mut group = CellCgroup::create(&parent, limits).unwrap();
        let leaf = group.path().to_path_buf();
        assert_eq!(read(group.path(), "pids.max").unwrap().trim(), "32");
        assert_eq!(read(group.path(), "memory.max").unwrap().trim(), "67108864");
        assert_eq!(
            read(group.path(), "cpu.max").unwrap().trim(),
            "150000 100000"
        );
        write(group.path(), "pids.max", "31").unwrap();
        assert_eq!(read(group.path(), "pids.max").unwrap().trim(), "31");
        let mut child = Sleeper(
            std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        let pid = i32::try_from(child.0.id()).unwrap();
        group.place(pid).unwrap();
        assert!(
            read(group.path(), "cgroup.procs")
                .unwrap()
                .lines()
                .any(|line| line == pid.to_string())
        );
        group.finish().unwrap();
        assert!(!leaf.exists(), "finish must remove its leaf");
        assert!(
            !child.0.wait().unwrap().success(),
            "cgroup.kill must kill the sleeper"
        );
        group.finish().unwrap();
        drop(group);
        let leaf = CellCgroup::new_leaf(&parent).unwrap();
        let path = leaf.path().to_path_buf();
        drop(leaf);
        assert!(!path.exists(), "drop must remove its leaf");
        let unlimited = CellCgroup::create(&parent, CgroupLimits::default()).unwrap();
        assert_eq!(read(unlimited.path(), "memory.max").unwrap().trim(), "max");
        assert_eq!(read(unlimited.path(), "pids.max").unwrap().trim(), "max");
        assert_eq!(
            read(unlimited.path(), "cpu.max").unwrap().trim(),
            "max 100000"
        );
        drop(unlimited);
        assert!(
            CellCgroup::create(
                &parent,
                CgroupLimits {
                    cpu_millis: Some(9),
                    ..limits
                }
            )
            .is_err()
        );
        assert_eq!(
            fs::read_dir(&parent)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().unwrap().is_dir())
                .count(),
            0
        );
        fs::remove_dir(parent).unwrap();
    }

    #[test]
    fn delegation_requires_every_controller_and_enables_only_missing_ones() {
        let Some(parent) = test_parent() else {
            return;
        };
        write(&parent, "cgroup.subtree_control", "+pids +memory").unwrap();
        let missing = CellCgroup::new_leaf(&parent).unwrap();
        assert!(
            prepare_parent(missing.path())
                .unwrap_err()
                .to_string()
                .contains("delegation needs pids, memory and cpu controllers")
        );
        drop(missing);
        write(&parent, "cgroup.subtree_control", "+cpu").unwrap();
        let partial = CellCgroup::new_leaf(&parent).unwrap();
        write(partial.path(), "cgroup.subtree_control", "+pids +memory").unwrap();
        prepare_parent(partial.path()).unwrap();
        let enabled = read(partial.path(), "cgroup.subtree_control").unwrap();
        for required in ["pids", "memory", "cpu"] {
            assert!(enabled.split_whitespace().any(|name| name == required));
        }
        drop(partial);
        fs::remove_dir(parent).unwrap();
    }

    #[test]
    fn a_parent_holding_processes_is_refused_without_moving_the_caller() {
        let Some(parent) = test_parent() else {
            return;
        };
        let mut child = Sleeper(
            std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        write(&parent, "cgroup.procs", &child.0.id().to_string()).unwrap();
        assert!(
            prepare_parent(&parent)
                .unwrap_err()
                .to_string()
                .contains("holds processes")
        );
        write(&parent, "cgroup.kill", "1").unwrap();
        child.0.wait().unwrap();
        fs::remove_dir(parent).unwrap();
    }
}
