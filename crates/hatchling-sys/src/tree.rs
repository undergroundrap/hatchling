//! Removing a disposable directory tree, and nothing outside it (WO-012).
//!
//! The broker removes its own leftovers, a dead cell's staging directory
//! or a killed run's directory, as its own user: root, for a root broker,
//! in directories others may write to. So nothing here resolves a path
//! twice. Each directory is opened from its parent's handle without
//! following a symlink, and each entry is unlinked through the handle it
//! was read from:
//!
//! - a symlink is removed, never followed;
//! - a directory on another mount (a bind mount included, by its mount
//!   id) is never entered, and stays with everything above it;
//! - a directory the caller keeps, by its device and inode (a state
//!   directory), is left whole, with everything above it;
//! - a directory swapped for another between being looked at and opened
//!   is left alone;
//! - no directory is entered that the walk is already in, or above: by
//!   its identity, apart from the `.` and `..` names skipped, so the walk
//!   can't climb out of the tree even if one of the two checks fails;
//! - the tree must be this process's user's, and only that user's
//!   directories are entered: one another user owns, renamed into the
//!   tree, is left whole, so a root broker can't be steered into deleting
//!   what that user couldn't.

use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, StatxFlags};

use crate::Error;

/// How deep [`remove_tree`] goes; anything below is kept.
pub const MAX_DEPTH: usize = 64;

/// How a directory is opened: itself, never a symlink to one.
const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// A directory's identity: its device and inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirId {
    /// The device, as `stat` gives it.
    pub dev: u64,
    /// The inode.
    pub ino: u64,
}

impl DirId {
    /// The directory at `path`, reached as a configured state directory
    /// is, symlinks followed; `None` if no directory is there.
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        rustix::fs::stat(path)
            .ok()
            .filter(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
            .map(|stat| Self {
                dev: stat.st_dev,
                ino: stat.st_ino,
            })
    }
}

/// What [`remove_tree`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removed {
    /// The whole tree is gone.
    Whole,
    /// Some of it is kept: a directory kept by the caller, a mount, a
    /// tree too deep, or an entry that couldn't be removed; and every
    /// directory above it. The rest is gone.
    Partly,
}

/// An [`Error::Removal`] for `operation`, from a rustix error.
fn failed(operation: &'static str) -> impl Fn(rustix::io::Errno) -> Error {
    move |source| Error::Removal {
        operation,
        source: io::Error::from_raw_os_error(source.raw_os_error()),
    }
}

/// What one look at an entry finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Look {
    /// A directory itself (not a symlink to one).
    directory: bool,
    /// Its identity.
    id: DirId,
    /// Its mount's id, when the kernel gives one; else its device.
    mount: u64,
    /// Its owner.
    uid: u32,
}

/// What a walk keeps to: its mount, the directories it leaves whole, and
/// the one user whose directories it enters.
#[derive(Debug, Clone, Copy)]
struct Walk<'a> {
    mount: u64,
    keep: &'a [DirId],
    owner: u32,
}

/// Looks at `name` in `dir` (or at `dir` itself, for an empty name),
/// following no symlink.
fn look(dir: impl AsFd, name: &CStr) -> Result<Look, rustix::io::Errno> {
    let flags = if name.is_empty() {
        AtFlags::EMPTY_PATH | AtFlags::SYMLINK_NOFOLLOW
    } else {
        AtFlags::SYMLINK_NOFOLLOW
    };
    let stat = rustix::fs::statx(
        dir,
        name,
        flags,
        StatxFlags::TYPE | StatxFlags::INO | StatxFlags::MNT_ID | StatxFlags::UID,
    )?;
    let dev = rustix::fs::makedev(stat.stx_dev_major, stat.stx_dev_minor);
    let mount = if StatxFlags::from_bits_retain(stat.stx_mask).contains(StatxFlags::MNT_ID) {
        stat.stx_mnt_id
    } else {
        dev
    };
    Ok(Look {
        directory: FileType::from_raw_mode(u32::from(stat.stx_mode)) == FileType::Directory,
        id: DirId {
            dev,
            ino: stat.stx_ino,
        },
        mount,
        uid: stat.stx_uid,
    })
}

