//! Real dm-verity admission and teardown in a private mount namespace.

use std::fs::{self, File};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hatchling_sys::room_image::{PinnedRoomImage, RoomImageActivationError, RoomImageMapper};
use rustix::mount::{MountFlags, mount};

const IMAGE: &str = "room-system-x86_64.squashfs";
const TREE: &str = "room-system-x86_64.verity";
const CHILD: &str = "HATCHLING_ROOM_IMAGE_TEST_NAMESPACE";
const TEST: &str = "room_image_admission_pins_verifies_and_releases_the_real_image";

#[expect(
    clippy::unwrap_used,
    reason = "fixture tools must execute and emit UTF-8"
)]
fn checked(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        // A mutant may break the production close or return an error after
        // activation. Locate only mappers whose backing files belong to
        // this fixture so failed assertions cannot leak their devices.
        if cleanup_fixture_mappers(&self.0).is_ok() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn cleanup_fixture_mappers(fixture: &Path) -> std::io::Result<()> {
    let prefix = format!("{}/", fixture.display());
    for entry in fs::read_dir("/sys/block")? {
        let kernel = entry?.path();
        let Ok(name) = fs::read_to_string(kernel.join("dm/name")) else {
            continue;
        };
        let name = name.trim_end();
        if !name.starts_with("hatchling-room-verity-") {
            continue;
        }
        let slaves = fs::read_dir(kernel.join("slaves"))?.collect::<Result<Vec<_>, _>>()?;
        if slaves.len() != 2
            || !slaves.iter().all(|slave| {
                fs::read_to_string(slave.path().join("loop/backing_file"))
                    .is_ok_and(|backing| backing.starts_with(&prefix))
            })
        {
            continue;
        }
        let status = Command::new("/usr/sbin/veritysetup")
            .args(["close", name])
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!("could not clean up {name}")));
        }
    }
    Ok(())
}

struct Mounted(PathBuf);

impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = hatchling_sys::unmount_strict(&self.0);
    }
}

#[expect(clippy::unwrap_used, reason = "a kernel fixture requires sysfs")]
fn mapper_names() -> Vec<String> {
    let mut names = fs::read_dir("/sys/block")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_to_string(entry.path().join("dm/name")).ok())
        .filter(|name| name.starts_with("hatchling-room-verity-"))
        .collect::<Vec<_>>();
    names.sort_unstable();
    names
}

#[expect(
    clippy::unwrap_used,
    reason = "building the fixture must succeed before the test"
)]
fn fixture(dir: &Path) -> (String, String) {
    let source = dir.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("marker"), b"authenticated room bytes").unwrap();
    // A one-block image needs no on-disk hash tree. Keep enough nonzero
    // uncompressed data for the fixture to exercise the second file.
    fs::write(source.join("padding"), vec![1; 32_768]).unwrap();
    let image = dir.join(IMAGE);
    let tree = dir.join(TREE);
    let image = image.to_str().unwrap();
    checked(
        "mksquashfs",
        &[
            source.to_str().unwrap(),
            image,
            "-noappend",
            "-noD",
            "-no-fragments",
            "-no-progress",
            "-processors",
            "1",
        ],
    );
    let sums = checked("sha256sum", &[image]);
    let salt = sums.split_ascii_whitespace().next().unwrap().to_owned();
    let root = dir.join("expected-root");
    checked(
        "veritysetup",
        &[
            "format",
            "--no-superblock",
            "--format=1",
            "--hash=sha256",
            "--data-block-size=4096",
            "--hash-block-size=4096",
            "--salt",
            &salt,
            "--root-hash-file",
            root.to_str().unwrap(),
            image,
            tree.to_str().unwrap(),
        ],
    );
    (fs::read_to_string(root).unwrap().trim().to_owned(), salt)
}

#[expect(
    clippy::unwrap_used,
    reason = "a hand-broken fixture must change the intended byte"
)]
fn corrupt(path: &Path) {
    let mut file = File::options().read(true).write(true).open(path).unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.rewind().unwrap();
    let [value] = byte;
    file.write_all(&[value ^ 1]).unwrap();
}

#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the proof must clean up a hand-broken mapper before failing"
)]
fn expect_verification_refusal(pinned: PinnedRoomImage, root: &str, salt: &str, case: &str) {
    let mut refused = pinned.activate(root, salt);
    if let Ok(mapper) = &mut refused {
        mapper.close().unwrap();
    }
    let reason = refused.expect_err("activation proceeded without successful verification");
    assert!(reason.to_string().contains("verify"), "{case}: {reason}");
}

#[expect(
    clippy::unwrap_used,
    reason = "the real verity tool must accept the controlled altered geometry"
)]
fn substitute_hash_block(image: &Path, tree: &Path, root: &str, salt: &str) {
    let mut block = [0; 4096];
    File::open(tree).unwrap().read_exact(&mut block).unwrap();
    fs::write(image, block).unwrap();
    checked(
        "veritysetup",
        &[
            "verify",
            "--no-superblock",
            "--format=1",
            "--hash=sha256",
            "--data-block-size=4096",
            "--hash-block-size=4096",
            "--salt",
            salt,
            image.to_str().unwrap(),
            tree.to_str().unwrap(),
            root,
        ],
    );
}

