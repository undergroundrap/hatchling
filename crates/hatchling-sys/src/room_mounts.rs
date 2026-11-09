//! Build the WO-034 room view from descriptors held by the trusted launcher.
//!
//! The verified system image becomes `/`. The encrypted workspace and
//! its separately supplied read-only inbox are attached below it. This
//! plan is separate from the ordinary agent cell's tmpfs root.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

use rustix::fs::{FileType, Mode, OFlags, ResolveFlags, StatVfsMountFlags};
use rustix::mount::{
    self, FsMountFlags, FsOpenFlags, MountAttrFlags, MountPropagationFlags, MoveMountFlags,
    OpenTreeFlags, UnmountFlags,
};

use crate::Error;

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
    /// Create the room's private mount namespace in a disposable launch child.
    /// The caller must be the sole task in its new PID namespace and must not
    /// create threads or children before the root switch completes.
    /// Open all mount sources after this succeeds. The kernel refuses to clone
    /// mount descriptors opened in the previous mount namespace.
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
        attach(root.as_fd(), self.staging.as_fd(), IMAGE)?;
        let workspace_at = directory_at(root.as_fd(), "workspace")?;
        let workspace = clone_mount(self.workspace.as_fd())?;
        attach(workspace.as_fd(), workspace_at.as_fd(), DATA)?;
        let inbox_at = directory_at(workspace.as_fd(), "inbox")?;
        let inbox = clone_mount(self.inbox.as_fd())?;
        attach(inbox.as_fd(), inbox_at.as_fd(), READ_ONLY_DATA)?;

        let uid = self.uid.to_string();
        let gid = self.gid.to_string();
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
        drop(self);
        drop(workspace_at);
        drop(inbox_at);
        drop(workspace);
        drop(inbox);
        enter_root(root.as_fd())?;
        drop(root);
        verify_view()
    }
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
