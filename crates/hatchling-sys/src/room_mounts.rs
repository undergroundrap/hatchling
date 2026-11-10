//! Build the WO-034 room view from descriptors held by the trusted launcher.
//!
//! The verified system image becomes `/`. The encrypted workspace and
//! its separately supplied read-only inbox are attached below it. This
//! plan is separate from the ordinary agent cell's tmpfs root.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;

use rustix::fs::{FileType, Mode, OFlags, ResolveFlags, StatVfsMountFlags};
use rustix::mount::{
    self, FsMountFlags, FsOpenFlags, MountAttrFlags, MountPropagationFlags, MoveMountFlags,
    OpenTreeFlags, UnmountFlags,
};

use crate::Error;
use crate::room_image::RoomImageMapper;

const IMAGE: MountAttrFlags = MountAttrFlags::MOUNT_ATTR_RDONLY
    .union(MountAttrFlags::MOUNT_ATTR_NODEV)
    .union(MountAttrFlags::MOUNT_ATTR_NOSUID);
const DATA: MountAttrFlags = MountAttrFlags::MOUNT_ATTR_NOEXEC
    .union(MountAttrFlags::MOUNT_ATTR_NODEV)
    .union(MountAttrFlags::MOUNT_ATTR_NOSUID);
const READ_ONLY_DATA: MountAttrFlags = DATA.union(MountAttrFlags::MOUNT_ATTR_RDONLY);
const DEVICE: MountAttrFlags = MountAttrFlags::MOUNT_ATTR_NOEXEC
    .union(MountAttrFlags::MOUNT_ATTR_NOSUID)
    .union(MountAttrFlags::MOUNT_ATTR_RDONLY);
const SESSION_BYTES: &str = "536870912";
// Linux UAPI <linux/magic.h>.
const SQUASHFS_MAGIC: rustix::fs::FsWord = 0x7371_7368;

/// Proof that the single-threaded launch child prepared its mount namespace.
pub struct RoomMountNamespace(std::marker::PhantomData<*mut ()>);

/// Sources pinned by the trusted room launcher before a process enters it.
pub struct RoomMounts {
    /// The root of the executable, read-only dm-verity `SquashFS` mount.
    /// Authenticating the image and mapper is the caller's responsibility.
    pub image: OwnedFd,
    /// The writable workspace directory on the authenticated encrypted disk.
    pub workspace: OwnedFd,
    /// The separate spawner-owned inbox directory on the encrypted disk.
    pub inbox: OwnedFd,
    /// An empty staging directory held exclusively by the trusted launcher.
    pub staging: OwnedFd,
    /// Owner of the private session tmpfs, in the mounting user namespace.
    pub uid: u32,
    /// Group of the private session tmpfs, in the mounting user namespace.
    pub gid: u32,
}

impl RoomMounts {
    /// Prepare the fixed room mount sources and inbox attachment point.
    /// The spawner passes the pinned root of its authenticated encrypted
    /// volume while it holds the disk-operation lock.
    ///
    /// # Errors
    /// Refuses an untrusted volume root or inbox, links or mount crossings
    /// at the fixed names, and any failed directory operation.
    pub fn prepare_volume(volume: BorrowedFd<'_>) -> Result<(), Error> {
        require_root_control(volume, "verify room volume root ownership")?;
        let inbox = ensure_directory_at(volume, "inbox")?;
        require_root_control(inbox.as_fd(), "verify room inbox ownership")?;
        let workspace = ensure_directory_at(volume, "workspace")?;
        ensure_directory_at(workspace.as_fd(), "inbox").map(drop)
    }

