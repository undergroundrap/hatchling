//! A locked-down mount, made for real (0.9's room disk): root's alone.
//!
//! As root, a tmpfs is mounted with [`hatchling_sys::mount_locked_down`]:
//! the kernel reports `nodev`, `nosuid` and `noexec` of it, a program
//! written into it can't run, a second mount over it is refused, an
//! unmount while something holds it open is refused and leaves it mounted,
//! and one that's free takes it down. A mount made without the flags is
//! never taken as locked down.
//!
//! And a mount made in a directory comes down in a mount namespace made
//! after it, when the directory's mounts were shared first
//! ([`hatchling_sys::share_below`]), on a mount that isn't shared: without
//! that, the copy stays.
//!
//! Not as root, mounting is refused, and these are skipped, saying so,
//! unless `HATCHLING_REQUIRE_ROOT_MOUNTS=1` asks for them to run (the root
//! pass sets it): then a skip is a failure.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use hatchling_sys::{
    is_mount_point, mount_locked_down, share_below, unmount_strict, unshare_below,
    verify_locked_down,
};

/// Whether these run here: as root. Not as root, they're skipped, unless
/// the run asked for them.
fn as_root(test: &str) -> bool {
    if hatchling_sys::euid() == 0 {
        return true;
    }
    assert!(
        std::env::var("HATCHLING_REQUIRE_ROOT_MOUNTS").as_deref() != Ok("1"),
        "{test} must run, and it needs root"
    );
    let _ = std::io::Write::write_all(
        &mut std::io::stderr(),
        format!("skipping: {test} mounts, which needs root\n").as_bytes(),
    );
    false
}

/// A directory of a test's own, with whatever is mounted on it taken
/// down, lazily if need be, when it goes.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-locked-mount-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        while is_mount_point(&self.0).unwrap_or(false) {
            if rustix::mount::unmount(&self.0, rustix::mount::UnmountFlags::DETACH).is_err() {
                break;
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A tmpfs mounted locked down is reported so, runs nothing written into
/// it, takes no second mount over it, and comes down only when nothing
/// holds it.
#[test]
fn a_locked_down_mount_holds_and_comes_down_only_when_free() {
    if !as_root("a_locked_down_mount_holds_and_comes_down_only_when_free") {
        return;
    }
    let scratch = Scratch::new("holds").unwrap();
    let target = scratch.0.as_path();
    mount_locked_down(Path::new("tmpfs"), target, "tmpfs", "size=1m,mode=0700").unwrap();
    assert!(is_mount_point(target).unwrap());
    verify_locked_down(target).unwrap();

    let program = target.join("program");
    std::fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ran = std::process::Command::new(&program).status();
    assert_eq!(
        ran.map_err(|error| error.kind()).err(),
        Some(std::io::ErrorKind::PermissionDenied),
        "a program on a noexec mount ran, or failed for another reason"
    );

    let again = mount_locked_down(Path::new("tmpfs"), target, "tmpfs", "size=1m");
    assert!(
        again
            .unwrap_err()
            .to_string()
            .contains("a mount point already"),
        "a second mount was stacked over the first"
    );

    let held = std::fs::File::open(&program).unwrap();
    assert!(unmount_strict(target).is_err(), "a mount in use came down");
    assert!(
        is_mount_point(target).unwrap(),
        "a mount in use was taken from its path"
    );
    // It is in use until every copy of the open file is closed. A copy is
    // kept here, as a child another test forks keeps one for the moment
    // before it goes on to its own program, and let go a little later.
    let copy = held.try_clone().unwrap();
    drop(held);
    assert!(
        unmount_strict(target).is_err(),
        "a mount a copy of its file held open came down"
    );
    let let_go = Arc::new(AtomicBool::new(false));
    let later = {
        let let_go = Arc::clone(&let_go);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let_go.store(true, Ordering::SeqCst);
            drop(copy);
        })
    };
    unmount_once_free(target).unwrap();
    assert!(
        let_go.load(Ordering::SeqCst),
        "the mount came down with a copy of its file still open"
    );
    later.join().unwrap();
    assert!(!is_mount_point(target).unwrap());
}

/// How long a mount nothing should hold may take to be free.
const FREE_WITHIN: Duration = Duration::from_secs(10);

/// Takes down the mount at `target` as [`unmount_strict`] does, once
/// nothing holds it. These tests run in parallel in one process, and some
/// start a program: the child forked for it has a copy of every file this
/// process has open until it goes on to that program, so a file closed
/// here may hold its mount a moment longer. A mount still in use after
/// [`FREE_WITHIN`], or refused for any other reason, is the error it was.
fn unmount_once_free(target: &Path) -> Result<(), hatchling_sys::Error> {
    let until = Instant::now() + FREE_WITHIN;
    loop {
        match unmount_strict(target) {
            Err(hatchling_sys::Error::Mount { source, .. })
                if source.raw_os_error() == Some(libc::EBUSY) && Instant::now() < until =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            done => return done,
        }
    }
}

/// A mount made without the three flags is never taken as locked down:
/// each missing one is named.
#[test]
fn a_mount_without_the_flags_is_not_locked_down() {
    if !as_root("a_mount_without_the_flags_is_not_locked_down") {
        return;
    }
    let scratch = Scratch::new("open").unwrap();
    let target = scratch.0.as_path();
    rustix::mount::mount(
        "tmpfs",
        target,
        "tmpfs",
        rustix::mount::MountFlags::NOSUID,
        "size=1m",
    )
    .unwrap();
    let said = verify_locked_down(target).unwrap_err().to_string();
    assert!(
        said.contains("nodev") && said.contains("noexec") && !said.contains("nosuid"),
        "the missing flags weren't named, or a present one was"
    );
    unmount_strict(target).unwrap();
}

