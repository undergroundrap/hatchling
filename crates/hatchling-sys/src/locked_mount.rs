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

use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
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

/// Pin the root of a locked-down ext4 filesystem in this mount namespace.
///
/// The caller supplies the trusted device's kernel major:minor string
/// and must secure the directory chain to the target.
///
/// # Errors
///
/// A mount error if the target can't be pinned or isn't the expected
/// filesystem root with the locked-down flags.
pub fn pin_locked_ext4_root(target: &Path, device: &str) -> Result<OwnedFd, Error> {
    use rustix::fs::{AtFlags, Mode, OFlags, StatxFlags};

    let source = rustix::fs::open(
        target,
        OFlags::PATH
            .union(OFlags::DIRECTORY)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )
    .map_err(failed("pin room workspace"))?;
    let seen = rustix::fs::statx(source.as_fd(), "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)
        .map_err(failed("look at pinned room workspace"))?;
    // An ordinary directory reports the same mount ID as the mount root.
    if !StatxFlags::from_bits_retain(seen.stx_mask).contains(StatxFlags::MNT_ID)
        || seen.stx_attributes & STATX_ATTR_MOUNT_ROOT == 0
    {
        return Err(refused(
            "check pinned room workspace",
            String::from("the room workspace isn't a known mount root"),
        ));
    }
    let mounts = std::fs::read("/proc/self/mountinfo").map_err(|source| Error::Mount {
        operation: "read room workspace mounts",
        source,
    })?;
    check_workspace_mount(&mounts, seen.stx_mnt_id, device)?;
    Ok(source)
}

/// Match the descriptor's mount ID against the current namespace's kernel
/// record. The device number identifies the filesystem, and the root field
/// distinguishes a whole volume from a bind of a directory on that volume.
fn check_workspace_mount(mounts: &[u8], id: u64, device: &str) -> Result<(), Error> {
    let why = |reason: &str| refused("check room workspace mount", reason.to_owned());
    let id = id.to_string();
    let Some(mut fields) = mounts.split(|&byte| byte == b'\n').find_map(|line| {
        let mut fields = line.split(|&byte| byte == b' ');
        (fields.next()? == id.as_bytes()).then_some(fields)
    }) else {
        return Err(why("the pinned room workspace mount isn't listed"));
    };
    let _parent = fields.next();
    let actual_device = fields.next();
    let root = fields.next();
    let _target = fields.next();
    let options = fields.next();
    let fstype = fields.skip_while(|field| *field != b"-").nth(1);
    if actual_device != Some(device.as_bytes()) || root != Some(b"/") || fstype != Some(b"ext4") {
        return Err(why(
            "the room workspace isn't the root of the expected encrypted ext4 volume",
        ));
    }
    let options = options.unwrap_or_default();
    for needed in [b"nodev".as_slice(), b"nosuid", b"noexec"] {
        if !options
            .split(|&byte| byte == b',')
            .any(|option| option == needed)
        {
            return Err(why("the room workspace mount isn't locked down"));
        }
    }
    Ok(())
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

/// The size of the file system holding a path, and its free space, in
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    /// All of it.
    pub size: u64,
    /// What's free, root's reserve included.
    pub free: u64,
    /// What's free to anyone.
    pub available: u64,
}

/// The size and free space of the file system holding `path`.
///
/// # Errors
///
/// [`Error::Mount`] when it can't be looked at.
pub fn space(path: &Path) -> Result<Space, Error> {
    let seen = rustix::fs::statvfs(path).map_err(failed("look at a file system's space"))?;
    Ok(space_from(&seen))
}

/// The size and free space of the filesystem pinned by the descriptor, in bytes.
///
/// # Errors
///
/// A mount error when the pinned filesystem can't be inspected.
pub fn space_at(fd: BorrowedFd<'_>) -> Result<Space, Error> {
    let seen = rustix::fs::fstatvfs(fd).map_err(failed("look at a pinned file system's space"))?;
    Ok(space_from(&seen))
}

fn space_from(seen: &rustix::fs::StatVfs) -> Space {
    let bytes = |blocks: u64| blocks.saturating_mul(seen.f_frsize);
    Space {
        size: bytes(seen.f_blocks),
        free: bytes(seen.f_bfree),
        available: bytes(seen.f_bavail),
    }
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

    #[test]
    fn workspace_mount_requires_the_pinned_id_full_volume_and_all_flags() {
        let good = b"10 5 253:7 / /room/mnt rw,nosuid,nodev,noexec shared:1 - ext4 /dev/dm-7 rw\n";
        assert!(check_workspace_mount(good, 10, "253:7").is_ok());
        assert!(
            check_workspace_mount(good, 11, "253:7")
                .unwrap_err()
                .to_string()
                .contains("isn't listed")
        );
        assert!(
            check_workspace_mount(good, 10, "253:8")
                .unwrap_err()
                .to_string()
                .contains("expected encrypted ext4")
        );
        for bad in [
            b"10 5 253:7 /nested /room/mnt rw,nosuid,nodev,noexec - ext4 /dev/dm-7 rw\n".as_slice(),
            b"10 5 253:7 / /room/mnt rw,nosuid,nodev,noexec - xfs /dev/dm-7 rw\n",
            b"10 5 253:7 / /room/mnt rw,nosuid,nodev,noexec shared:1 ext4 /dev/dm-7 rw\n",
        ] {
            assert!(
                check_workspace_mount(bad, 10, "253:7")
                    .unwrap_err()
                    .to_string()
                    .contains("expected encrypted ext4")
            );
        }
        for options in ["rw,nodev,noexec", "rw,nosuid,noexec", "rw,nosuid,nodev"] {
            let row = format!("10 5 253:7 / /room/mnt {options} - ext4 /dev/dm-7 rw\n");
            assert!(
                check_workspace_mount(row.as_bytes(), 10, "253:7")
                    .unwrap_err()
                    .to_string()
                    .contains("isn't locked down")
            );
        }
        let mut stacked = good.to_vec();
        stacked
            .extend_from_slice(b"11 10 0:55 / /room/mnt rw,nosuid,nodev,noexec - tmpfs none rw\n");
        assert!(
            check_workspace_mount(&stacked, 11, "253:7")
                .unwrap_err()
                .to_string()
                .contains("expected encrypted ext4")
        );
    }

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

    /// A file system's space is in bytes, and what's free to anyone is no
    /// more than what's free, which is no more than all of it.
    #[test]
    fn a_file_systems_space_adds_up() {
        let dir = scratch("space");
        let seen = space(&dir).unwrap();
        assert!(seen.size > 0, "a file system of no size");
        assert!(
            seen.available <= seen.free && seen.free <= seen.size,
            "the free space doesn't add up"
        );
        assert!(space(&dir.join("none")).is_err());
        let fd = std::fs::File::open(&dir).unwrap();
        assert_eq!(space_at(fd.as_fd()).unwrap().size, seen.size);
        let mut stat = rustix::fs::fstatvfs(&fd).unwrap();
        stat.f_frsize = 1024;
        stat.f_blocks = u64::MAX;
        stat.f_bfree = 10;
        stat.f_bavail = 4;
        assert_eq!(
            space_from(&stat),
            Space {
                size: u64::MAX,
                free: 10_240,
                available: 4_096,
            }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Nothing is said to be unmounted that wasn't: a directory with
    /// nothing mounted on it, and a path that isn't there, are refused.
    #[test]
    fn an_unmount_of_nothing_mounted_is_refused() {
        let dir = scratch("unmount");
        assert!(
            unmount_strict(&dir).is_err(),
            "a directory with nothing mounted on it was said to be unmounted"
        );
        assert!(unmount_strict(&dir.join("none")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A directory that isn't mounted on is no mount point, and neither is
    /// a link to one; `/` is.
    #[test]
    fn a_mount_point_is_told_from_a_directory() {
        let dir = scratch("point");
        assert!(!is_mount_point(&dir).unwrap());
        assert!(
            pin_locked_ext4_root(&dir, "0:0")
                .unwrap_err()
                .to_string()
                .contains("mount root")
        );
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
