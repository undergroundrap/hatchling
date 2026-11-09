//! The room disk, made for real (0.9): root's alone.
//!
//! As root, with the real cryptsetup and mke2fs, and the real vault keeper
//! giving each image's key: a 64 MiB room disk is made, opened and mounted
//! `nodev,nosuid,noexec`, refuses to run a program, holds a file, is
//! refused a close while that file is open, closes, and is opaque closed;
//! it opens again with its key, maps nothing with another vault's, and is
//! wiped. Its key slot moves from one vault's key to another's, closed and
//! then open, without touching what it holds. An image that's a link to a
//! file of the test's own is refused every op, and the file is left exactly
//! as it was.
//!
//! Not as root these are skipped, saying so, unless
//! `HATCHLING_REQUIRE_ROOM_DISK=1` asks for them (the root pass sets it):
//! then a skip is a failure. Everything here is made under a directory of
//! the test's own in `/var/lib`, never anywhere else.

use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use hatchling_broker::room_disk::{self, RoomDisk, RoomDiskOp, RoomDiskState, Rotated};
use hatchling_core::framing::{read_frame, write_frame};

/// A room disk's size here: the smallest there is.
const SIZE: u64 = 64 * 1024 * 1024;

/// Whether these run here: as root. Not as root, they're skipped, unless
/// the run asked for them.
fn as_root(test: &str) -> bool {
    if rustix::process::geteuid().is_root() {
        return true;
    }
    assert!(
        std::env::var("HATCHLING_REQUIRE_ROOM_DISK").as_deref() != Ok("1"),
        "{test} must run, and it needs root"
    );
    let _ = std::io::Write::write_all(
        &mut std::io::stderr(),
        format!("skipping: {test} makes a room disk, which needs root\n").as_bytes(),
    );
    false
}

/// A directory of the test's own, root's and searchable, under `/var/lib`:
/// what's made there, a room disk's mount and mapping among it, goes with
/// it.
struct Place {
    dir: PathBuf,
    disk: RoomDisk,
}

/// What a helper here fails with.
type Failed = Box<dyn std::error::Error>;