/// Removes the directory `name` in `root`, and everything in it but what
/// it keeps (see the module docs). `keep` names the directories to leave
/// whole.
///
/// # Errors
///
/// Returns [`Error::Removal`] if `name` isn't one plain name (`.`, `..`
/// and a path are refused), if `root` can't be opened as a directory, if
/// `name` isn't a directory itself (a symlink to one included), if it is
/// on another mount than `root`, or if it isn't this process's user's;
/// nothing is removed then.
pub fn remove_tree(root: &Path, name: &OsStr, keep: &[DirId]) -> Result<Removed, Error> {
    let bytes = name.as_encoded_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(Error::Removal {
            operation: "name the tree",
            source: io::Error::new(io::ErrorKind::InvalidInput, "not one plain name"),
        });
    }
    let parent =
        rustix::fs::open(root, DIR_FLAGS, Mode::empty()).map_err(failed("open the root"))?;
    let base = look(&parent, c"").map_err(failed("look at the root"))?;
    let name = CString::new(name.as_encoded_bytes()).map_err(|_| Error::Removal {
        operation: "name the tree",
        source: io::Error::new(io::ErrorKind::InvalidInput, "a NUL in its name"),
    })?;
    let top = rustix::fs::openat(&parent, &name, DIR_FLAGS, Mode::empty())
        .map_err(failed("open the tree"))?;
    let found = look(&top, c"").map_err(failed("look at the tree"))?;
    if found.mount != base.mount {
        return Err(Error::Removal {
            operation: "enter the tree",
            source: io::Error::new(io::ErrorKind::PermissionDenied, "it is another mount"),
        });
    }
    let owner = rustix::process::geteuid().as_raw();
    if found.uid != owner {
        return Err(Error::Removal {
            operation: "enter the tree",
            source: io::Error::new(io::ErrorKind::PermissionDenied, "it isn't this user's"),
        });
    }
    if keep.contains(&found.id) {
        return Ok(Removed::Partly);
    }
    let walk = Walk {
        mount: base.mount,
        keep,
        owner,
    };
    let whole = empty(&top, walk, &[base.id, found.id]);
    drop(top);
    if whole && rustix::fs::unlinkat(&parent, &name, AtFlags::REMOVEDIR).is_ok() {
        Ok(Removed::Whole)
    } else {
        Ok(Removed::Partly)
    }
}

/// Removes the directory at `path` through [`remove_tree`], keeping
/// nothing.
///
/// For a disposable directory its caller made, such as a cell's staging
/// or a run's directory. Its parent is the root, so a mount in it (should
/// one ever show through) is never entered, and nothing outside it goes
/// (WO-024).
///
/// # Errors
///
/// As [`remove_tree`]; and [`Error::Removal`] if `path` has no parent and
/// final name.
pub fn remove_dir_tree(path: &Path) -> Result<Removed, Error> {
    let (Some(root), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(Error::Removal {
            operation: "name the tree",
            source: io::Error::new(io::ErrorKind::InvalidInput, "no parent and name"),
        });
    };
    remove_tree(root, name, &[])
}

/// Removes what it may of everything in `dir`, as `walk` keeps to;
/// whether all of it is gone. `above` is every directory the walk is in,
/// from the root down to `dir`.
fn empty(dir: &OwnedFd, walk: Walk<'_>, above: &[DirId]) -> bool {
    let Ok(entries) = rustix::fs::Dir::read_from(dir) else {
        return false;
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let name = entry.file_name();
        if name != c"." && name != c".." {
            names.push(name.to_owned());
        }
    }
    // Every entry is tried, whatever came before it.
    let mut whole = true;
    for name in &names {
        whole &= remove_entry(dir, name, walk, above);
    }
    whole
}

