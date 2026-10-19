//! Mount-table construction for the cell.
//!
//! A [`MountPlan`] is built and validated *before* any mount syscall
//! runs: every bind source must be an absolute path inside the
//! caller-provided allowlist, and every in-cell target must be a single
//! safe path component. Validation failures happen before the mount
//! namespace is touched, so a bad plan can never leave a half-built
//! cell behind.
//!
//! The plan the broker builds: a fresh read-only tmpfs root, one
//! read-only bind mount of the broker socket directory, read-only bind
//! mounts of the agent binary and exactly the shared libraries its ELF
//! headers need, then `pivot_root` into the new root with the old root
//! detached. What isn't mounted can't be attacked.
//!
//! A harness cell also gets writable binds: its work tree and its
//! scratch directory. They stay `NOSUID|NODEV`; only they are writable.
//!
//! A real CLI's cell also gets the null, zero and random devices,
//! writable and without `NODEV` (a `NODEV` device cannot be opened), and a
//! `/proc` of its own processes ([`MountPlan::with_proc`]).

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::Error;

// Mount-flag composites as literals, following the same pattern as the
// BPF opcode constants in seccomp.rs: MS_NOSUID (0x2) | MS_NODEV (0x4)
// and MS_BIND (0x1000) | MS_RDONLY (0x1) have non-overlapping bits, so
// `|`-to-`^` mutants are semantically equivalent and unkillable as
// expressions. The literals remove the operator entirely; the tests
// below pin the exact bits.
const TMPFS_ROOT_FLAGS: rustix::mount::MountFlags =
    rustix::mount::MountFlags::from_bits_retain(0x6); // NOSUID | NODEV
const REMOUNT_READONLY_FLAGS: rustix::mount::MountFlags =
    rustix::mount::MountFlags::from_bits_retain(0x1001); // BIND | RDONLY
const REMOUNT_WRITABLE_FLAGS: rustix::mount::MountFlags =
    rustix::mount::MountFlags::from_bits_retain(0x1006); // BIND | NOSUID | NODEV
const REMOUNT_DEVICE_FLAGS: rustix::mount::MountFlags =
    rustix::mount::MountFlags::from_bits_retain(0x1002); // BIND | NOSUID
const PROC_FLAGS: rustix::mount::MountFlags = rustix::mount::MountFlags::from_bits_retain(0xE); // NOSUID | NODEV | NOEXEC

/// Mount flags for a remount of `source`, `base` added to the flags it
/// must keep.
///
/// A plain bind inherits the source mount's flags; the remount then
/// applies RDONLY. But flags the kernel locks on the source mount must
/// be repeated in the remount: omitting a locked flag is an attempt to
/// clear it, which the kernel rejects with EPERM. The locked set is
/// NOSUID, NODEV, NOEXEC, and the atime flags — so every `statvfs`
/// flag with a `mount(2)` counterpart is carried over (the same
/// mapping bubblewrap uses for its remounts), and `base` is what the
/// remount adds: BIND|RDONLY for a read-only bind, BIND|NOSUID|NODEV for a
/// writable one. A statvfs failure falls back to `base` alone — the
/// remount then either succeeds (no locked flags) or fails loudly with
/// EPERM, never silently.
fn remount_flags_for(source: &Path, base: rustix::mount::MountFlags) -> rustix::mount::MountFlags {
    use rustix::fs::StatVfsMountFlags as Vf;
    use rustix::mount::MountFlags;
    let mut flags = base;
    if let Ok(vfs) = rustix::fs::statvfs(source) {
        // (statvfs flag, mount flag): each ST_* bit with an MS_*
        // counterpart is retained from the source mount. RDONLY needs
        // no mapping — the remount enforces it unconditionally.
        for (vf, mf) in [
            (Vf::NOSUID, MountFlags::NOSUID),
            (Vf::NODEV, MountFlags::NODEV),
            (Vf::NOEXEC, MountFlags::NOEXEC),
            (Vf::SYNCHRONOUS, MountFlags::SYNCHRONOUS),
            (Vf::MANDLOCK, MountFlags::PERMIT_MANDATORY_FILE_LOCKING),
            (Vf::NOATIME, MountFlags::NOATIME),
            (Vf::NODIRATIME, MountFlags::NODIRATIME),
            (Vf::RELATIME, MountFlags::RELATIME),
        ] {
            if vfs.f_flag.contains(vf) {
                flags |= mf;
            }
        }
    }
    flags
}

/// One bind mount: host `source` appears in-cell at `/<target>`,
/// read-only unless `writable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMount {
    /// Absolute host path to bind-mount. Must be in the plan's
    /// allowlist.
    pub source: PathBuf,
    /// Single path component, the in-cell name (`sock`, `agent`, ...).
    /// The mount lands at `/<target>`.
    pub target: String,
    /// Whether the source is a directory (`true`) or a file (`false`).
    /// The in-cell mount point is created to match.
    pub is_dir: bool,
    /// Whether the cell may write here (a harness's work tree and
    /// scratch directory). Still `NOSUID|NODEV`. Every other bind is
    /// read-only.
    pub writable: bool,
    /// Whether the source is a character device the cell writes (
    /// the null, zero and random devices): writable and `NOSUID`, but not
    /// `NODEV`, which would make it unopenable. The plan refuses it for
    /// anything but a character device.
    pub device: bool,
}