impl Place {
    fn new(tag: &str) -> Result<Self, Failed> {
        let dir = PathBuf::from(format!(
            "/var/lib/hatchling-room-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
        let disk = RoomDisk::at(dir.join("room"), SIZE)?;
        Ok(Self { dir, disk })
    }

    /// The mapping's name, once there's an image.
    fn mapping(&self) -> Option<String> {
        let head = hatchling_sys::read_file_head(&self.disk.image(), 208).ok()?;
        let uuid = std::str::from_utf8(head.get(168..204)?)
            .ok()?
            .parse()
            .ok()?;
        Some(room_disk::mapping_name(&uuid))
    }

    /// Whether the mapping is there, as the kernel lists it.
    fn mapped(&self) -> bool {
        self.mapping()
            .is_some_and(|name| Path::new("/dev/mapper").join(name).exists())
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let mount = self.disk.mount_point();
        if hatchling_sys::is_mount_point(&mount).unwrap_or(false) {
            let _ = Command::new("umount").arg("-l").arg(&mount).status();
        }
        if let Some(name) = self.mapping() {
            let _ = Command::new("/usr/sbin/cryptsetup")
                .args(["close", &name])
                .stderr(Stdio::null())
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A vault keeper of its own, under `place`, as the broker starts it, and
/// its end of the spawner's channel: its master key is its own vault's.
struct Keeper {
    child: Child,
    _broker: UnixStream,
    spawner: Mutex<UnixStream>,
}

impl Keeper {
    fn start(place: &Place, tag: &str) -> Result<Self, Failed> {
        let home = place.dir.join(tag);
        for dir in ["", "vault", "vault.key", "sock", "tmp"] {
            std::fs::create_dir_all(home.join(dir))?;
            std::fs::set_permissions(home.join(dir), std::fs::Permissions::from_mode(0o700))?;
        }
        let path = |name: &str| home.join(name).to_string_lossy().into_owned();
        let (broker, brokers) = UnixStream::pair()?;
        let (spawner, spawners) = UnixStream::pair()?;
        for end in [brokers.as_fd(), spawners.as_fd()] {
            hatchling_sys::keep_across_exec(end)?;
        }
        let child = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
            .arg("vault-keeper")
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin")
            .env("TMPDIR", path("tmp"))
            .env("HATCHLING_BROKER_SOCKET", path("sock/broker.sock"))
            .env("HATCHLING_AGENT_UID", "4242")
            .env("HATCHLING_LOG_LEVEL", "error")
            .env("HATCHLING_VAULT_DIR", path("vault"))
            .env("HATCHLING_KEEPER_FD", brokers.as_raw_fd().to_string())
            .env(
                "HATCHLING_KEEPER_SPAWNER_FD",
                spawners.as_raw_fd().to_string(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        drop((brokers, spawners));
        broker.set_read_timeout(Some(Duration::from_secs(30)))?;
        // The keeper asks for the broker's stores' keys: there are none.
        let asked = read_frame(&broker)?;
        if asked.first() != Some(&2) {
            return Err(Failed::from("the keeper didn't start"));
        }
        write_frame(&broker, &hatchling_vault::handoff::kept_bytes(&[])?)?;
        let handed = read_frame(&broker)?;
        if handed.first() != Some(&0) {
            return Err(Failed::from("the keeper didn't hand over"));
        }
        Ok(Self {
            child,
            _broker: broker,
            spawner: Mutex::new(spawner),
        })
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The mount options the kernel lists for `mount`.
fn mount_options(mount: &Path) -> Option<String> {
    let listed = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let mount = mount.to_string_lossy().into_owned();
    listed.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(' ').collect();
        if fields.get(4) == Some(&mount.as_str()) {
            fields.get(5).map(|options| (*options).to_owned())
        } else {
            None
        }
    })
}

/// Release only this test's extra bind, even when an assertion fails.
/// Use the system tool so a hand mutant of `unmount_strict` can't skip cleanup.
struct HeldBind(PathBuf);

impl Drop for HeldBind {
    fn drop(&mut self) {
        let _ = Command::new("umount")
            .arg("-l")
            .arg(&self.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[test]
fn room_disk_refuses_a_substitute_with_the_right_flags() -> Result<(), Failed> {
    if !as_root("workspace mount provenance") {
        return Ok(());
    }
    let place = Place::new("workspace-provenance")?;
    let keeper = Keeper::start(&place, "keeper")?;
    let run = |op| room_disk::run(&place.disk, &keeper.spawner, op);
    run(RoomDiskOp::Create)?;
    run(RoomDiskOp::Open)?;
    run(RoomDiskOp::Open)?;
    let mount = place.disk.mount_point();
    let dev = std::fs::metadata(&mount)?.dev();
    let device = format!("{}:{}", rustix::fs::major(dev), rustix::fs::minor(dev));
    let pinned = hatchling_sys::locked_mount::pin_locked_ext4_root(&mount, &device)?;
    assert!(rustix::fs::fcntl_getfl(&pinned)?.contains(rustix::fs::OFlags::PATH));
    assert!(rustix::io::fcntl_getfd(&pinned)?.contains(rustix::io::FdFlags::CLOEXEC));
    drop(pinned);
    let alias = place.dir.join("workspace-alias");
    std::os::unix::fs::symlink(&mount, &alias)?;
    assert!(hatchling_sys::locked_mount::pin_locked_ext4_root(&alias, &device).is_err());
    let nested = mount.join("nested");
    std::fs::create_dir(&nested)?;
    std::fs::write(mount.join("kept"), b"real encrypted workspace")?;

    for same_disk in [false, true] {
        let mut command = Command::new("mount");
        if same_disk {
            command.arg("--bind").arg(&nested);
        } else {
            command.args(["-t", "tmpfs", "-o", "nodev,nosuid,noexec", "tmpfs"]);
        }
        assert!(command.arg(&mount).status()?.success());
        let covering = HeldBind(mount.clone());
        assert!(
            place.mapped(),
            "the expected encrypted mapping must still exist"
        );
        hatchling_sys::verify_locked_down(&mount)?;
        let refused = run(RoomDiskOp::Open).unwrap_err();
        assert!(
            refused.contains("expected encrypted ext4 volume"),
            "{refused}"
        );
        let refused = room_disk::status(&place.disk).unwrap_err();
        assert!(
            refused.contains("expected encrypted ext4 volume"),
            "{refused}"
        );
        drop(covering);
        assert_eq!(
            std::fs::read(mount.join("kept"))?,
            b"real encrypted workspace"
        );
        assert_eq!(room_disk::status(&place.disk)?.state, RoomDiskState::Open);
    }
    run(RoomDiskOp::Close)?;
    Ok(())
}

/// A second bind of the open room disk, as a cell's would be, holds it: the
/// close takes the mount away but can't close the mapping, says so, and the
/// room disk is then mapped, never said to be closed. Once the bind's gone,
/// the close finishes. It's left closed.
fn held_by_a_bind(place: &Place, mount: &Path) -> Result<(), Failed> {
    let held = place.dir.join("held");
    std::fs::create_dir(&held)?;
    let _held_bind = HeldBind(held.clone());
    let bound = Command::new("mount")
        .arg("--bind")
        .arg(mount)
        .arg(&held)
        .status()?;
    assert!(bound.success(), "the test's bind wasn't made");
    assert!(
        room_disk::close(&place.disk).is_err(),
        "a close that left the mapping was said to have closed it"
    );
    assert!(place.mapped(), "the bind didn't hold the mapping");
    assert_eq!(
        room_disk::status(&place.disk)?.state,
        RoomDiskState::Mapped,
        "a room disk still mapped was said to be closed"
    );
    hatchling_sys::unmount_strict(&held)?;
    room_disk::close(&place.disk)?;
    assert!(!place.mapped());
    assert_eq!(room_disk::status(&place.disk)?.state, RoomDiskState::Closed);
    Ok(())
}

/// What a lock, a reset, a restore, the service's stop and a broker's start
/// use, as an operator's command: the same close, for real, of the room disk
/// open at `mount`. Closed, a second close is nothing to do.
fn operators_close(place: &Place, mount: &Path) -> Result<(), Failed> {
    room_disk::close(&place.disk)?;
    assert!(!hatchling_sys::is_mount_point(mount)?);
    assert!(!place.mapped(), "an operator's close left the mapping");
    room_disk::close(&place.disk)?;
    Ok(())
}

/// A restore's test of a vault's key, for real: its own vault's (the
/// keeper's) opens the room disk, another's doesn't, and neither maps it.
fn keys_tested(place: &Place) -> Result<(), Failed> {
    let master =
        |tag: &str| hatchling_vault::FileKey::new(place.dir.join(tag).join("vault.key/key")).load();
    assert_eq!(
        room_disk::opens_with(&place.disk, &master("keeper")?)?,
        Some(true)
    );
    assert_eq!(
        room_disk::opens_with(&place.disk, &master("other")?)?,
        Some(false)
    );
    assert!(!place.mapped(), "a test of a key mapped the room disk");
    Ok(())
}

/// A room disk made, opened, held, closed, opened again, refused another
/// vault's key, and wiped, for real.
#[test]
fn a_room_disk_is_made_opened_closed_and_wiped_for_real() {
    if !as_root("a_room_disk_is_made_opened_closed_and_wiped_for_real") {
        return;
    }
    let place = Place::new("life").unwrap();
    let keeper = Keeper::start(&place, "keeper").unwrap();
    let run = |op| room_disk::run(&place.disk, &keeper.spawner, op);
    let state = || run(RoomDiskOp::Status).unwrap().unwrap();

    assert_eq!(state().state, RoomDiskState::Absent);
    run(RoomDiskOp::Create).unwrap();
    let made = state();
    assert_eq!(made.state, RoomDiskState::Closed);
    assert_eq!(made.image_bytes, Some(SIZE));
    assert!(
        made.on_disk.is_some_and(|bytes| bytes < SIZE / 2),
        "the image isn't sparse"
    );
    assert!(!place.mapped(), "a create left its mapping");

    run(RoomDiskOp::Open).unwrap();
    let mount = place.disk.mount_point();
    let options = mount_options(&mount).unwrap_or_default();
    for flag in ["nodev", "nosuid", "noexec"] {
        assert!(
            options.split(',').any(|option| option == flag),
            "mounted without {flag}: {options}"
        );
    }
    let open = state();
    assert_eq!(open.state, RoomDiskState::Open);
    assert!(open.used.is_some() && open.free.is_some());
    let program = mount.join("program");
    std::fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        Command::new(&program)
            .status()
            .map_err(|error| error.kind())
            .err(),
        Some(std::io::ErrorKind::PermissionDenied),
        "a program on the room disk ran"
    );
    let canary = format!("hatchling-room-canary-{}", std::process::id());
    std::fs::write(mount.join("canary"), &canary).unwrap();

    let held = std::fs::File::open(mount.join("canary")).unwrap();
    let said = run(RoomDiskOp::Close).unwrap_err();
    assert!(said.contains("busy"), "a close in use: {said}");
    assert!(
        hatchling_sys::is_mount_point(&mount).unwrap(),
        "a room disk in use was taken away"
    );
    drop(held);
    run(RoomDiskOp::Close).unwrap();
    assert!(!hatchling_sys::is_mount_point(&mount).unwrap());
    assert!(!place.mapped(), "a close left the mapping");
    let raw = std::fs::read(place.disk.image()).unwrap();
    assert!(
        !raw.windows(canary.len())
            .any(|window| window == canary.as_bytes()),
        "what was written is in the closed image"
    );
    assert_eq!(state().state, RoomDiskState::Closed);

    run(RoomDiskOp::Open).unwrap();
    assert_eq!(
        std::fs::read_to_string(mount.join("canary")).unwrap(),
        canary,
        "what was written didn't come back"
    );
    held_by_a_bind(&place, &mount).unwrap();
    run(RoomDiskOp::Open).unwrap();
    operators_close(&place, &mount).unwrap();

    let other = Keeper::start(&place, "other").unwrap();
    let said = room_disk::run(&place.disk, &other.spawner, RoomDiskOp::Open).unwrap_err();
    assert!(!said.is_empty());
    assert!(!place.mapped(), "another vault's key mapped the room disk");
    assert!(!hatchling_sys::is_mount_point(&mount).unwrap());

    keys_tested(&place).unwrap();

    let image = std::fs::File::open(place.disk.image()).unwrap();
    run(RoomDiskOp::Wipe).unwrap();
    assert!(!place.disk.image().exists(), "the image is still there");
    let mut zeroed = vec![1u8; 1024 * 1024];
    std::os::unix::fs::FileExt::read_exact_at(&image, &mut zeroed, 0).unwrap();
    assert!(
        zeroed.iter().all(|byte| *byte == 0),
        "the image's header wasn't overwritten"
    );
    assert_eq!(state().state, RoomDiskState::Absent);
}

/// A change of the vault's key, for real: the room disk's key slot moves
/// from one vault's key to another's, closed, without touching the data
/// area; the old key then fails, and the new one opens it and it mounts.
/// Then it moves back while the disk is open and mounted, which goes on
/// working, and the first vault's key opens it again.
#[test]
fn a_room_disks_key_moves_with_the_vaults_for_real() {
    if !as_root("a_room_disks_key_moves_with_the_vaults_for_real") {
        return;
    }
    let place = Place::new("rotate").unwrap();
    let keeper = Keeper::start(&place, "keeper").unwrap();
    let other = Keeper::start(&place, "other").unwrap();
    moved_for_real(&place, &keeper, &other).unwrap();
}

/// The image past its 16 MiB header: the data area, which a key-slot change
/// mustn't touch.
fn data_area(place: &Place) -> Result<Vec<u8>, Failed> {
    let image = std::fs::read(place.disk.image())?;
    Ok(image.get(16 * 1024 * 1024..).unwrap_or_default().to_vec())
}

/// [`a_room_disks_key_moves_with_the_vaults_for_real`], from one vault's
/// keeper, `keeper`, to another's, `other`, and back.
fn moved_for_real(place: &Place, keeper: &Keeper, other: &Keeper) -> Result<(), Failed> {
    let master =
        |tag: &str| hatchling_vault::FileKey::new(place.dir.join(tag).join("vault.key/key")).load();
    let (one, two) = (master("keeper")?, master("other")?);
    room_disk::run(&place.disk, &keeper.spawner, RoomDiskOp::Create)?;
    let before = data_area(place)?;
    assert_eq!(room_disk::rotate(&place.disk, &one, &two)?, Rotated::Done);
    assert!(
        data_area(place)? == before,
        "a key-slot change touched the data area"
    );
    assert_eq!(room_disk::opens_with(&place.disk, &one)?, Some(false));
    assert_eq!(room_disk::opens_with(&place.disk, &two)?, Some(true));
    assert!(room_disk::run(&place.disk, &keeper.spawner, RoomDiskOp::Open).is_err());
    assert!(!place.mapped(), "the old vault's key mapped the room disk");
    room_disk::run(&place.disk, &other.spawner, RoomDiskOp::Open)?;
    let mount = place.disk.mount_point();
    std::fs::write(mount.join("canary"), b"moved")?;
    // Back again, with the disk open and mounted: it goes on working.
    assert_eq!(room_disk::rotate(&place.disk, &two, &one)?, Rotated::Done);
    assert_eq!(std::fs::read(mount.join("canary"))?, b"moved");
    std::fs::write(mount.join("after"), b"still writable")?;
    room_disk::run(&place.disk, &other.spawner, RoomDiskOp::Close)?;
    assert!(room_disk::run(&place.disk, &other.spawner, RoomDiskOp::Open).is_err());
    room_disk::run(&place.disk, &keeper.spawner, RoomDiskOp::Open)?;
    assert_eq!(std::fs::read(mount.join("after"))?, b"still writable");
    room_disk::run(&place.disk, &keeper.spawner, RoomDiskOp::Close)?;
    room_disk::run(&place.disk, &keeper.spawner, RoomDiskOp::Wipe)?;
    Ok(())
}

/// An image that's a link, to a file of the test's own, is refused every op
/// that would touch it, and the file is left exactly as it was.
#[test]
fn an_image_that_is_a_link_is_refused_and_what_it_names_untouched() {
    if !as_root("an_image_that_is_a_link_is_refused_and_what_it_names_untouched") {
        return;
    }
    let place = Place::new("link").unwrap();
    let keeper = Keeper::start(&place, "keeper").unwrap();
    let target = place.dir.join("target.bin");
    let bytes: Vec<u8> = (0..4 * 1024 * 1024_u32)
        .map(|at| u8::try_from(at % 251).unwrap())
        .collect();
    std::fs::write(&target, &bytes).unwrap();
    std::fs::create_dir(place.disk.dir()).unwrap();
    std::fs::set_permissions(place.disk.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&target, place.disk.image()).unwrap();
    for op in [RoomDiskOp::Create, RoomDiskOp::Open, RoomDiskOp::Wipe] {
        let said = room_disk::run(&place.disk, &keeper.spawner, op).unwrap_err();
        assert!(
            said.contains("isn't a regular file itself"),
            "refused for something else: {said}"
        );
    }
    assert_eq!(
        std::fs::read(&target).unwrap(),
        bytes,
        "the file the link names was changed"
    );
    assert!(place.disk.image().is_symlink());
}