/// A process in a mount namespace of its own, made as a hardened service's
/// is: a copy of every mount, each a slave of the one it was copied from.
struct Service(std::process::Child);

impl Service {
    fn start() -> std::io::Result<Self> {
        let child = std::process::Command::new("unshare")
            .args(["--mount", "--propagation", "slave", "sleep", "60"])
            .spawn()?;
        let service = Self(child);
        let own = std::fs::read_link("/proc/self/ns/mnt")?;
        // It's a service once it's in its own namespace and is `sleep`.
        for _ in 0..500 {
            let theirs = std::fs::read_link(format!("/proc/{}/ns/mnt", service.0.id())).ok();
            let name = std::fs::read_to_string(format!("/proc/{}/comm", service.0.id()));
            if theirs.is_some_and(|theirs| theirs != own)
                && name.is_ok_and(|name| name == "sleep\n")
            {
                return Ok(service);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(std::io::Error::other(
            "the service never had a namespace of its own",
        ))
    }

    /// Whether something is mounted at `path` in the service's namespace.
    fn sees_mounted(&self, path: &Path) -> std::io::Result<bool> {
        let mounts = std::fs::read_to_string(format!("/proc/{}/mountinfo", self.0.id()))?;
        let path = path.to_string_lossy();
        Ok(mounts
            .lines()
            .any(|line| line.split(' ').nth(4) == Some(&*path)))
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A private mount to stand for a machine's root that isn't shared, with a
/// room directory in it.
fn private_root(name: &str) -> std::io::Result<(Scratch, PathBuf)> {
    let scratch = Scratch::new(name)?;
    rustix::mount::mount_bind(&scratch.0, &scratch.0)?;
    rustix::mount::mount_change(&scratch.0, rustix::mount::MountPropagationFlags::PRIVATE)?;
    let room = scratch.0.join("room");
    std::fs::create_dir(&room)?;
    std::fs::create_dir(room.join("mnt"))?;
    Ok((scratch, room))
}

/// On a machine whose root mount isn't shared (WSL, a container), a
/// service started while the room disk was mounted kept its own copy of
/// the mount: the room disk came down here, stayed mounted there, and
/// couldn't be closed. With the room directory's mounts shared first, the
/// unmount reaches the copy.
#[test]
fn a_mount_comes_down_in_a_namespace_made_after_it() {
    if !as_root("a_mount_comes_down_in_a_namespace_made_after_it") {
        return;
    }
    // The fault, first: on a mount that isn't shared, the copy stays.
    {
        let (_scratch, room) = private_root("copy-stays").unwrap();
        let mount = room.join("mnt");
        mount_locked_down(Path::new("tmpfs"), &mount, "tmpfs", "size=1m").unwrap();
        let service = Service::start().unwrap();
        assert!(
            service.sees_mounted(&mount).unwrap(),
            "the service took no copy"
        );
        unmount_strict(&mount).unwrap();
        assert!(
            service.sees_mounted(&mount).unwrap(),
            "the copy went by itself: this machine doesn't show the fault"
        );
    }
    // Shared first, the unmount reaches it.
    let (_scratch, room) = private_root("copy-goes").unwrap();
    let mount = room.join("mnt");
    assert!(
        share_below(&room).unwrap(),
        "a private mount needed no bind"
    );
    assert!(is_mount_point(&room).unwrap());
    assert!(
        !share_below(&room).unwrap(),
        "shared already, and bound again"
    );
    mount_locked_down(Path::new("tmpfs"), &mount, "tmpfs", "size=1m").unwrap();
    verify_locked_down(&mount).unwrap();
    let service = Service::start().unwrap();
    assert!(
        service.sees_mounted(&mount).unwrap(),
        "the service took no copy"
    );
    unmount_strict(&mount).unwrap();
    assert!(
        !service.sees_mounted(&mount).unwrap(),
        "the mount came down here, and its copy stayed in the service"
    );
    // And the directory's own bind goes, once nothing's mounted in it.
    assert!(unshare_below(&room).unwrap());
    assert!(!is_mount_point(&room).unwrap());
    assert!(
        !unshare_below(&room).unwrap(),
        "nothing there, and taken down"
    );
}

/// A directory on a mount that's shared already needs nothing, and nothing
/// is mounted on it; and only a directory's own bind is taken down, never
/// another file system mounted there.
#[test]
fn a_shared_mount_is_left_as_it_is_and_another_mount_is_never_taken_down() {
    if !as_root("a_shared_mount_is_left_as_it_is_and_another_mount_is_never_taken_down") {
        return;
    }
    let (scratch, room) = private_root("shared-already").unwrap();
    rustix::mount::mount_change(&scratch.0, rustix::mount::MountPropagationFlags::SHARED).unwrap();
    assert!(
        !share_below(&room).unwrap(),
        "a shared mount was bound over"
    );
    assert!(!is_mount_point(&room).unwrap());
    // A file system of its own at the room directory isn't this code's.
    mount_locked_down(Path::new("tmpfs"), &room, "tmpfs", "size=1m").unwrap();
    assert!(
        !unshare_below(&room).unwrap(),
        "another mount was taken down"
    );
    assert!(is_mount_point(&room).unwrap());
    unmount_strict(&room).unwrap();
}