/// A validated mount plan for one cell.
#[derive(Debug, Clone)]
pub struct MountPlan {
    /// Host staging directory that becomes the new root. Must be an
    /// absolute path; the spawner creates it `0700`.
    new_root: PathBuf,
    /// Validated bind mounts, in application order.
    binds: Vec<BindMount>,
    /// Whether the new root gets a procfs of the cell's own processes.
    proc: bool,
}

impl MountPlan {
    /// Builds and validates a plan.
    ///
    /// `allowlist` is the exact set of absolute host paths the caller
    /// permits as bind sources (the broker passes the socket directory,
    /// the agent binary, and the `ldd`-resolved libraries — nothing
    /// else). Every bind source must be a member; anything else is
    /// rejected before any syscall runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidMountPlan`] if the new root is not
    /// absolute, a bind source is not absolute or not in the allowlist,
    /// or a target is not a relative all-`Normal` path (see
    /// [`is_safe_target`]).
    pub fn new(
        new_root: PathBuf,
        allowlist: &HashSet<PathBuf>,
        binds: Vec<BindMount>,
    ) -> Result<Self, Error> {
        if !new_root.is_absolute() {
            return Err(Error::InvalidMountPlan {
                reason: format!("new root must be an absolute path: {}", new_root.display()),
            });
        }
        for bind in &binds {
            if !bind.source.is_absolute() {
                return Err(Error::InvalidMountPlan {
                    reason: format!("bind source must be absolute: {}", bind.source.display()),
                });
            }
            if !allowlist.contains(&bind.source) {
                return Err(Error::InvalidMountPlan {
                    reason: format!(
                        "bind source not in the allowlist: {}",
                        bind.source.display()
                    ),
                });
            }
            if !is_safe_target(&bind.target) {
                return Err(Error::InvalidMountPlan {
                    reason: format!(
                        "bind target must be a relative path of normal components only: {:?}",
                        bind.target
                    ),
                });
            }
            if bind.device && !is_char_device(&bind.source) {
                return Err(Error::InvalidMountPlan {
                    reason: format!(
                        "a device bind must be a character device: {}",
                        bind.source.display()
                    ),
                });
            }
        }
        Ok(Self {
            new_root,
            binds,
            proc: false,
        })
    }

    /// The same plan, with a procfs of the applying process's pid
    /// namespace at the new root's `/proc`.
    ///
    /// `subset=pid` shows only processes: none of the host's `/proc/sys`,
    /// `/proc/kcore` or other tables; it is `NOSUID|NODEV|NOEXEC`. The
    /// plan must be applied by the cell's pid 1: the kernel lets a user
    /// namespace mount procfs only while a whole one is still visible,
    /// so it is mounted before the old root is detached.
    #[must_use]
    pub const fn with_proc(mut self) -> Self {
        self.proc = true;
        self
    }

    /// Whether the plan mounts a procfs of the cell's own processes.
    #[must_use]
    pub const fn has_proc(&self) -> bool {
        self.proc
    }

    /// The staging directory that becomes the new root.
    #[must_use]
    pub fn new_root(&self) -> &Path {
        &self.new_root
    }

    /// The validated bind mounts, in application order.
    #[must_use]
    pub fn binds(&self) -> &[BindMount] {
        &self.binds
    }

    /// Applies the plan in the current mount namespace, and becomes the
    /// cell's user.
    ///
    /// Mounts a `tmpfs` on the new root, creates the mount points,
    /// bind-mounts each source read-only, remounts the root read-only,
    /// then `pivot_root`s into it and detaches the old root. On success
    /// the process's root is the new cell root at `/`, and it runs as
    /// the cell's user (uid/gid 0 of its namespace), capabilities kept.
    ///
    /// The plan runs in two identities. A root broker's child holds host
    /// uid 0 until it becomes the cell's user: that identity reaches the
    /// broker's staging tree and bind sources, but a tmpfs of the cell's
    /// namespace takes new entries only from a user mapped there. So the
    /// broker's identity mounts the tmpfs, opens every source and steps
    /// into the new root; the cell's user builds the tree from there,
    /// binding each source through its open fd.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Mount`] if any mount syscall fails, or
    /// [`Error::Process`] if the switch to the cell's user does. A
    /// failure partway is reported with the failing operation; the
    /// caller is expected to abort the spawn (the child exits), never to
    /// continue with a half-built cell.
    pub fn apply(&self) -> Result<(), Error> {
        let staged = self.stage()?;
        crate::become_cell_user()?;
        self.build(&staged)
    }