    /// Create the room's private mount namespace in a disposable launch child.
    /// The caller must be the sole task in its new PID namespace and must not
    /// create threads or children before the root switch completes.
    /// Open the directory sources to be cloned after this succeeds. The kernel
    /// refuses to clone mount descriptors opened in the previous mount namespace.
    /// A handed-off block pin identifies a device, so it may be opened earlier.
    ///
    /// # Errors
    ///
    /// Returns an error outside a single-threaded PID 1 or if setup fails.
    pub fn prepare_namespace() -> Result<RoomMountNamespace, Error> {
        let pid = rustix::process::getpid().as_raw_nonzero().get();
        let tid = rustix::thread::gettid().as_raw_nonzero().get();
        if (pid, tid) != (1, 1) {
            return Err(refused("prepare room mounts", "expected namespace init"));
        }
        // unshare splits the calling thread's filesystem state. A sibling
        // would keep the old root reachable through the room's proc mount.
        if !single_threaded_init()? {
            return Err(refused(
                "prepare room mounts",
                "namespace init has another thread",
            ));
        }
        // Keep mount propagation private before attaching any room source.
        crate::unshare_mount()?;
        mount::mount_change(
            "/",
            MountPropagationFlags::PRIVATE.union(MountPropagationFlags::REC),
        )
        .map_err(failed("make room mounts private"))?;
        Ok(RoomMountNamespace(std::marker::PhantomData))
    }

    /// Enter the room's mount view in the namespace prepared by
    /// [`Self::prepare_namespace`]. The trusted caller must pin the sources
    /// there, exclude other writers during setup, and close any other host
    /// filesystem descriptors before exec. It also owns the identity drop,
    /// filter, cgroup and source authentication.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid mount inputs or a failed setup syscall.
    /// On any error the child must exit before running an untrusted program.
    pub fn enter(self, _namespace: RoomMountNamespace) -> Result<(), Error> {
        let root = clone_mount(self.image.as_fd())?;
        let Self {
            image,
            workspace,
            inbox,
            staging,
            uid,
            gid,
        } = self;
        drop(image);
        Self::enter_with_root(root, workspace, inbox, staging, uid, gid)
    }

    /// Mount the authenticated image in the prepared launch namespace,
    /// then enter the room. The caller owns the verified mapper until all
    /// room processes exit. Workspace, inbox and staging are pinned in
    /// this namespace under the same contract as [`Self::enter`].
    ///
    /// # Errors
    ///
    /// Refuses an invalid mapper or mount source and returns any setup error.
    /// The launch child must exit on failure.
    pub fn enter_verified(
        _namespace: RoomMountNamespace,
        image: &RoomImageMapper,
        workspace: OwnedFd,
        inbox: OwnedFd,
        staging: OwnedFd,
        uid: u32,
        gid: u32,
    ) -> Result<(), Error> {
        let (block, expected_device) = image.pin_device().map_err(|source| Error::Mount {
            operation: "pin verified room image device",
            source,
        })?;
        let root = mount_verified_image(block, expected_device)?;
        Self::enter_with_root(root, workspace, inbox, staging, uid, gid)
    }

    /// Enter with the verified image and the data directories from the
    /// authenticated encrypted volume. The spawner supplies the device number
    /// from its verified LUKS mapping and secures the chain to `volume`.
    /// The volume root and its inbox are controlled by root. The workspace and
    /// inbox are fixed sibling directories and may not cross a mount boundary.
    ///
    /// # Errors
    ///
    /// Refuses the wrong volume, unexpected directory ownership or permissions,
    /// and any failed mount operation. The launch child must exit on failure.
    pub fn enter_verified_volume(
        namespace: RoomMountNamespace,
        image: &RoomImageMapper,
        volume: &Path,
        device: &str,
        staging: OwnedFd,
        uid: u32,
        gid: u32,
    ) -> Result<(), Error> {
        let (workspace, inbox) = pin_encrypted_sources(volume, device)?;
        Self::enter_verified(namespace, image, workspace, inbox, staging, uid, gid)
    }

