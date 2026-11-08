//! A file system mounted locked down: no device files, no set-id programs,
//! no programs at all (0.9's room disk).
//!
//! [`mount_locked_down`] mounts with `nodev`, `nosuid` and `noexec`, then
//! asks the kernel what it got ([`verify_locked_down`]): a mount that
//! doesn't hold all three, whatever the reason, is taken down again and
//! refused. [`unmount_strict`] takes one down and says so if it can't:
//! never lazily (`MNT_DETACH`), which would leave a file system in use with
//! no path to it, and never by force.
//!
//! Where to mount, and what, is the caller's: these say how.

use std::path::Path;

use rustix::fs::StatVfsMountFlags;
use rustix::mount::{MountFlags, UnmountFlags};

use crate::Error;

/// What a locked-down mount is mounted with.
const LOCKED_DOWN: MountFlags = MountFlags::NODEV
    .union(MountFlags::NOSUID)
    .union(MountFlags::NOEXEC);

/// What `statvfs` must report of a locked-down mount.
const LOCKED_DOWN_SEEN: StatVfsMountFlags = StatVfsMountFlags::NODEV
    .union(StatVfsMountFlags::NOSUID)
    .union(StatVfsMountFlags::NOEXEC);

/// `STATX_ATTR_MOUNT_ROOT`: the path is the root of a mount.
const STATX_ATTR_MOUNT_ROOT: u64 = 0x2000;

/// An [`Error::Mount`] for `operation`, from a rustix error.
fn failed(operation: &'static str) -> impl Fn(rustix::io::Errno) -> Error {
    move |source| Error::Mount {
        operation,
        source: std::io::Error::from_raw_os_error(source.raw_os_error()),
    }
}

/// An [`Error::Mount`] for `operation` that a check here made, saying `why`.
fn refused(operation: &'static str, why: String) -> Error {
    Error::Mount {
        operation,
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, why),
    }
}

/// Mounts `source`, a file system of type `fstype`, at `target`, with
/// `data` as its options, locked down: `nodev`, `nosuid` and `noexec`.
///
/// `target` must be a directory itself, not a link, and not a mount point
/// already: a second mount over a first would hide it rather than replace
/// it. Once mounted, what the kernel reports of it is checked; a mount
/// without all three is unmounted and refused.
///
/// # Errors
///
/// [`Error::Mount`] when `target` isn't one to mount at, the mount fails,
/// or it isn't locked down.
pub fn mount_locked_down(
    source: &Path,
    target: &Path,
    fstype: &str,
    data: &str,
) -> Result<(), Error> {
    let meta = std::fs::symlink_metadata(target).map_err(|error| Error::Mount {
        operation: "look at where to mount",
        source: error,
    })?;
    if !meta.file_type().is_dir() {
        return Err(refused(
            "mount locked down",
            format!("{} isn't a directory itself", target.display()),
        ));
    }
    if is_mount_point(target)? {
        return Err(refused(
            "mount locked down",
            format!("{} is a mount point already", target.display()),
        ));
    }
    rustix::mount::mount(source, target, fstype, LOCKED_DOWN, data).map_err(failed("mount"))?;
    if let Err(error) = verify_locked_down(target) {
        let _ = rustix::mount::unmount(target, UnmountFlags::NOFOLLOW);
        return Err(error);
    }
    Ok(())
}

/// Why the mount at `target` isn't locked down, if it isn't: it must be a
/// mount point, and the kernel must report `nodev`, `nosuid` and `noexec`
/// of it.
///
/// # Errors
///
/// [`Error::Mount`] naming what's missing, or why it couldn't be looked at.
pub fn verify_locked_down(target: &Path) -> Result<(), Error> {
    if !is_mount_point(target)? {
        return Err(refused(
            "check a mount",
            format!("{} isn't a mount point", target.display()),
        ));
    }
    let seen = rustix::fs::statvfs(target)
        .map_err(failed("look at a mount"))?
        .f_flag;
    let missing = LOCKED_DOWN_SEEN.difference(seen);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(refused(
            "check a mount",
            format!(
                "{} is mounted without {}",
                target.display(),
                flag_names(missing)
            ),
        ))
    }
}

/// The names of `flags`, of those a locked-down mount needs.
fn flag_names(flags: StatVfsMountFlags) -> String {
    [
        (StatVfsMountFlags::NODEV, "nodev"),
        (StatVfsMountFlags::NOSUID, "nosuid"),
        (StatVfsMountFlags::NOEXEC, "noexec"),
    ]
    .into_iter()
    .filter(|(flag, _)| flags.contains(*flag))
    .map(|(_, name)| name)
    .collect::<Vec<_>>()
    .join(", ")
}

