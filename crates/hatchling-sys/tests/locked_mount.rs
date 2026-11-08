//! A locked-down mount, made for real (0.9's room disk): root's alone.
//!
//! As root, a tmpfs is mounted with [`hatchling_sys::mount_locked_down`]:
//! the kernel reports `nodev`, `nosuid` and `noexec` of it, a program
//! written into it can't run, a second mount over it is refused, an
//! unmount while something holds it open is refused and leaves it mounted,
//! and one that's free takes it down. A mount made without the flags is
//! never taken as locked down.
//!
//! Not as root, mounting is refused, and these are skipped, saying so,
//! unless `HATCHLING_REQUIRE_ROOT_MOUNTS=1` asks for them to run (the root
//! pass sets it): then a skip is a failure.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use hatchling_sys::{is_mount_point, mount_locked_down, unmount_strict, verify_locked_down};

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
    drop(held);
    unmount_strict(target).unwrap();
    assert!(!is_mount_point(target).unwrap());
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