    /// Enter from a mapper pin received over the spawner's dedicated private
    /// socket. The launch child checks the pin and reauthenticates the data
    /// sources under the same contract as [`Self::enter_verified_volume`].
    /// The spawner retains the mapper and must drain the room before closing it.
    ///
    /// # Errors
    ///
    /// Refuses a missing or invalid descriptor, a mismatched filesystem or
    /// volume, or any failed mount operation. The child must exit on failure.
    pub fn enter_handed_off_volume(
        _namespace: RoomMountNamespace,
        control: UnixStream,
        volume: &Path,
        device: &str,
        staging: OwnedFd,
        uid: u32,
        gid: u32,
    ) -> Result<(), Error> {
        let (block, expected_device) = crate::room_image::receive_launcher_device(&control)?;
        drop(control);
        let (workspace, inbox) = pin_encrypted_sources(volume, device)?;
        let root = mount_verified_image(block, expected_device)?;
        Self::enter_with_root(root, workspace, inbox, staging, uid, gid)
    }

    fn enter_with_root(
        root: OwnedFd,
        workspace_source: OwnedFd,
        inbox_source: OwnedFd,
        staging: OwnedFd,
        uid: u32,
        gid: u32,
    ) -> Result<(), Error> {
        attach(root.as_fd(), staging.as_fd(), IMAGE)?;
        let workspace_at = directory_at(root.as_fd(), "workspace")?;
        let workspace = clone_mount(workspace_source.as_fd())?;
        attach(workspace.as_fd(), workspace_at.as_fd(), DATA)?;
        let inbox_at = directory_at(workspace.as_fd(), "inbox")?;
        let inbox = clone_mount(inbox_source.as_fd())?;
        attach(inbox.as_fd(), inbox_at.as_fd(), READ_ONLY_DATA)?;

        let uid = uid.to_string();
        let gid = gid.to_string();
        let session = filesystem(
            "tmpfs",
            &[
                ("size", SESSION_BYTES),
                ("mode", "0700"),
                ("uid", &uid),
                ("gid", &gid),
            ],
            DATA,
        )?;
        attach(
            session.as_fd(),
            directory_at(root.as_fd(), "session")?.as_fd(),
            DATA,
        )?;

        let dev = filesystem("tmpfs", &[("size", "1M"), ("mode", "0755")], DATA)?;
        attach(
            dev.as_fd(),
            directory_at(root.as_fd(), "dev")?.as_fd(),
            DATA,
        )?;
        add_device(dev.as_fd(), "null", 3)?;
        add_device(dev.as_fd(), "zero", 5)?;
        add_device(dev.as_fd(), "urandom", 9)?;
        set_flags(dev.as_fd(), MountAttrFlags::MOUNT_ATTR_RDONLY)?;

        let proc = filesystem("proc", &[("subset", "pid")], READ_ONLY_DATA)?;
        attach(
            proc.as_fd(),
            directory_at(root.as_fd(), "proc")?.as_fd(),
            READ_ONLY_DATA,
        )?;

        // Release descriptors that refer to host paths before pivoting.
        drop(staging);
        drop(workspace_source);
        drop(inbox_source);
        drop(workspace_at);
        drop(inbox_at);
        drop(workspace);
        drop(inbox);
        enter_root(root.as_fd())?;
        drop(root);
        verify_view()
    }
}

fn mount_verified_image(block: OwnedFd, expected_device: u64) -> Result<OwnedFd, Error> {
    let context = mount::fsopen("squashfs", FsOpenFlags::FSOPEN_CLOEXEC)
        .map_err(failed("open verified room filesystem"))?;
    // Resolve the pinned block inode when the kernel opens its source.
    mount::fsconfig_set_string(
        context.as_fd(),
        "source",
        format!("/proc/self/fd/{}", block.as_raw_fd()),
    )
    .map_err(failed("set verified room filesystem source"))?;
    mount::fsconfig_set_flag(context.as_fd(), "ro")
        .map_err(failed("set verified room filesystem read-only"))?;
    mount::fsconfig_create(context.as_fd()).map_err(failed("create verified room filesystem"))?;
    let root = mount::fsmount(context.as_fd(), FsMountFlags::FSMOUNT_CLOEXEC, IMAGE)
        .map_err(failed("mount verified room filesystem"))?;
    verify_image_mount(root.as_fd(), expected_device)?;
    drop(block);
    Ok(root)
}

