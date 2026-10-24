//! Landlock: a process confining its own file access (WO-017, slice 4).
//!
//! A process builds a ruleset naming the directories and files it may
//! use and how, then restricts itself to it, for good: no capability is
//! needed, and nothing it can do afterwards widens it. The broker does
//! this once its start is done, so a compromised broker reaches its own
//! state and the system's read-only paths, never the rest of its user's
//! files: without root, it runs as its owner, whose home it would
//! otherwise read.
//!
//! Every right the running kernel's Landlock knows is handled, so any
//! file access the rules don't grant is refused. A kernel without
//! Landlock (before 5.13, or with it off) is said so: [`Confinement::new`]
//! answers none.

use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::Path;

use crate::Error;

/// `landlock_create_ruleset`'s flag asking for the ABI version.
const CREATE_RULESET_VERSION: libc::c_uint = 1;

/// `LANDLOCK_RULE_PATH_BENEATH`.
const RULE_PATH_BENEATH: libc::c_int = 1;

/// `LANDLOCK_ACCESS_FS_EXECUTE`.
const EXECUTE: u64 = 0x1;

/// The rights that apply to a file rather than a directory: `EXECUTE`
/// 0x1, `WRITE_FILE` 0x2, `READ_FILE` 0x4, `TRUNCATE` 0x4000 and
/// `IOCTL_DEV` 0x8000. Written as its value: built with `|`, each `|` ->
/// `^` would be the same number.
const FILE_RIGHTS: u64 = 0xc007;

/// Reading: `READ_FILE` 0x4 and `READ_DIR` 0x8.
const READ: u64 = 0xc;

/// The filesystem rights Landlock ABI `abi` knows: version 1 the first
/// thirteen (0x1fff), 2 adds `REFER` (0x2000), 3 `TRUNCATE` (0x4000),
/// and 5 `IOCTL_DEV` (0x8000); 4 added network rights only.
const fn handled_for(abi: u32) -> u64 {
    match abi {
        0 => 0,
        1 => 0x1fff,
        2 => 0x3fff,
        3 | 4 => 0x7fff,
        _ => 0xffff,
    }
}

/// `struct landlock_ruleset_attr`, as far as files go: a kernel takes a
/// shorter one than its own.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

/// `struct landlock_path_beneath_attr`, which the kernel packs.
#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// How a path may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read: its files read, its directories listed.
    Read,
    /// Everything but running a program: read, write, make, remove and
    /// rename what is beneath.
    ReadWrite,
}

/// The Landlock ABI this kernel speaks, if it has Landlock on.
#[must_use]
pub fn landlock_abi() -> Option<u32> {
    // SAFETY: with no attribute and the version flag, the call only
    // answers the ABI version; it reads and writes no memory.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0_usize,
            CREATE_RULESET_VERSION,
        )
    };
    // A version from 1 up, or -1.
    u32::try_from(abi).ok()
}

/// A ruleset being built: the paths a process will be held to.
#[derive(Debug)]
pub struct Confinement {
    ruleset: OwnedFd,
    handled: u64,
}

impl Confinement {
    /// A ruleset handling every file right this kernel's Landlock knows;
    /// none when it has no Landlock.
    ///
    /// # Errors
    ///
    /// [`Error::Process`] when the kernel has Landlock but refuses the
    /// ruleset.
    pub fn new() -> Result<Option<Self>, Error> {
        let Some(abi) = landlock_abi() else {
            return Ok(None);
        };
        let handled = handled_for(abi);
        let attr = RulesetAttr {
            handled_access_fs: handled,
        };
        // SAFETY: the attribute is valid for the call, and its size is
        // passed with it; the kernel answers a new descriptor or -1.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::from_ref(&attr),
                std::mem::size_of::<RulesetAttr>(),
                0_u32,
            )
        };
        // A new descriptor, or -1.
        let fd = i32::try_from(fd)
            .ok()
            .filter(|&fd| fd != -1)
            .ok_or_else(|| failed("landlock_create_ruleset"))?;
        // SAFETY: the kernel just made this descriptor; nothing else
        // owns it.
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Some(Self { ruleset, handled }))
    }

    /// Lets the process use `path`, and what is beneath it, as `access`
    /// says. A file is given only the rights a file has. Whether it was:
    /// a path that doesn't exist is left out.
    ///
    /// # Errors
    ///
    /// [`Error::Process`] when it exists but can't be opened or looked
    /// at, or the kernel refuses the rule.
    pub fn allow(&mut self, path: &Path, access: Access) -> Result<bool, Error> {
        let opened = match rustix::fs::open(
            path,
            rustix::fs::OFlags::PATH.union(rustix::fs::OFlags::CLOEXEC),
            rustix::fs::Mode::empty(),
        ) {
            Ok(opened) => opened,
            Err(rustix::io::Errno::NOENT) => return Ok(false),
            Err(errno) => {
                return Err(Error::Process {
                    operation: "open a path for landlock",
                    source: errno.into(),
                });
            }
        };
        let stat = rustix::fs::fstat(&opened).map_err(|errno| Error::Process {
            operation: "fstat a path for landlock",
            source: errno.into(),
        })?;
        let wanted = match access {
            Access::Read => READ,
            Access::ReadWrite => !EXECUTE,
        };
        let is_dir =
            rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Directory;
        let rights = if is_dir { wanted } else { wanted & FILE_RIGHTS };
        let attr = PathBeneathAttr {
            allowed_access: rights & self.handled,
            parent_fd: opened.as_raw_fd(),
        };
        // SAFETY: the ruleset and the path's descriptor are open, and the
        // attribute is valid for the call.
        let added = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                self.ruleset.as_raw_fd(),
                RULE_PATH_BENEATH,
                std::ptr::from_ref(&attr),
                0_u32,
            )
        };
        if added != 0 {
            return Err(failed("landlock_add_rule"));
        }
        Ok(true)
    }

    /// Holds the calling thread, and every thread it starts from now on,
    /// to the ruleset, for good. No exec may gain privileges again
    /// (`PR_SET_NO_NEW_PRIVS`, which Landlock requires of a process that
    /// isn't root's peer). Call it while the process has one thread: one
    /// already running stays as it was.
    ///
    /// # Errors
    ///
    /// [`Error::Process`] when either step is refused.
    pub fn restrict_self(self) -> Result<(), Error> {
        crate::set_no_new_privs()?;
        // SAFETY: the ruleset is open; the call changes only the calling
        // thread's credentials.
        let restricted = unsafe {
            libc::syscall(
                libc::SYS_landlock_restrict_self,
                self.ruleset.as_raw_fd(),
                0_u32,
            )
        };
        if restricted != 0 {
            return Err(failed("landlock_restrict_self"));
        }
        Ok(())
    }
}

