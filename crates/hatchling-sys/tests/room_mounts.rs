//! Root integration probe for the room mount view.
//!
//! Run this test alone in PID 1 of a disposable PID namespace with
//! `HATCHLING_ROOM_MOUNT_FIXTURE` pointing at a fixture containing the
//! verified image mount, encrypted workspace, inbox source and staging.

use std::fs;
use std::io::Write;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use hatchling_sys::room_image::{PinnedRoomImage, RoomImageActivationError};
use hatchling_sys::room_mounts::RoomMounts;
use rustix::fs::{Mode, OFlags};

#[allow(clippy::panic, reason = "a misconfigured harness-free test must fail")]
fn main() {
    match std::env::var("HATCHLING_ROOM_MOUNT_MODE").as_deref() {
        Ok("view") => room_mount_view(),
        Ok("verified") => room_mount_verified_view(),
        Ok("threads") => rejects_namespace_preparation_with_another_thread(),
        Err(std::env::VarError::NotPresent) => {
            assert!(std::env::var_os("HATCHLING_REQUIRE_ROOM_MOUNTS").is_none());
            room_mounts_rejects_namespace_preparation_outside_init();
        }
        _ => panic!("invalid room mount probe mode"),
    }
}

fn room_mounts_rejects_namespace_preparation_outside_init() {
    assert_ne!(rustix::process::getpid().as_raw_nonzero().get(), 1);
    assert!(RoomMounts::prepare_namespace().is_err());
}

fn rejects_namespace_preparation_with_another_thread() {
    assert_eq!(rustix::process::getpid().as_raw_nonzero().get(), 1);
    let ready = std::sync::Barrier::new(2);
    let release = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let refused = RoomMounts::prepare_namespace().is_err();
            ready.wait();
            release.wait();
            refused
        });
        ready.wait();
        let refused = RoomMounts::prepare_namespace().is_err();
        release.wait();
        assert!(refused, "PID 1 must refuse setup with a sibling thread");
        assert!(matches!(worker.join(), Ok(true)));
    });
}

fn pin(path: &Path) -> rustix::io::Result<OwnedFd> {
    rustix::fs::open(
        path,
        OFlags::PATH
            .union(OFlags::DIRECTORY)
            .union(OFlags::CLOEXEC)
            .union(OFlags::NOFOLLOW),
        Mode::empty(),
    )
}

#[allow(clippy::expect_used, reason = "a failed mount probe must exit PID 1")]
fn room_mount_verified_view() {
    let fixture = std::env::var_os("HATCHLING_ROOM_MOUNT_FIXTURE").expect("mount fixture path");
    let fixture = Path::new(&fixture);
    let install = pin(&fixture.join("install")).expect("pin fixture install");
    let image = PinnedRoomImage::pin(install.as_fd()).expect("pin fixture image and tree");
    drop(install);
    let root = std::env::var("HATCHLING_ROOM_PROOF_ROOT").expect("fixture root hash");
    let digest = std::env::var("HATCHLING_ROOM_PROOF_DIGEST").expect("fixture image digest");
    let activated = image.activate(&root, &digest);
    let name = match &activated {
        Ok(mapper) => mapper
            .device()
            .file_name()
            .and_then(std::ffi::OsStr::to_str),
        Err(RoomImageActivationError::StartupCleanup { mapper, .. }) => Some(mapper.name()),
        Err(RoomImageActivationError::BeforeActivation(_)) => None,
    };
    if let Some(name) = name {
        let mut names = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(fixture.join("verified-mapper"))
            .expect("open mapper cleanup record");
        writeln!(names, "{name}").expect("record mapper for fixture cleanup");
    }
    let mapper = activated.expect("activate verified image");
    // Device-mapper's udev rendezvous runs before room IPC isolation.
    hatchling_sys::unshare_ipc().expect("enter the room IPC namespace");
    let namespace = RoomMounts::prepare_namespace().expect("prepare private room namespace");
    let expected_device = fs::metadata(mapper.device())
        .expect("identify verified mapper")
        .rdev();
    let device =
        std::env::var("HATCHLING_ROOM_PROOF_WORKSPACE_DEVICE").expect("fixture volume device");
    let volume = if std::env::var_os("HATCHLING_ROOM_PROOF_SUBSTITUTE_VOLUME").is_some() {
        fixture.join("substitute")
    } else {
        fixture.join("disk")
    };
    let entered = RoomMounts::enter_verified_volume(
        namespace,
        &mapper,
        &volume,
        &device,
        pin(&fixture.join("stage")).expect("pin staging directory"),
        65534,
        65534,
    );
    if let Ok(expected) = std::env::var("HATCHLING_ROOM_PROOF_SOURCE_ERROR") {
        let error = entered.expect_err("reject the substituted room source before entry");
        assert!(
            matches!(error, hatchling_sys::Error::Mount { operation, .. } if operation == expected),
            "{error}"
        );
        return;
    }
    entered.expect("enter with verified image and encrypted sources");
    assert_eq!(
        fs::metadata("/").expect("inspect room root").dev(),
        expected_device
    );
    old_root_is_not_reachable(fixture);
    hatchling_sys::clear_groups().expect("drop inherited groups");
    hatchling_sys::drop_to(65534, 65534).expect("drop to fixture room user");
    assert_eq!(
        fs::read_to_string("/workspace/inbox/fixture").expect("read verified inbox"),
        "inbox-content"
    );
    assert!(
        Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .status()
            .expect("execute from the verified mount")
            .success()
    );
}