    /// The plan's first half, as the broker's own file identity: the
    /// tmpfs on the new root (owned by the cell's user), each bind
    /// source opened, the flags every remount keeps, and the working
    /// directory moved into the new root.
    fn stage(&self) -> Result<Staged, Error> {
        // Fresh empty root. NOSUID|NODEV: nothing in the cell can gain
        // privilege via setuid bits or device nodes. `uid`/`gid` are read
        // in the cell's namespace: its root belongs to the cell's user.
        rustix::mount::mount(
            "tmpfs",
            &self.new_root,
            "tmpfs",
            TMPFS_ROOT_FLAGS,
            "size=64M,mode=0755,uid=0,gid=0",
        )
        .map_err(mount_err("mount(tmpfs)"))?;
        let mut sources = Vec::with_capacity(self.binds.len());
        for bind in &self.binds {
            // Two disjoint flags, joined by `union`: `|` there would leave
            // `^` an equivalent mutant.
            let source = rustix::fs::open(
                &bind.source,
                rustix::fs::OFlags::PATH.union(rustix::fs::OFlags::CLOEXEC),
                rustix::fs::Mode::empty(),
            )
            .map_err(mount_err("open(bind source)"))?;
            // A plain bind inherits the source's writability; the
            // remount makes it read-only in the cell (or, for a writable
            // bind, NOSUID|NODEV; for a device, NOSUID), retaining the
            // source mount's locked flags (see remount_flags_for).
            let base = if bind.device {
                REMOUNT_DEVICE_FLAGS
            } else if bind.writable {
                REMOUNT_WRITABLE_FLAGS
            } else {
                REMOUNT_READONLY_FLAGS
            };
            sources.push((source, remount_flags_for(&bind.source, base)));
        }
        // The root goes read-only last, retaining the tmpfs's NOSUID|NODEV.
        let root_flags = remount_flags_for(&self.new_root, REMOUNT_READONLY_FLAGS);
        std::env::set_current_dir(&self.new_root).map_err(|source| Error::Mount {
            operation: "chdir(new root)",
            source,
        })?;
        Ok(Staged {
            sources,
            root_flags,
        })
    }

    /// The plan's second half, as the cell's user, relative to the new
    /// root: the mount points, each bind (its source by fd), the procfs,
    /// the read-only root, the pivot and the old root's detach.
    fn build(&self, staged: &Staged) -> Result<(), Error> {
        use std::os::fd::AsRawFd as _;
        // The pivot_root put_old directory.
        std::fs::create_dir("old").map_err(|source| Error::Mount {
            operation: "mkdir(old)",
            source,
        })?;
        for (bind, (source, flags)) in self.binds.iter().zip(&staged.sources) {
            let target = Path::new(&bind.target);
            if bind.is_dir {
                std::fs::create_dir(target).map_err(|source| Error::Mount {
                    operation: "mkdir(bind target)",
                    source,
                })?;
            } else {
                // Nested file targets (shared libraries at their loader
                // paths, e.g. `lib/x86_64-linux-gnu/libc.so.6`) need
                // their parent directories created first.
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|source| Error::Mount {
                        operation: "mkdir(bind target parents)",
                        source,
                    })?;
                }
                std::fs::write(target, []).map_err(|source| Error::Mount {
                    operation: "touch(bind target)",
                    source,
                })?;
            }
            // The source by its fd: the cell's user may not reach it by
            // its path.
            let by_fd = format!("/proc/self/fd/{}", source.as_raw_fd());
            rustix::mount::mount(
                by_fd.as_str(),
                target,
                "",
                rustix::mount::MountFlags::BIND,
                "",
            )
            .map_err(mount_err("mount(bind)"))?;
            rustix::mount::mount_remount(target, *flags, "")
                .map_err(mount_err("mount(bind,remount)"))?;
        }
        if self.proc {
            std::fs::create_dir("proc").map_err(|source| Error::Mount {
                operation: "mkdir(proc)",
                source,
            })?;
            rustix::mount::mount("proc", "proc", "proc", PROC_FLAGS, "subset=pid")
                .map_err(mount_err("mount(proc)"))?;
        }
        // The root itself goes read-only last: nothing in-cell is
        // writable afterwards.
        rustix::mount::mount_remount(".", staged.root_flags, "")
            .map_err(mount_err("mount(root,remount-ro)"))?;
        rustix::process::pivot_root(".", "old").map_err(|source| Error::Mount {
            operation: "pivot_root",
            source: source.into(),
        })?;
        std::env::set_current_dir("/").map_err(|source| Error::Mount {
            operation: "chdir(/)",
            source,
        })?;
        // Detach the old root: after this the host tree is unreachable
        // from inside the cell.
        // SAFETY: `umount2` on `/old` (a valid NUL-terminated C string
        // literal) with `MNT_DETACH` only detaches the old root mount;
        // the path is known-good because `pivot_root` just placed it.
        let ret = unsafe { libc::umount2(c"/old".as_ptr(), libc::MNT_DETACH) };
        if ret != 0 {
            return Err(Error::Mount {
                operation: "umount2(/old)",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }
}

/// What [`MountPlan::apply`]'s first half opens for its second: each
/// bind's source with the flags its remount keeps, in plan order, and
/// the flags of the root's read-only remount.
struct Staged {
    /// The bind sources, `O_PATH`, and their remount flags.
    sources: Vec<(rustix::fd::OwnedFd, rustix::mount::MountFlags)>,
    /// The root's read-only remount flags.
    root_flags: rustix::mount::MountFlags,
}

/// A mount syscall's failure as [`Error::Mount`], naming the operation.
fn mount_err(operation: &'static str) -> impl Fn(rustix::io::Errno) -> Error {
    move |source| Error::Mount {
        operation,
        source: std::io::Error::from_raw_os_error(source.raw_os_error()),
    }
}

/// Whether `path` is a character device (not following a final symlink).
fn is_char_device(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt as _;
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_char_device())
}