/// The last OS error, for `operation`.
fn failed(operation: &'static str) -> Error {
    Error::Process {
        operation,
        source: io::Error::last_os_error(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ForkResult, child_exit_code, exit_child, fork};

    /// Each ABI handles the rights it knows, never fewer than the last;
    /// a file is given only file rights, and reading is files and
    /// listing alone.
    #[test]
    fn each_abi_handles_the_rights_it_knows() {
        let known = [0, 0x1fff, 0x3fff, 0x7fff, 0x7fff, 0xffff, 0xffff];
        for (abi, rights) in known.into_iter().enumerate() {
            assert_eq!(handled_for(u32::try_from(abi).unwrap()), rights, "abi {abi}");
        }
        assert_eq!(FILE_RIGHTS, 0x1 + 0x2 + 0x4 + 0x4000 + 0x8000);
        assert_eq!(READ, 0x4 + 0x8);
        assert_eq!(EXECUTE, 1);
    }

    /// Confined, a process reads and writes where it may, reads only where
    /// it may only read, and reaches nothing else, not even what its user
    /// could. (In a child: a confinement is for good.) Without Landlock,
    /// there is no ruleset to build.
    #[test]
    fn a_confined_process_reaches_only_what_it_was_given() {
        use std::time::Duration;
        let Some(_) = landlock_abi() else {
            assert!(Confinement::new().unwrap().is_none());
            assert!(
                std::env::var_os("HATCHLING_REQUIRE_CELL").is_none(),
                "this kernel has no Landlock"
            );
            return;
        };
        let base = std::env::temp_dir().join(format!("hatchling-landlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (own, seen, other) = (base.join("own"), base.join("seen"), base.join("other"));
        for dir in [&own, &seen, &other] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("file"), b"x").unwrap();
        }
        let log = base.join("log");
        std::fs::write(&log, b"").unwrap();
        match fork().unwrap() {
            ForkResult::Parent { child } => {
                let status = child.wait_deadline(Duration::from_secs(10)).unwrap();
                let code = child_exit_code(status);
                std::fs::remove_dir_all(&base).unwrap();
                assert_eq!(
                    code, 0,
                    "1: not confined; 2: can't write its own; 3: can't read what it sees; \
                     4: wrote what it only sees; 5: read what it wasn't given; \
                     6: can't append its file; 7: read beside its file"
                );
            }
            ForkResult::Child => {
                let confined = Confinement::new()
                    .ok()
                    .flatten()
                    .and_then(|mut rules| {
                        let given = rules.allow(&own, Access::ReadWrite).ok()?
                            && rules.allow(&seen, Access::Read).ok()?
                            && rules.allow(&log, Access::ReadWrite).ok()?
                            && !rules.allow(&base.join("absent"), Access::Read).ok()?;
                        given.then_some(rules)
                    })
                    .map(Confinement::restrict_self);
                if !matches!(confined, Some(Ok(()))) {
                    exit_child(1);
                }
                if std::fs::write(own.join("new"), b"y").is_err() {
                    exit_child(2);
                }
                if std::fs::read(seen.join("file")).is_err() {
                    exit_child(3);
                }
                if std::fs::write(seen.join("file"), b"y").is_ok() {
                    exit_child(4);
                }
                if std::fs::read(other.join("file")).is_ok() {
                    exit_child(5);
                }
                let appended = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&log)
                    .and_then(|mut file| std::io::Write::write_all(&mut file, b"z"));
                if appended.is_err() {
                    exit_child(6);
                }
                if std::fs::read_dir(&base).is_ok() {
                    exit_child(7);
                }
                exit_child(0);
            }
        }
    }
}