fn pin_encrypted_sources(volume: &Path, device: &str) -> Result<(OwnedFd, OwnedFd), Error> {
    let disk = crate::locked_mount::pin_locked_ext4_root(volume, device)?;
    require_root_control(disk.as_fd(), "verify room volume root ownership")?;
    let workspace = directory_at(disk.as_fd(), "workspace")?;
    let inbox = directory_at(disk.as_fd(), "inbox")?;
    require_root_control(inbox.as_fd(), "verify room inbox ownership")?;
    Ok((workspace, inbox))
}

fn require_root_control(directory: BorrowedFd<'_>, operation: &'static str) -> Result<(), Error> {
    let stat = rustix::fs::fstat(directory).map_err(failed(operation))?;
    // A root-controlled parent keeps the room user from replacing either
    // source directory. The inbox itself must also refuse untrusted writers.
    if stat.st_uid != 0 || stat.st_mode & 0o022 != 0 {
        return Err(refused(operation, "directory must be controlled by root"));
    }
    Ok(())
}

fn verify_image_mount(root: BorrowedFd<'_>, expected_device: u64) -> Result<(), Error> {
    let stat = rustix::fs::fstat(root).map_err(failed("identify verified room mount device"))?;
    let fs = rustix::fs::fstatfs(root).map_err(failed("identify verified room filesystem"))?;
    validate_image_mount(stat.st_dev, expected_device, fs.f_type == SQUASHFS_MAGIC)
}

fn validate_image_mount(actual: u64, expected: u64, squashfs: bool) -> Result<(), Error> {
    if actual != expected || !squashfs {
        return Err(refused(
            "verify room root mount",
            "root must be SquashFS on the verified room image device",
        ));
    }
    Ok(())
}

fn single_threaded_init() -> Result<bool, Error> {
    let mut threads = std::fs::read_dir("/proc/self/task").map_err(|source| Error::Mount {
        operation: "inspect room init threads",
        source,
    })?;
    let Some(first) = threads.next() else {
        return Ok(false);
    };
    first.map_err(|source| Error::Mount {
        operation: "inspect room init threads",
        source,
    })?;
    Ok(threads.next().is_none())
}

fn failed(operation: &'static str) -> impl Fn(rustix::io::Errno) -> Error {
    move |source| Error::Mount {
        operation,
        source: source.into(),
    }
}

fn refused(operation: &'static str, why: &'static str) -> Error {
    Error::Mount {
        operation,
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, why),
    }
}

fn directory_at(parent: BorrowedFd<'_>, name: &str) -> Result<OwnedFd, Error> {
    // Never follow a room-writable name into a link or a nested host mount.
    rustix::fs::openat2(
        parent,
        name,
        OFlags::PATH.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC),
        Mode::empty(),
        ResolveFlags::BENEATH
            .union(ResolveFlags::NO_SYMLINKS)
            .union(ResolveFlags::NO_MAGICLINKS)
            .union(ResolveFlags::NO_XDEV),
    )
    .map_err(failed("pin room mount target"))
}

fn ensure_directory_at(parent: BorrowedFd<'_>, name: &str) -> Result<OwnedFd, Error> {
    match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => directory_at(parent, name),
        Err(source) => Err(failed("create room directory")(source)),
    }
}

fn clone_mount(source: BorrowedFd<'_>) -> Result<OwnedFd, Error> {
    // No AT_RECURSIVE: submounts underneath a source are never carried in.
    mount::open_tree(
        source,
        "",
        OpenTreeFlags::OPEN_TREE_CLONE
            .union(OpenTreeFlags::OPEN_TREE_CLOEXEC)
            .union(OpenTreeFlags::AT_EMPTY_PATH),
    )
    .map_err(failed("clone room mount"))
}

fn set_flags(tree: BorrowedFd<'_>, flags: MountAttrFlags) -> Result<(), Error> {
    let attr = libc::mount_attr {
        attr_set: u64::from(flags.bits()),
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    // SAFETY: the descriptor stays borrowed, the empty path is NUL
    // terminated, and attr points to a live Linux mount_attr of the given size.
    let result = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            tree.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            std::ptr::from_ref(&attr),
            size_of_val(&attr),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(Error::Mount {
            operation: "set room mount flags",
            source: std::io::Error::last_os_error(),
        })
    }
}