/// Removes `name` in `dir`, a directory with what it may of its
/// contents; whether it is gone. `above` is as for [`empty`]: a directory
/// in it (`.`, `..`, or any path back up) is never entered.
fn remove_entry(dir: &OwnedFd, name: &CStr, walk: Walk<'_>, above: &[DirId]) -> bool {
    let Ok(seen) = look(dir, name) else {
        return false;
    };
    if !seen.directory {
        return rustix::fs::unlinkat(dir, name, AtFlags::empty()).is_ok();
    }
    if above.contains(&seen.id) {
        return false;
    }
    if seen.mount != walk.mount || walk.keep.contains(&seen.id) || above.len() > MAX_DEPTH {
        return false;
    }
    if seen.uid != walk.owner {
        return false;
    }
    let Ok(sub) = rustix::fs::openat(dir, name, DIR_FLAGS, Mode::empty()) else {
        return false;
    };
    if look(&sub, c"").map(|opened| opened.id) != Ok(seen.id) {
        return false;
    }
    let deeper: Vec<DirId> = above.iter().copied().chain([seen.id]).collect();
    let whole = empty(&sub, walk, &deeper);
    drop(sub);
    whole && rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR).is_ok()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    use super::*;

    /// A scratch directory, gone when dropped.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("hatchling-tree-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("root")).unwrap();
        std::fs::create_dir_all(dir.join("outside").join("dir")).unwrap();
        std::fs::write(dir.join("outside").join("file"), b"keep me").unwrap();
        std::fs::write(dir.join("outside").join("dir").join("inner"), b"me too").unwrap();
        Scratch(dir)
    }

    /// A tree with files, nested directories, and symlinks out of it.
    fn plant(tree: &Path, outside: &Path) {
        std::fs::create_dir_all(tree.join("a").join("b")).unwrap();
        std::fs::write(tree.join("file"), b"x").unwrap();
        std::fs::write(tree.join("a").join("b").join("deep"), b"y").unwrap();
        symlink(outside.join("file"), tree.join("to-file")).unwrap();
        symlink(outside.join("dir"), tree.join("a").join("to-dir")).unwrap();
        symlink("/nonexistent", tree.join("dangling")).unwrap();
    }

    #[test]
    fn a_directory_goes_by_its_path_and_nothing_outside_it() {
        let dir = scratch("by-path");
        let (root, outside) = (dir.0.join("root"), dir.0.join("outside"));
        let tree = root.join("tree");
        plant(&tree, &outside);
        assert!(matches!(remove_dir_tree(&tree), Ok(Removed::Whole)));
        assert!(!tree.exists() && root.exists() && intact(&outside));
        assert!(remove_dir_tree(Path::new("/")).is_err(), "no parent");
        assert!(remove_dir_tree(&root.join("missing")).is_err());
        // Bounded like the walk it goes through: a tree deeper than
        // MAX_DEPTH is removed only partly.
        let deep = root.join("deep");
        std::fs::create_dir_all(deeper_than_the_walk(&deep)).unwrap();
        assert!(matches!(remove_dir_tree(&deep), Ok(Removed::Partly)));
        assert!(deep.exists());
    }

    /// A path `MAX_DEPTH + 2` directories below `top`.
    fn deeper_than_the_walk(top: &Path) -> PathBuf {
        (0..MAX_DEPTH + 2).fold(top.to_path_buf(), |path, _| path.join("d"))
    }

    fn intact(outside: &Path) -> bool {
        std::fs::read(outside.join("file")).is_ok_and(|bytes| bytes == b"keep me")
            && std::fs::read(outside.join("dir").join("inner"))
                .is_ok_and(|bytes| bytes == b"me too")
    }

    #[test]
    fn a_look_sees_what_is_there_and_an_entry_goes_unless_kept() {
        let dir = scratch("look");
        let (root, outside) = (dir.0.join("root"), dir.0.join("outside"));
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("file"), b"f").unwrap();
        symlink(outside.join("dir"), root.join("link")).unwrap();
        let handle = rustix::fs::open(&root, DIR_FLAGS, Mode::empty()).unwrap();
        let this = look(&handle, c"").unwrap();
        let sub = look(&handle, c"sub").unwrap();
        assert!(this.directory && sub.directory);
        assert_eq!(sub.id, DirId::of(&root.join("sub")).unwrap());
        assert_eq!(sub.mount, this.mount, "one mount");
        assert!(!look(&handle, c"file").unwrap().directory);
        assert!(
            !look(&handle, c"link").unwrap().directory,
            "a symlink, not its target"
        );
        assert!(look(&handle, c"missing").is_err());
        // A kept directory stays; a file or a symlink goes, its target kept.
        let here = [this.id];
        let owner = rustix::process::geteuid().as_raw();
        assert_eq!(this.uid, owner);
        let walk = |keep: &'static [DirId]| Walk {
            mount: this.mount,
            keep,
            owner,
        };
        let kept = Walk {
            keep: &[sub.id][..],
            ..walk(&[])
        };
        assert!(!remove_entry(&handle, c"sub", kept, &here));
        assert!(root.join("sub").exists());
        // `.` and `..` are never entered, even asked for by name: by their
        // identity, the walk is in them.
        let parent = DirId::of(&dir.0).unwrap();
        assert!(!remove_entry(&handle, c".", walk(&[]), &[parent, this.id]));
        assert!(!remove_entry(&handle, c"..", walk(&[]), &[parent, this.id]));
        assert!(root.join("file").exists() && root.join("sub").exists());
        assert!(intact(&outside), "nothing above the tree goes");
        assert!(remove_entry(&handle, c"file", walk(&[]), &here));
        assert!(remove_entry(&handle, c"link", walk(&[]), &here));
        assert!(!root.join("file").exists() && intact(&outside));
        let deep = vec![parent; MAX_DEPTH + 1];
        assert!(!remove_entry(&handle, c"sub", walk(&[]), &deep), "too deep");
        assert!(
            !remove_entry(
                &handle,
                c"sub",
                Walk {
                    mount: this.mount + 1,
                    ..walk(&[])
                },
                &here
            ),
            "another mount"
        );
        let stranger = Walk {
            owner: owner.wrapping_add(1),
            ..walk(&[])
        };
        assert!(
            !remove_entry(&handle, c"sub", stranger, &here),
            "another user's"
        );
        assert!(root.join("sub").exists());
        assert!(remove_entry(&handle, c"sub", walk(&[]), &here));
        assert!(!root.join("sub").exists());
        assert!(!remove_entry(&handle, c"missing", walk(&[]), &here));
    }

    #[test]
    fn a_tree_goes_whole_and_its_symlinks_take_nothing_with_them() {
        let dir = scratch("whole");
        let (root, outside) = (dir.0.join("root"), dir.0.join("outside"));
        plant(&root.join("tree"), &outside);
        assert_eq!(
            remove_tree(&root, OsStr::new("tree"), &[]).unwrap(),
            Removed::Whole
        );
        assert!(!root.join("tree").exists());
        assert!(intact(&outside));
        assert_eq!(MAX_DEPTH, 64);
    }

    #[test]
    fn a_kept_directory_stays_whole_with_everything_above_it() {
        let dir = scratch("keep");
        let (root, outside) = (dir.0.join("root"), dir.0.join("outside"));
        let tree = root.join("tree");
        plant(&tree, &outside);
        let state = tree.join("a").join("vault");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("secret"), b"s").unwrap();
        let keep = [DirId::of(&state).unwrap()];
        assert_eq!(
            remove_tree(&root, OsStr::new("tree"), &keep).unwrap(),
            Removed::Partly
        );
        assert_eq!(std::fs::read(state.join("secret")).unwrap(), b"s");
        assert!(!tree.join("file").exists() && !tree.join("a").join("b").exists());
        assert!(intact(&outside));
        // Kept at the top: nothing is touched.
        let other = root.join("other");
        plant(&other, &outside);
        let kept = [DirId::of(&other).unwrap()];
        assert_eq!(
            remove_tree(&root, OsStr::new("other"), &kept).unwrap(),
            Removed::Partly
        );
        assert!(other.join("file").exists());
    }

    #[test]
    fn a_symlinked_tree_or_root_is_refused_and_nothing_goes() {
        let dir = scratch("link");
        let (root, outside) = (dir.0.join("root"), dir.0.join("outside"));
        symlink(&outside, root.join("tree")).unwrap();
        assert!(matches!(
            remove_tree(&root, OsStr::new("tree"), &[]),
            Err(Error::Removal {
                operation: "open the tree",
                ..
            })
        ));
        symlink(&root, dir.0.join("root-link")).unwrap();
        plant(&root.join("real"), &outside);
        assert!(matches!(
            remove_tree(&dir.0.join("root-link"), OsStr::new("real"), &[]),
            Err(Error::Removal {
                operation: "open the root",
                ..
            })
        ));
        assert!(root.join("real").join("file").exists());
        assert!(matches!(
            remove_tree(&root, OsStr::new("missing"), &[]),
            Err(Error::Removal { .. })
        ));
        assert!(matches!(
            remove_tree(&root, OsStr::new("a\0b"), &[]),
            Err(Error::Removal {
                operation: "name the tree",
                ..
            })
        ));
        for name in ["", ".", "..", "a/b", "../outside"] {
            assert!(
                matches!(
                    remove_tree(&root, OsStr::new(name), &[]),
                    Err(Error::Removal {
                        operation: "name the tree",
                        ..
                    })
                ),
                "{name:?}"
            );
        }
        assert!(intact(&outside));
    }

    #[test]
    fn a_tree_too_deep_keeps_what_lies_below_its_bound() {
        let dir = scratch("deep");
        let root = dir.0.join("root");
        let mut deepest = root.join("tree");
        for _ in 0..=MAX_DEPTH {
            deepest.push("d");
        }
        std::fs::create_dir_all(&deepest).unwrap();
        std::fs::write(deepest.join("bottom"), b"z").unwrap();
        assert_eq!(
            remove_tree(&root, OsStr::new("tree"), &[]).unwrap(),
            Removed::Partly
        );
        assert!(deepest.join("bottom").exists());
        // One level less goes whole.
        let shallow_dir = scratch("shallow");
        let shallow = shallow_dir.0.join("root");
        let mut last = shallow.join("tree");
        for _ in 1..MAX_DEPTH {
            last.push("d");
        }
        std::fs::create_dir_all(&last).unwrap();
        std::fs::write(last.join("bottom"), b"z").unwrap();
        assert_eq!(
            remove_tree(&shallow, OsStr::new("tree"), &[]).unwrap(),
            Removed::Whole
        );
    }

    #[test]
    fn a_mount_in_the_tree_is_never_entered() {
        // In a child with its own user and mount namespaces: a directory
        // from outside the tree, bind-mounted into it (the same device,
        // another mount), and the tree bind-mounted at its own root (a
        // mount at the top).
        use crate::process::{ForkResult, child_exit_code, exit_child, fork};
        use std::time::Duration;

        let dir = scratch("mount");
        let (root, outside) = (dir.0.join("root"), dir.0.join("outside"));
        let child = match fork().unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                crate::mounts::tests::enter_private_mount_namespace();
                let tree = root.join("tree");
                plant(&tree, &outside);
                let bound = tree.join("a").join("bound");
                if std::fs::create_dir(&bound).is_err() {
                    exit_child(99);
                }
                let bind = |source: &Path, target: &Path| {
                    let (Ok(source), Ok(target)) = (
                        CString::new(source.as_os_str().as_encoded_bytes()),
                        CString::new(target.as_os_str().as_encoded_bytes()),
                    ) else {
                        exit_child(99)
                    };
                    // SAFETY: a bind mount in this child's own mount
                    // namespace, from two valid C strings.
                    unsafe {
                        libc::mount(
                            source.as_ptr(),
                            target.as_ptr(),
                            std::ptr::null(),
                            libc::MS_BIND,
                            std::ptr::null(),
                        )
                    }
                };
                if bind(&outside.join("dir"), &bound) != 0 {
                    exit_child(99);
                }
                if !matches!(
                    remove_tree(&root, OsStr::new("tree"), &[]),
                    Ok(Removed::Partly)
                ) {
                    exit_child(2);
                }
                if !intact(&outside) || tree.join("file").exists() {
                    exit_child(3);
                }
                let top = root.join("mounted");
                if std::fs::create_dir(&top).is_err() || bind(&outside, &top) != 0 {
                    exit_child(99);
                }
                if !matches!(
                    remove_tree(&root, OsStr::new("mounted"), &[]),
                    Err(Error::Removal {
                        operation: "enter the tree",
                        ..
                    })
                ) || !intact(&outside)
                {
                    exit_child(4);
                }
                exit_child(0);
            }
        };
        let status = child.wait_deadline(Duration::from_secs(30)).unwrap();
        let code = child_exit_code(status);
        if code == 99 {
            let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1");
            assert!(
                !required,
                "HATCHLING_REQUIRE_CELL=1 but namespaces are unavailable: refusing to skip"
            );
            return;
        }
        assert_eq!(code, 0, "the mounts must be left, and the rest removed");
        assert!(intact(&outside));
    }
}