#[allow(clippy::expect_used, reason = "a failed mount probe must exit PID 1")]
fn room_mount_view() {
    let fixture = std::env::var_os("HATCHLING_ROOM_MOUNT_FIXTURE").expect("mount fixture path");
    let fixture = Path::new(&fixture);
    assert!(fixture.join("outside-canary").is_file());
    let namespace = RoomMounts::prepare_namespace().expect("prepare private room namespace");
    let plan = RoomMounts {
        image: pin(&fixture.join("image")).expect("pin fixture image"),
        workspace: pin(&fixture.join("disk/workspace")).expect("pin fixture workspace"),
        inbox: pin(&fixture.join("disk/inbox")).expect("pin fixture inbox"),
        staging: pin(&fixture.join("stage")).expect("pin staging directory"),
        uid: 65534,
        gid: 65534,
    };
    let forbidden = std::env::var_os("HATCHLING_ROOM_BAD_INBOX").is_some();
    if forbidden {
        let error = plan
            .enter(namespace)
            .expect_err("reject the planted inbox target");
        assert!(
            matches!(
                error,
                hatchling_sys::Error::Mount {
                    operation: "pin room mount target",
                    ..
                }
            ),
            "{error}"
        );
        return;
    }
    plan.enter(namespace).expect("enter room view");
    let root_write = fs::write("/room-mount-probe", b"x").expect_err("root must be read-only");
    assert_eq!(root_write.raw_os_error(), Some(libc::EROFS));
    old_root_is_not_reachable(fixture);
    if std::env::var_os("HATCHLING_ROOM_WRITABLE_IMAGE_FIXTURE").is_some() {
        return;
    }
    hatchling_sys::clear_groups().expect("drop inherited groups");
    hatchling_sys::drop_to(65534, 65534).expect("drop to fixture room user");
    assert_eq!(
        fs::read_to_string("/workspace/inbox/fixture").expect("read inbox"),
        "inbox-content"
    );
    let inbox_write =
        fs::write("/workspace/inbox/refused", b"x").expect_err("inbox must be read-only");
    assert_eq!(inbox_write.raw_os_error(), Some(libc::EROFS));
    assert!(fs::write("/etc/room-mount-probe", b"x").is_err());
    assert!(!Path::new("/proc/sys").exists());
    assert!(!Path::new("/dev/shm").exists());
    assert!(!Path::new("/dev/pts").exists());
    assert!(!fixture.exists());
    assert!(!Path::new("/workspace/host-mount/canary").exists());
    assert!(!Path::new("/session/marker").exists());
    fs::write("/workspace/marker", b"persistent").expect("write workspace");
    fs::write("/session/marker", b"ephemeral").expect("write session");
    let scratch = rustix::fs::statvfs("/session").expect("measure session");
    assert_eq!(scratch.f_blocks * scratch.f_frsize, 512 * 1024 * 1024);
    fs::copy("/bin/sh", "/session/payload").expect("copy image binary");
    for path in ["/workspace/payload", "/session/payload"] {
        let error = Command::new(path)
            .arg("-c")
            .arg("exit 17")
            .status()
            .expect_err("writable mounts must not run binaries");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }
    let output = Command::new("/bin/sh").env_clear().env("PATH", "/bin:/usr/bin").args(["-ec",
        "head -c 1 /dev/urandom >/dev/null; head -c 1 /dev/zero >/dev/null; test -r /proc/self/status"
    ]).status().expect("execute from image");
    assert!(output.success());
    if std::env::var_os("HATCHLING_ROOM_FILL_SCRATCH").is_some() {
        let mut file = fs::File::create("/session/full").expect("create scratch file");
        let chunk = vec![1; 1024 * 1024];
        let error = (0..=512)
            .find_map(|_| file.write_all(&chunk).err())
            .expect("scratch writes must fail at the tmpfs cap");
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(
            rustix::fs::statvfs("/session")
                .expect("inspect full scratch")
                .f_bavail,
            0
        );
        assert_eq!(
            fs::read_to_string("/workspace/marker").expect("workspace still available"),
            "persistent"
        );
    }
}

#[allow(
    clippy::expect_used,
    reason = "the root alias test must fail if an expected path is inaccessible"
)]
fn old_root_is_not_reachable(fixture: &Path) {
    let root = fs::metadata("/").expect("stat the room root");
    let relative = fixture.strip_prefix("/").expect("absolute fixture path");
    let tasks = fs::read_dir("/proc/1/task").expect("enumerate room init tasks");
    let mut aliases = vec!["/proc/1/root".into(), "/proc/self/root".into()];
    let mut count = 0;
    for task in tasks {
        let task = task.expect("inspect init task");
        count += 1;
        aliases.push(task.path().join("root"));
    }
    assert_eq!(count, 1, "the room starts with exactly one task");
    for alias in aliases {
        let target = fs::metadata(&alias).expect("follow the proc root alias");
        assert_eq!((root.dev(), root.ino()), (target.dev(), target.ino()));
        let error = fs::metadata(alias.join(relative).join("outside-canary"))
            .expect_err("the old root canary must be outside the room");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