/// Unmounts the file system at `target`, there and then: never lazily,
/// which would leave it in use with no path to it, and never by force. A
/// file system in use is refused (`EBUSY`), and stays.
///
/// # Errors
///
/// [`Error::Mount`] when it isn't unmounted, or something is still mounted
/// at `target` after (one mount stacked on another).
pub fn unmount_strict(target: &Path) -> Result<(), Error> {
    rustix::mount::unmount(target, UnmountFlags::NOFOLLOW).map_err(failed("unmount"))?;
    if is_mount_point(target)? {
        return Err(refused(
            "unmount",
            format!("{} is still a mount point", target.display()),
        ));
    }
    Ok(())
}

/// Whether `path` is the root of a mount.
///
/// As the kernel says (`STATX_ATTR_MOUNT_ROOT`), which Linux has since 5.8,
/// before any kernel Hatchling runs on. A kernel that doesn't say is
/// refused rather than guessed at. A link isn't followed: a link is never
/// a mount point.
///
/// # Errors
///
/// [`Error::Mount`] when it can't be looked at, or the kernel doesn't say.
pub fn is_mount_point(path: &Path) -> Result<bool, Error> {
    use rustix::fs::{AtFlags, StatxFlags};
    let flags = AtFlags::SYMLINK_NOFOLLOW.union(AtFlags::NO_AUTOMOUNT);
    let seen = rustix::fs::statx(rustix::fs::CWD, path, flags, StatxFlags::BASIC_STATS)
        .map_err(failed("look at a mount point"))?;
    if seen.stx_attributes_mask & STATX_ATTR_MOUNT_ROOT != STATX_ATTR_MOUNT_ROOT {
        return Err(refused(
            "look at a mount point",
            String::from("this kernel doesn't say what's a mount point"),
        ));
    }
    Ok(seen.stx_attributes & STATX_ATTR_MOUNT_ROOT != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of a test's own.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-mount-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    /// Each error names what was being done, and why it failed: the
    /// kernel's own words, or the check's.
    #[test]
    fn an_error_names_the_operation_and_why() {
        let said = failed("unmount")(rustix::io::Errno::BUSY).to_string();
        assert!(
            said.contains("unmount")
                && said.contains(&std::io::Error::from_raw_os_error(16).to_string()),
            "the kernel's error wasn't named with its operation: {said}"
        );
        let said = refused("check a mount", String::from("a reason of its own")).to_string();
        assert!(
            said.contains("check a mount") && said.contains("a reason of its own"),
            "the check's reason wasn't named with its operation: {said}"
        );
    }

    /// A directory that isn't mounted on is no mount point, and neither is
    /// a link to one; `/` is.
    #[test]
    fn a_mount_point_is_told_from_a_directory() {
        let dir = scratch("point");
        assert!(!is_mount_point(&dir).unwrap());
        let link = dir.join("link");
        std::os::unix::fs::symlink("/", &link).unwrap();
        assert!(!is_mount_point(&link).unwrap(), "a link was followed");
        assert!(is_mount_point(Path::new("/")).unwrap());
        assert!(is_mount_point(&dir.join("none")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// What isn't a mount is never taken as locked down, and a mount's
    /// missing flags are named.
    #[test]
    fn only_a_mount_with_all_three_flags_is_locked_down() {
        let dir = scratch("verify");
        let said = verify_locked_down(&dir).unwrap_err().to_string();
        assert!(said.contains("isn't a mount point"), "a directory passed");
        assert_eq!(flag_names(StatVfsMountFlags::empty()), "");
        assert_eq!(flag_names(LOCKED_DOWN_SEEN), "nodev, nosuid, noexec");
        assert_eq!(
            flag_names(StatVfsMountFlags::NOEXEC | StatVfsMountFlags::RDONLY),
            "noexec"
        );
        // `/` is a mount point, and a root file system mounted noexec
        // couldn't have run this test: it's refused, naming noexec.
        let root = verify_locked_down(Path::new("/"))
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(root.contains("noexec"), "a mount that runs programs passed");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Where a mount can't go is refused before the mount is tried: a link,
    /// a file, a mount point already, nothing there.
    #[test]
    fn a_mount_is_refused_where_it_cannot_go() {
        let dir = scratch("where");
        let source = Path::new("tmpfs");
        let link = dir.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let file = dir.join("file");
        std::fs::write(&file, b"").unwrap();
        for target in [link.as_path(), file.as_path(), Path::new("/")] {
            let said = mount_locked_down(source, target, "tmpfs", "")
                .unwrap_err()
                .to_string();
            assert!(
                said.contains("isn't a directory itself") || said.contains("a mount point already"),
                "a mount went somewhere it can't, or failed for another reason"
            );
        }
        assert!(mount_locked_down(source, &dir.join("none"), "tmpfs", "").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