#[expect(
    clippy::unwrap_used,
    reason = "a root fixture must start in a private namespace"
)]
fn in_private_mount_namespace() -> bool {
    if !rustix::process::geteuid().is_root() {
        assert!(std::env::var_os("HATCHLING_REQUIRE_ROOT_MOUNTS").is_none());
        return false;
    }
    let namespace = fs::read_link("/proc/self/ns/mnt").unwrap();
    let Some(parent) = std::env::var_os(CHILD) else {
        let result = Command::new("unshare")
            .args(["--mount", "--propagation", "private"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, namespace)
            .status()
            .unwrap();
        assert!(result.success(), "private room image fixture failed");
        return false;
    };
    assert_ne!(namespace, PathBuf::from(parent));
    assert_eq!(
        checked("findmnt", &["-n", "-o", "PROPAGATION", "/"]).trim(),
        "private"
    );
    true
}

#[test]
fn room_image_admission_pins_verifies_and_releases_the_real_image() {
    if !in_private_mount_namespace() {
        return;
    }
    let path = PathBuf::from(format!(
        "/var/tmp/hatchling-room-image-test.{}",
        std::process::id()
    ));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    let scratch = Scratch(path);
    let (root, salt) = fixture(&scratch.0);
    let directory = File::open(&scratch.0).unwrap();
    let pinned = || PinnedRoomImage::pin(directory.as_fd());
    let before = mapper_names();
    let wrong_root = if root.starts_with('0') {
        "1".repeat(64)
    } else {
        "0".repeat(64)
    };
    expect_verification_refusal(pinned().unwrap(), &wrong_root, &salt, "wrong root");
    assert_eq!(mapper_names(), before);
    expect_verification_refusal(pinned().unwrap(), &root, &wrong_root, "wrong salt");
    assert_eq!(mapper_names(), before);

    let image = scratch.0.join(IMAGE);
    let tree = scratch.0.join(TREE);
    fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o770)).unwrap();
    let exposed = pinned();
    fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        exposed.is_err(),
        "a writable installation directory was accepted"
    );
    fs::set_permissions(&image, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(pinned().is_err());
    fs::set_permissions(&image, fs::Permissions::from_mode(0o644)).unwrap();
    let originals = pinned().unwrap();
    let original_image = scratch.0.join("original-image");
    let original_tree = scratch.0.join("original-tree");
    fs::rename(&image, &original_image).unwrap();
    fs::rename(&tree, &original_tree).unwrap();
    assert!(pinned().is_err(), "a missing image was accepted");
    fs::copy(&original_tree, &tree).unwrap();
    symlink("original-image", &image).unwrap();
    assert!(pinned().is_err(), "an image symlink was accepted");
    fs::remove_file(&image).unwrap();
    substitute_hash_block(&image, &tree, &root, &salt);
    expect_verification_refusal(
        pinned().unwrap(),
        &root,
        &salt,
        "hash block presented as the image",
    );
    assert_eq!(mapper_names(), before);
    fs::copy(&original_image, &image).unwrap();
    fs::remove_file(original_image).unwrap();
    fs::remove_file(original_tree).unwrap();
    corrupt(&tree);
    expect_verification_refusal(pinned().unwrap(), &root, &salt, "modified hash tree");
    assert_eq!(mapper_names(), before);
    corrupt(&tree);
    corrupt(&image);
    expect_verification_refusal(pinned().unwrap(), &root, &salt, "modified image");
    assert_eq!(mapper_names(), before);
    corrupt(&tree);

    mount_and_retry_cleanup(
        originals.activate(&root, &salt).unwrap(),
        &scratch.0,
        &before,
    );
}

#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "every step in the real mapper setup and teardown must succeed"
)]
fn mount_and_retry_cleanup(mut mapper: RoomImageMapper, fixture: &Path, before: &[String]) {
    let name = mapper.device().file_name().unwrap().to_owned();
    let mountpoint = fixture.join("mount");
    fs::create_dir(&mountpoint).unwrap();
    mount(
        mapper.device(),
        &mountpoint,
        "squashfs",
        MountFlags::RDONLY
            .union(MountFlags::NODEV)
            .union(MountFlags::NOSUID),
        "",
    )
    .unwrap();
    let mounted = Mounted(mountpoint);
    assert_eq!(
        fs::read(mounted.0.join("marker")).unwrap(),
        b"authenticated room bytes"
    );
    assert!(
        mapper.close().is_err(),
        "a mounted image was reported closed"
    );
    assert_eq!(mapper_names().len(), before.len() + 1);
    let setup_error = mount(
        mapper.device(),
        fixture.join("missing-launch-root"),
        "squashfs",
        MountFlags::RDONLY,
        "",
    )
    .unwrap_err();
    let failure = mapper.rollback(setup_error.into());
    let mut mapper = match failure {
        RoomImageActivationError::StartupCleanup {
            mapper,
            cleanup: Some(_),
            ..
        } => Some(mapper),
        RoomImageActivationError::BeforeActivation(_)
        | RoomImageActivationError::StartupCleanup { .. } => None,
    }
    .expect("busy startup rollback did not retain mapper ownership");
    assert_eq!(mapper.name(), name);
    hatchling_sys::unmount_strict(&mounted.0).unwrap();
    mapper.close().unwrap();
    mapper.close().unwrap();
    assert_eq!(mapper_names(), before);
}