/// A bind target is safe when it is a relative path made only of normal
/// components: no absolute paths, no `.`/`..`, no Windows prefixes.
/// Nested targets are required — shared libraries must land at their
/// loader paths (e.g. `lib/x86_64-linux-gnu/libc.so.6`) — and stay
/// inside the new root because every component is `Normal`.
fn is_safe_target(target: &str) -> bool {
    if target.is_empty() {
        return false;
    }
    let mut any = false;
    for component in Path::new(target).components() {
        if !matches!(component, Component::Normal(_)) {
            return false;
        }
        any = true;
    }
    any
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn allowlist(paths: &[&str]) -> HashSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn bind(source: &str, target: &str, is_dir: bool) -> BindMount {
        BindMount {
            source: PathBuf::from(source),
            target: target.to_owned(),
            is_dir,
            writable: false,
            device: false,
        }
    }

    #[test]
    fn accepts_a_fully_valid_plan() {
        let plan = MountPlan::new(
            PathBuf::from("/tmp/stage"),
            &allowlist(&["/tmp/sock", "/bin/agent"]),
            vec![
                bind("/tmp/sock", "sock", true),
                bind("/bin/agent", "agent", false),
            ],
        )
        .unwrap();
        assert_eq!(plan.new_root(), Path::new("/tmp/stage"));
        assert_eq!(plan.binds().len(), 2);
    }

    #[test]
    fn rejects_a_bind_source_outside_the_allowlist() {
        // The allowlist pin: a plan smuggling /etc into the cell must
        // fail here, before any mount syscall runs.
        let err = MountPlan::new(
            PathBuf::from("/tmp/stage"),
            &allowlist(&["/tmp/sock"]),
            vec![bind("/etc", "evil", true)],
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidMountPlan { .. }));
    }

    #[test]
    fn rejects_a_relative_bind_source() {
        // Relative sources would resolve against the spawner's cwd: an
        // attacker-controlled cwd must not redirect a bind.
        let err = MountPlan::new(
            PathBuf::from("/tmp/stage"),
            &allowlist(&["tmp/sock"]),
            vec![bind("tmp/sock", "sock", true)],
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidMountPlan { .. }));
    }

    #[test]
    fn rejects_a_relative_new_root() {
        let err = MountPlan::new(
            PathBuf::from("relative/stage"),
            &allowlist(&["/tmp/sock"]),
            vec![bind("/tmp/sock", "sock", true)],
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidMountPlan { .. }));
    }

    #[test]
    fn rejects_unsafe_targets() {
        // Targets escape the new root if they are absolute or contain
        // `..` segments: "../escape" or "/abs" would land outside the
        // cell root. Nested normal paths ("a/b") are required for
        // shared libraries at their loader paths. Interior `.`
        // segments are normalized away by `components()` (and by the
        // kernel's path resolution) and cannot escape, so they are
        // accepted — pinned below in `target_safety_is_exact`.
        for target in ["", "../escape", ".", "/abs", "a/../b", ".."] {
            let err = MountPlan::new(
                PathBuf::from("/tmp/stage"),
                &allowlist(&["/tmp/sock"]),
                vec![bind("/tmp/sock", target, true)],
            )
            .unwrap_err();
            assert!(
                matches!(err, Error::InvalidMountPlan { .. }),
                "target {target:?} should be rejected"
            );
        }
    }

    #[test]
    fn accepts_nested_library_targets() {
        // Shared libraries must land at their loader paths inside the
        // cell, e.g. `lib/x86_64-linux-gnu/libc.so.6`.
        let plan = MountPlan::new(
            PathBuf::from("/tmp/stage"),
            &allowlist(&["/lib/x86_64-linux-gnu/libc.so.6"]),
            vec![bind(
                "/lib/x86_64-linux-gnu/libc.so.6",
                "lib/x86_64-linux-gnu/libc.so.6",
                false,
            )],
        )
        .unwrap();
        assert_eq!(
            plan.binds().first().map(|b| b.target.as_str()),
            Some("lib/x86_64-linux-gnu/libc.so.6")
        );
    }

    #[test]
    fn target_safety_is_exact() {
        assert!(is_safe_target("sock"));
        assert!(is_safe_target("agent"));
        assert!(is_safe_target("lib/x86_64-linux-gnu/libc.so.6"));
        // Interior `.` normalizes away in `components()` exactly as
        // the kernel resolves it: `a/./b` is `a/b`, still inside the
        // new root.
        assert!(is_safe_target("a/./b"));
        assert!(!is_safe_target(""));
        assert!(!is_safe_target(".."));
        assert!(!is_safe_target("../escape"));
        assert!(!is_safe_target("/abs"));
    }

    #[test]
    fn mount_flag_composites_pin_the_exact_bits() {
        // The composites are literals (see the constants above); this
        // pins that they equal NOSUID|NODEV and BIND|RDONLY
        // respectively. A wrong literal (e.g. a dropped bit) fails
        // here instead of silently weakening the cell's mount table.
        use rustix::mount::MountFlags;
        assert_eq!(TMPFS_ROOT_FLAGS, MountFlags::NOSUID | MountFlags::NODEV);
        assert_eq!(
            REMOUNT_READONLY_FLAGS,
            MountFlags::BIND | MountFlags::RDONLY
        );
        // And the bits themselves, so a platform with different
        // MS_* values fails loudly instead of mounting wrong.
        assert_eq!(TMPFS_ROOT_FLAGS.bits(), 0x6);
        assert_eq!(REMOUNT_READONLY_FLAGS.bits(), 0x1001);
        // A writable bind keeps NOSUID|NODEV and never carries RDONLY.
        assert_eq!(
            REMOUNT_WRITABLE_FLAGS,
            MountFlags::BIND | MountFlags::NOSUID | MountFlags::NODEV
        );
        assert_eq!(REMOUNT_WRITABLE_FLAGS.bits(), 0x1006);
        // A device keeps NOSUID but never NODEV, which would make it
        // unopenable; the cell's procfs is NOSUID|NODEV|NOEXEC.
        assert_eq!(REMOUNT_DEVICE_FLAGS, MountFlags::BIND | MountFlags::NOSUID);
        assert_eq!(REMOUNT_DEVICE_FLAGS.bits(), 0x1002);
        assert_eq!(
            PROC_FLAGS,
            MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC
        );
        assert_eq!(PROC_FLAGS.bits(), 0xE);
    }

    #[test]
    fn a_device_bind_is_only_ever_a_character_device() {
        let device = |source: &str| BindMount {
            device: true,
            writable: true,
            ..bind(source, "dev/x", false)
        };
        let allowed = allowlist(&["/dev/null", "/etc/passwd", "/dev"]);
        let plan = MountPlan::new(
            PathBuf::from("/tmp/root"),
            &allowed,
            vec![device("/dev/null")],
        )
        .unwrap();
        assert_eq!(plan.binds().len(), 1);
        assert!(!plan.has_proc(), "no procfs unless asked");
        assert!(plan.with_proc().has_proc());
        for source in ["/etc/passwd", "/dev"] {
            assert!(
                matches!(
                    MountPlan::new(PathBuf::from("/tmp/root"), &allowed, vec![device(source)]),
                    Err(Error::InvalidMountPlan { .. })
                ),
                "{source} is no character device"
            );
        }
        assert!(is_char_device(Path::new("/dev/null")));
        assert!(!is_char_device(Path::new("/nonexistent/device")));
    }

    #[test]
    fn remount_flags_retain_source_mount_flags() {
        // The mapping is verified against the live filesystem:
        // whatever flag state statvfs reports for a path, the remount
        // flags must repeat it, on top of BIND|RDONLY. A mutant
        // dropping any retention would remount EPERM on mounts with
        // locked flags (e.g. a nosuid,nodev /tmp, or a noexec or
        // noatime source) — the kernel refuses to clear a locked flag.
        use rustix::fs::StatVfsMountFlags as Vf;
        use rustix::mount::MountFlags as Mf;
        let dir = std::env::temp_dir();
        let flags = remount_flags_for(&dir, REMOUNT_READONLY_FLAGS);
        assert!(flags.contains(Mf::BIND | Mf::RDONLY));
        let writable = remount_flags_for(&dir, REMOUNT_WRITABLE_FLAGS);
        assert!(writable.contains(Mf::BIND | Mf::NOSUID | Mf::NODEV));
        assert!(
            !writable.contains(Mf::RDONLY),
            "a writable bind is never remounted read-only"
        );
        let vfs = rustix::fs::statvfs(&dir).expect("statvfs(temp_dir)");
        for (vf, mf, name) in [
            (Vf::NOSUID, Mf::NOSUID, "NOSUID"),
            (Vf::NODEV, Mf::NODEV, "NODEV"),
            (Vf::NOEXEC, Mf::NOEXEC, "NOEXEC"),
            (Vf::SYNCHRONOUS, Mf::SYNCHRONOUS, "SYNCHRONOUS"),
            (Vf::MANDLOCK, Mf::PERMIT_MANDATORY_FILE_LOCKING, "MANDLOCK"),
            (Vf::NOATIME, Mf::NOATIME, "NOATIME"),
            (Vf::NODIRATIME, Mf::NODIRATIME, "NODIRATIME"),
            (Vf::RELATIME, Mf::RELATIME, "RELATIME"),
        ] {
            assert_eq!(
                flags.contains(mf),
                vfs.f_flag.contains(vf),
                "{name} must be retained from the source mount"
            );
        }
        // Pin retention against a mount whose flags are fixed by the
        // kernel: /proc is always nosuid,nodev,noexec. The temp-dir
        // loop above only exercises the loop body when the local
        // mount actually sets flags — on a bare ext4 TMPDIR it never
        // runs, and a mutant breaking the retention (`|=` -> `&=`)
        // is missed.
        let proc_flags = remount_flags_for(Path::new("/proc"), REMOUNT_READONLY_FLAGS);
        assert!(
            proc_flags.contains(Mf::BIND | Mf::RDONLY),
            "remount must always add BIND|RDONLY"
        );
        for (mf, name) in [
            (Mf::NOSUID, "NOSUID"),
            (Mf::NODEV, "NODEV"),
            (Mf::NOEXEC, "NOEXEC"),
        ] {
            assert!(
                proc_flags.contains(mf),
                "/proc is always {name}: retention must carry it into the remount flags"
            );
        }
    }

    #[test]
    fn apply_actually_pivots_into_the_new_root() {
        // Mutants: apply replaced with Ok(()), or the umount2
        // `ret != 0` check inverted to `==`. Fork a child into a new
        // mount namespace, apply a minimal plan (no binds), and verify
        // the root actually changed: a marker file written in the
        // staging dir must appear at /marker, and /old must be gone
        // (detached). If apply is a no-op, neither holds.
        use crate::process::{ForkResult, child_exit_code, exit_child, fork};
        use std::time::Duration;

        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // Read euid BEFORE unsharing the user namespace: in a
                // fresh userns with no maps yet, geteuid() returns the
                // overflow uid 65534, so the id map below would pin to
                // 65534 and write_maps would fail EPERM for an
                // unprivileged broker (mirrors the cell probe in
                // hatchling-broker's cell_boundary.rs, which also reads
                // euid first).
                // SAFETY: geteuid takes no args.
                let euid = unsafe { libc::geteuid() };
                // Read now: once in its own user namespace, unmapped, it
                // reads as the overflow gid.
                let own_gid = crate::process::egid();
                // Unshare the user namespace first so the mount
                // namespace unshare works unprivileged too (mirrors
                // the cell probe in hatchling-broker's
                // cell_boundary.rs). Without it, non-root runs always
                // take the exit-99 skip and pipeline mutants are
                // missed.
                // SAFETY: unshare(CLONE_NEWUSER) in the
                // single-threaded child.
                if unsafe { libc::unshare(libc::CLONE_NEWUSER) } != 0 {
                    exit_child(99);
                }
                // A process can map only its own uid into a namespace it
                // just made, root included (a root broker's map is
                // written by the spawn's parent, outside it). The plan's
                // mechanics are the same either way, so the test maps
                // its own.
                let base = euid;
                let Ok(map) = crate::idmap::IdMap::new(base, 1) else {
                    exit_child(99)
                };
                // And its own gid, which needn't be the uid's number.
                let Ok(group_map) = crate::idmap::IdMap::new(own_gid, 1) else {
                    exit_child(99)
                };
                // SAFETY: getpid takes no args.
                let self_pid = unsafe { libc::getpid() };
                if crate::idmap::write_maps(self_pid, &map, &group_map).is_err() {
                    exit_child(99);
                }
                // SAFETY: unshare(CLONE_NEWNS) in the single-threaded
                // child isolates our mounts from the test runner.
                if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
                    exit_child(99);
                }
                // Make propagation private so mounts don't leak out.
                // SAFETY: remounting / as MS_PRIVATE|MS_REC only
                // changes propagation in our namespace.
                if unsafe {
                    libc::mount(
                        std::ptr::null(),
                        c"/".as_ptr(),
                        std::ptr::null(),
                        libc::MS_PRIVATE | libc::MS_REC,
                        std::ptr::null(),
                    )
                } != 0
                {
                    exit_child(99);
                }
                // SAFETY: getpid takes no args.
                let pid = unsafe { libc::getpid() };
                let stage = std::env::temp_dir().join(format!("hatchling-mount-test-{pid}"));
                if std::fs::create_dir_all(&stage).is_err() {
                    exit_child(99);
                }
                if std::fs::write(stage.join("marker"), b"cell").is_err() {
                    exit_child(99);
                }
                let allowlist = HashSet::new();
                let Ok(plan) = MountPlan::new(stage.clone(), &allowlist, vec![]) else {
                    exit_child(99)
                };
                // Exit 2 if apply itself fails: distinguishes "apply
                // errored" from "apply was a no-op".
                if plan.apply().is_err() {
                    exit_child(2);
                }
                // Verify: / must now be a tmpfs (the fresh root), not
                // the host's filesystem. If apply was a no-op, statfs
                // reports the original filesystem type.
                // SAFETY: statfs on "/" with a valid out pointer; the
                // child is single-threaded after fork here.
                let mut st: libc::statfs = unsafe { std::mem::zeroed() };
                // SAFETY: st is a valid zeroed statfs; c"/" is a valid
                // NUL-terminated path; the child is single-threaded
                // after fork here.
                let statfs_ok = unsafe { libc::statfs(c"/".as_ptr(), &raw mut st) } != 0;
                if statfs_ok {
                    exit_child(3);
                }
                // TMPFS_MAGIC: f_type is __fsword_t (i64 on 64-bit).
                if st.f_type != 0x0102_1994_i64 {
                    exit_child(4);
                }
                // And /old must be detached: after umount2 the mountpoint
                // is an empty directory. If the old root were still
                // mounted there, it would contain the host's files
                // (e.g. /old/bin). An empty /old proves detachment.
                let old_empty = std::fs::read_dir("/old").map_or(true, |mut d| d.next().is_none());
                if !old_empty {
                    exit_child(5);
                }
                // Best-effort cleanup of the staging dir (now detached).
                let _ = std::fs::remove_dir_all(&stage);
                exit_child(0);
            }
        };
        // The child creates its staging dir on the host before it pivots
        // away from it, so its own cleanup above can no longer reach it:
        // the parent removes it once the test ends, pass or fail
        // . The child has no pid namespace, so its getpid is
        // this pid.
        let _stage = StageDir(
            std::env::temp_dir().join(format!("hatchling-mount-test-{}", child.pid().get())),
        );
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        let code = child_exit_code(status);
        if code == 99 {
            // Environmental: cannot create namespaces here. Under
            // HATCHLING_REQUIRE_CELL=1 the skip becomes a hard
            // failure: a broken pipeline must not silently disable
            // the test that guards it.
            let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1");
            assert!(
                !required,
                "HATCHLING_REQUIRE_CELL=1 but namespaces are unavailable: refusing to skip"
            );
            return;
        }
        assert_eq!(
            code, 0,
            "apply must actually pivot into the new root (got exit {code})"
        );
    }

    #[test]
    fn a_mount_failure_names_its_operation_and_the_kernels_code() {
        let error = mount_err("mount(proc)")(rustix::io::Errno::ACCESS);
        assert!(matches!(
            &error,
            Error::Mount { operation: "mount(proc)", source } if source.raw_os_error() == Some(13)
        ));
    }

    #[test]
    fn staging_mounts_the_cells_root_opens_each_source_and_steps_in() {
        // The plan's first half, in a child with its own user and
        // mount namespaces: the tmpfs is on the new root and is the cell
        // user's, each bind's source is open with its read-only remount
        // flags, and the working directory is the new root.
        use std::os::unix::fs::MetadataExt as _;

        use crate::process::{ForkResult, child_exit_code, exit_child, fork};
        use std::time::Duration;

        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let self_pid = enter_private_mount_namespace();
                let stage = std::env::temp_dir().join(format!("hatchling-mount-stage-{self_pid}"));
                let source =
                    std::env::temp_dir().join(format!("hatchling-mount-stage-src-{self_pid}"));
                if std::fs::create_dir_all(&stage).is_err()
                    || std::fs::create_dir_all(&source).is_err()
                {
                    exit_child(99);
                }
                let allowlist: HashSet<PathBuf> = std::iter::once(source.clone()).collect();
                let binds = vec![BindMount {
                    source: source.clone(),
                    target: String::from("src"),
                    is_dir: true,
                    writable: false,
                    device: false,
                }];
                let Ok(plan) = MountPlan::new(stage.clone(), &allowlist, binds) else {
                    exit_child(99)
                };
                let Ok(staged) = plan.stage() else {
                    exit_child(2)
                };
                let readonly = rustix::mount::MountFlags::RDONLY;
                let [(opened, flags)] = staged.sources.as_slice() else {
                    exit_child(3)
                };
                if !flags.contains(readonly) || !staged.root_flags.contains(readonly) {
                    exit_child(4);
                }
                let same = rustix::fs::fstat(opened)
                    .ok()
                    .zip(std::fs::metadata(&source).ok());
                if same.is_none_or(|(open, path)| open.st_ino != path.ino()) {
                    exit_child(5);
                }
                if std::env::current_dir().ok() != Some(stage) {
                    exit_child(6);
                }
                // TMPFS_MAGIC, owned by in-cell uid 0.
                let tmpfs = rustix::fs::statfs(".").is_ok_and(|fs| fs.f_type == 0x0102_1994_i64);
                if !tmpfs || std::fs::metadata(".").map(|meta| meta.uid()).ok() != Some(0) {
                    exit_child(7);
                }
                let _ = std::fs::remove_dir_all(&source);
                exit_child(0);
            }
        };
        let pid = child.pid().get();
        let _stage = StageDir(std::env::temp_dir().join(format!("hatchling-mount-stage-{pid}")));
        let _source =
            StageDir(std::env::temp_dir().join(format!("hatchling-mount-stage-src-{pid}")));
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        let code = child_exit_code(status);
        if code == 99 {
            let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1");
            assert!(
                !required,
                "HATCHLING_REQUIRE_CELL=1 but namespaces are unavailable: refusing to skip"
            );
            return;
        }
        assert_eq!(code, 0, "staging failed at step {code}");
    }

    #[test]
    fn a_writable_bind_takes_writes_and_a_read_only_one_refuses_them() {
        // Mutants: the writable choice inverted or dropped. A child in its
        // own user and mount namespaces binds one host dir read-only at
        // /ro and one writable at /rw, then writes to both. The write to
        // /rw must land on the host; the write to /ro must fail; /rw must
        // still be nosuid,nodev.
        use crate::process::{ForkResult, child_exit_code, exit_child, fork};
        use std::time::Duration;

        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let self_pid = enter_private_mount_namespace();
                let stage = std::env::temp_dir().join(format!("hatchling-mount-rw-{self_pid}"));
                let sources =
                    std::env::temp_dir().join(format!("hatchling-mount-rw-src-{self_pid}"));
                let (ro, rw) = (sources.join("ro"), sources.join("rw"));
                for dir in [&stage, &ro, &rw] {
                    if std::fs::create_dir_all(dir).is_err() {
                        exit_child(99);
                    }
                }
                let allowlist: HashSet<PathBuf> = [ro.clone(), rw.clone()].into_iter().collect();
                let binds = vec![
                    BindMount {
                        source: ro,
                        target: String::from("ro"),
                        is_dir: true,
                        writable: false,
                        device: false,
                    },
                    BindMount {
                        source: rw,
                        target: String::from("rw"),
                        is_dir: true,
                        writable: true,
                        device: false,
                    },
                ];
                let Ok(plan) = MountPlan::new(stage, &allowlist, binds) else {
                    exit_child(99)
                };
                if plan.apply().is_err() {
                    exit_child(2);
                }
                if std::fs::write("/rw/note", b"written in the cell").is_err() {
                    exit_child(3);
                }
                if std::fs::write("/ro/note", b"must not land").is_ok() {
                    exit_child(4);
                }
                let Ok(vfs) = rustix::fs::statvfs("/rw") else {
                    exit_child(5)
                };
                let flags = vfs.f_flag;
                if !flags.contains(rustix::fs::StatVfsMountFlags::NOSUID)
                    || !flags.contains(rustix::fs::StatVfsMountFlags::NODEV)
                {
                    exit_child(6);
                }
                exit_child(0);
            }
        };
        let pid = child.pid().get();
        let _stage = StageDir(std::env::temp_dir().join(format!("hatchling-mount-rw-{pid}")));
        let sources = std::env::temp_dir().join(format!("hatchling-mount-rw-src-{pid}"));
        let _sources = StageDir(sources.clone());
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        let code = child_exit_code(status);
        if code == 99 {
            let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1");
            assert!(
                !required,
                "HATCHLING_REQUIRE_CELL=1 but namespaces are unavailable: refusing to skip"
            );
            return;
        }
        assert_eq!(
            code, 0,
            "the binds must take and refuse writes as planned (got exit {code})"
        );
        assert_eq!(
            std::fs::read(sources.join("rw").join("note")).unwrap(),
            b"written in the cell"
        );
        assert!(!sources.join("ro").join("note").exists());
    }

    /// In a forked child: a fresh user namespace mapping this uid, and a
    /// private mount namespace. Returns the child's pid; exits 99 (the
    /// environmental skip) when namespaces are unavailable.
    #[allow(
        clippy::redundant_pub_crate,
        reason = "the tree module's tests take a mount namespace this way too"
    )]
    pub(crate) fn enter_private_mount_namespace() -> i32 {
        use crate::process::exit_child;
        // SAFETY: geteuid takes no args.
        let euid = unsafe { libc::geteuid() };
        // Read now, before the namespace (see above).
        let own_gid = crate::process::egid();
        // SAFETY: unshare(CLONE_NEWUSER) in the single-threaded
        // child.
        if unsafe { libc::unshare(libc::CLONE_NEWUSER) } != 0 {
            exit_child(99);
        }
        // Its own uid and gid: the only maps it may write (see above).
        let Ok(map) = crate::idmap::IdMap::new(euid, 1) else {
            exit_child(99)
        };
        let Ok(group_map) = crate::idmap::IdMap::new(own_gid, 1) else {
            exit_child(99)
        };
        // SAFETY: getpid takes no args.
        let self_pid = unsafe { libc::getpid() };
        if crate::idmap::write_maps(self_pid, &map, &group_map).is_err() {
            exit_child(99);
        }
        // SAFETY: unshare(CLONE_NEWNS) in the single-threaded
        // child isolates our mounts from the test runner.
        if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
            exit_child(99);
        }
        // SAFETY: remounting / as MS_PRIVATE|MS_REC only
        // changes propagation in our namespace.
        if unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_PRIVATE | libc::MS_REC,
                std::ptr::null(),
            )
        } != 0
        {
            exit_child(99);
        }
        self_pid
    }

    /// A forked child's staging dir, removed on drop.
    struct StageDir(PathBuf);

    impl Drop for StageDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
