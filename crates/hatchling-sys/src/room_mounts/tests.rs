use super::*;

#[test]
fn room_root_requires_both_the_verified_device_and_squashfs() {
    assert!(validate_image_mount(17, 17, true).is_ok());
    assert!(validate_image_mount(18, 17, true).is_err());
    assert!(validate_image_mount(17, 17, false).is_err());
}

#[test]
fn room_root_refuses_a_real_non_squashfs_descriptor() {
    let proc = std::fs::File::open("/proc").expect("pin proc");
    let device = rustix::fs::fstat(&proc)
        .expect("inspect proc device")
        .st_dev;
    assert!(matches!(
        verify_image_mount(proc.as_fd(), device),
        Err(Error::Mount {
            operation: "verify room root mount",
            ..
        })
    ));
}

#[test]
fn room_mount_plan_rejects_a_non_mount_root_before_attachment() {
    let (pipe, _write) = rustix::pipe::pipe().expect("make pipe");
    let duplicate = || pipe.try_clone().expect("clone pipe");
    assert!(matches!(
        RoomMounts::enter_with_root(
            duplicate(),
            duplicate(),
            duplicate(),
            duplicate(),
            65534,
            65534
        ),
        Err(Error::Mount {
            operation: "set room mount flags",
            ..
        })
    ));
}

#[test]
fn errors_keep_the_operation_and_failure_cause() {
    let kernel = failed("mount test")(rustix::io::Errno::NOSPC);
    let refused = refused("mount test", "wrong mount");
    assert!(matches!(
        kernel,
        Error::Mount { operation: "mount test", source }
            if source.raw_os_error() == Some(libc::ENOSPC)
    ));
    assert!(matches!(
        refused,
        Error::Mount { operation: "mount test", source }
            if source.kind() == std::io::ErrorKind::PermissionDenied
                && source.to_string() == "wrong mount"
    ));
}

#[test]
fn directories_are_resolved_under_the_pin() {
    let proc = std::fs::File::open("/proc").expect("pin proc");
    assert!(directory_at(proc.as_fd(), "self").is_err());
    assert!(directory_at(proc.as_fd(), "..").is_err());
    let pid = rustix::process::getpid().as_raw_nonzero().get().to_string();
    assert!(directory_at(proc.as_fd(), &pid).is_ok());
}

#[test]
fn single_threaded_init_refuses_another_live_thread() {
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| barrier.wait());
        let result = single_threaded_init();
        barrier.wait();
        assert!(!result.expect("inspect live test threads"));
    });
}

#[test]
fn mount_operations_refuse_unsuitable_descriptors_and_filesystems() {
    let (pipe, _write) = rustix::pipe::pipe().expect("make pipe");
    let null = std::fs::File::open("/dev/null").expect("open null");
    assert!(clone_mount(pipe.as_fd()).is_err());
    assert!(set_flags(pipe.as_fd(), IMAGE).is_err());
    assert!(attach(pipe.as_fd(), null.as_fd(), IMAGE).is_err());
    assert!(filesystem("hatchling-not-a-filesystem", &[], IMAGE).is_err());
    assert!(enter_root(null.as_fd()).is_err());
}

#[test]
fn device_setup_refuses_wrong_major_or_minor_before_creating_a_target() {
    let dev = std::fs::File::open("/dev/null").expect("open null");
    // null has major 1, minor 3; tty has major 5, minor 0.
    for (name, minor) in [("null", 5), ("tty", 0)] {
        assert!(matches!(
            add_device(dev.as_fd(), name, minor),
            Err(Error::Mount { operation: "identify room device", source })
                if source.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }
}

#[test]
fn writable_root_fails_room_mount_verification() {
    let root = rustix::fs::statvfs("/").expect("inspect test root");
    if !root.f_flag.contains(StatVfsMountFlags::RDONLY) {
        assert!(matches!(
            verify_view(),
            Err(Error::Mount { operation: "inspect room mount flags", source })
                if source.to_string() == "/"
        ));
    }
}