fn attach(
    tree: BorrowedFd<'_>,
    target: BorrowedFd<'_>,
    flags: MountAttrFlags,
) -> Result<(), Error> {
    set_flags(tree, flags)?;
    mount::move_mount(
        tree,
        "",
        target,
        "",
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH.union(MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH),
    )
    .map_err(failed("attach room mount"))
}

fn filesystem(
    kind: &str,
    options: &[(&str, &str)],
    flags: MountAttrFlags,
) -> Result<OwnedFd, Error> {
    let context =
        mount::fsopen(kind, FsOpenFlags::FSOPEN_CLOEXEC).map_err(failed("open room filesystem"))?;
    for (key, value) in options {
        mount::fsconfig_set_string(context.as_fd(), *key, *value)
            .map_err(failed("configure room filesystem"))?;
    }
    mount::fsconfig_create(context.as_fd()).map_err(failed("create room filesystem"))?;
    mount::fsmount(context.as_fd(), FsMountFlags::FSMOUNT_CLOEXEC, flags)
        .map_err(failed("mount room filesystem"))
}

fn add_device(dev: BorrowedFd<'_>, name: &str, minor: u32) -> Result<(), Error> {
    let source = rustix::fs::open(
        format!("/dev/{name}"),
        OFlags::PATH.union(OFlags::CLOEXEC).union(OFlags::NOFOLLOW),
        Mode::empty(),
    )
    .map_err(failed("open room device"))?;
    let stat = rustix::fs::fstat(&source).map_err(failed("identify room device"))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::CharacterDevice
        || rustix::fs::major(stat.st_rdev) != 1
        || rustix::fs::minor(stat.st_rdev) != minor
    {
        return Err(refused(
            "identify room device",
            "unexpected device identity",
        ));
    }
    let target = rustix::fs::openat(
        dev,
        name,
        OFlags::CREATE.union(OFlags::EXCL).union(OFlags::CLOEXEC),
        Mode::RUSR,
    )
    .map_err(failed("create room device mount target"))?;
    attach(clone_mount(source.as_fd())?.as_fd(), target.as_fd(), DEVICE)
}

fn enter_root(root: BorrowedFd<'_>) -> Result<(), Error> {
    rustix::process::fchdir(root).map_err(failed("enter room root"))?;
    // Stacking the old root on the new one avoids writing to the image.
    rustix::process::pivot_root(".", ".").map_err(failed("pivot into room root"))?;
    mount::unmount(".", UnmountFlags::DETACH).map_err(failed("detach old root in room"))?;
    rustix::process::chdir("/").map_err(failed("set room working directory"))
}

fn verify_view() -> Result<(), Error> {
    use StatVfsMountFlags as Flags;
    let base = Flags::NODEV.union(Flags::NOSUID);
    let data = base.union(Flags::NOEXEC);
    let read_only = data.union(Flags::RDONLY);
    for (path, required, forbidden) in [
        ("/", base.union(Flags::RDONLY), Flags::NOEXEC),
        ("/workspace", data, Flags::RDONLY),
        ("/workspace/inbox", read_only, Flags::empty()),
        ("/session", data, Flags::RDONLY),
        ("/proc", read_only, Flags::empty()),
        ("/dev", read_only, Flags::empty()),
        (
            "/dev/null",
            Flags::RDONLY.union(Flags::NOSUID).union(Flags::NOEXEC),
            Flags::NODEV,
        ),
        (
            "/dev/zero",
            Flags::RDONLY.union(Flags::NOSUID).union(Flags::NOEXEC),
            Flags::NODEV,
        ),
        (
            "/dev/urandom",
            Flags::RDONLY.union(Flags::NOSUID).union(Flags::NOEXEC),
            Flags::NODEV,
        ),
    ] {
        let seen = rustix::fs::statvfs(path)
            .map_err(failed("inspect room mount flags"))?
            .f_flag;
        if !seen.contains(required) || seen.intersects(forbidden) {
            return Err(refused("inspect room mount flags", path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
