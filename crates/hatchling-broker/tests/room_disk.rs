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

use std::io::Write as _;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use hatchling_broker::room_disk::{self, RoomDisk, RoomDiskOp, RoomDiskState, Rotated};
use hatchling_broker::spawner::{RoomForegroundStatus, Spawner};
use hatchling_core::framing::{read_frame, write_frame};
use sha2::{Digest as _, Sha256};

/// A room disk's size here: the smallest there is.
const SIZE: u64 = 64 * 1024 * 1024;

/// Run the disk fixtures one at a time as root. The private namespace test
/// must not clone a sibling's mount and keep its device busy. Without root,
/// skip unless the run asked for real disk tests.
fn as_root(test: &str) -> Option<MutexGuard<'static, ()>> {
    static DISK_TESTS: Mutex<()> = Mutex::new(());
    if rustix::process::geteuid().is_root() {
        return Some(
            DISK_TESTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
    assert!(
        std::env::var("HATCHLING_REQUIRE_ROOM_DISK").as_deref() != Ok("1"),
        "{test} must run, and it needs root"
    );
    let _ = std::io::Write::write_all(
        &mut std::io::stderr(),
        format!("skipping: {test} makes a room disk, which needs root\n").as_bytes(),
    );
    None
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

    /// [`Place::new`], on a mount of its own that isn't shared: what a
    /// machine's root is in WSL and in containers, wherever this runs.
    fn private(tag: &str) -> Result<Self, Failed> {
        let place = Self::new(tag)?;
        rustix::mount::mount_bind(&place.dir, &place.dir)?;
        rustix::mount::mount_change(&place.dir, rustix::mount::MountPropagationFlags::PRIVATE)?;
        Ok(place)
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
        // The room directory's own bind, and the place's, where there's
        // one: nothing is mounted on either otherwise, and this says so.
        for dir in [mount.parent(), Some(self.dir.as_path())]
            .into_iter()
            .flatten()
        {
            if hatchling_sys::is_mount_point(dir).unwrap_or(false) {
                let _ = Command::new("umount").arg("-l").arg(dir).status();
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A process in a mount namespace of its own, made as a hardened service's
/// is: a copy of every mount, each a slave of the one it was copied from.
struct Service(Child);

impl Service {
    fn start() -> Result<Self, Failed> {
        let child = Command::new("unshare")
            .args(["--mount", "--propagation", "slave", "sleep", "60"])
            .spawn()?;
        let service = Self(child);
        let own = std::fs::read_link("/proc/self/ns/mnt")?;
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
        Err("the service never had a namespace of its own".into())
    }

    /// Whether something is mounted at `path` in the service's namespace.
    fn sees_mounted(&self, path: &Path) -> Result<bool, Failed> {
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
    let Some(_disk_tests) = as_root("workspace mount provenance") else {
        return Ok(());
    };
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

#[test]
fn room_disk_rolls_back_a_fresh_mount_of_the_wrong_device() -> Result<(), Failed> {
    let Some(_disk_tests) = as_root("fresh workspace mount rollback") else {
        return Ok(());
    };
    let namespace = std::fs::read_link("/proc/self/ns/mnt")?;
    let parent = "HATCHLING_TEST_FRESH_MOUNT_PARENT";
    let Some(parent_namespace) = std::env::var_os(parent) else {
        // Device-name substitution must never propagate out of the test.
        let child = Command::new("unshare")
            .args(["--mount", "--propagation", "private", "--"])
            .arg(std::env::current_exe()?)
            .args([
                "--exact",
                "room_disk_rolls_back_a_fresh_mount_of_the_wrong_device",
                "--nocapture",
            ])
            .env(parent, namespace)
            .status()?;
        assert!(child.success(), "the private fresh-mount test failed");
        return Ok(());
    };
    assert_ne!(namespace, PathBuf::from(parent_namespace));
    assert!(
        !std::fs::read_to_string("/proc/self/mountinfo")?
            .split_ascii_whitespace()
            .any(|field| field.starts_with("shared:")),
        "the mount namespace can still propagate changes"
    );

    // Drop the target first on failure: the substitute can still be mounted
    // there if a hand break prevents rollback.
    let other = Place::new("fresh-mount-substitute")?;
    let place = Place::new("fresh-mount")?;
    let keeper = Keeper::start(&place, "keeper")?;
    let run = |op| room_disk::run(&place.disk, &keeper.spawner, op);
    let mount = place.disk.mount_point();
    run(RoomDiskOp::Create)?;
    run(RoomDiskOp::Open)?;
    std::fs::write(mount.join("kept"), b"expected room")?;
    hatchling_sys::unmount_strict(&mount)?;
    assert!(!hatchling_sys::is_mount_point(&mount)?);
    assert_eq!(room_disk::status(&place.disk)?.state, RoomDiskState::Mapped);

    room_disk::run(&other.disk, &keeper.spawner, RoomDiskOp::Create)?;
    room_disk::run(&other.disk, &keeper.spawner, RoomDiskOp::Open)?;
    let other_canary = other.disk.mount_point().join("kept");
    std::fs::write(&other_canary, b"different room")?;
    let node = |place: &Place| -> Result<PathBuf, Failed> {
        let name = place.mapping().ok_or("the fixture has no mapping name")?;
        Ok(std::fs::canonicalize(Path::new("/dev/mapper").join(name))?)
    };
    let (expected, substitute) = (node(&place)?, node(&other)?);
    assert_ne!(expected, substitute);
    assert!(
        Command::new("mount")
            .arg("--bind")
            .arg(&substitute)
            .arg(&expected)
            .status()?
            .success()
    );
    let redirected = HeldBind(expected.clone());
    assert_eq!(
        std::fs::metadata(&expected)?.rdev(),
        std::fs::metadata(&substitute)?.rdev()
    );

    // The real mount takes the substitute device, but the authenticated
    // sysfs mapping is unchanged. Only the post-mount check can refuse it.
    let opened = run(RoomDiskOp::Open);
    drop(redirected);
    let refused = opened.expect_err("fresh open accepted the wrong ext4 device");
    assert!(
        refused.contains("expected encrypted ext4 volume"),
        "fresh open did not reach the provenance check: {refused}"
    );
    assert!(
        !hatchling_sys::is_mount_point(&mount)?,
        "fresh refusal left the substitute mounted"
    );
    assert_eq!(
        room_disk::status(&place.disk)?.state,
        RoomDiskState::Closed,
        "fresh refusal left the expected encrypted disk mapped"
    );
    assert_eq!(std::fs::read(&other_canary)?, b"different room");
    run(RoomDiskOp::Open)?;
    assert_eq!(std::fs::read(mount.join("kept"))?, b"expected room");
    run(RoomDiskOp::Close)?;
    room_disk::run(&other.disk, &keeper.spawner, RoomDiskOp::Close)?;
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

struct CloseDisk(Spawner);
impl Drop for CloseDisk {
    fn drop(&mut self) {
        // Close synchronously before Place removes the fixture files.
        // Disconnecting alone starts the spawner's close asynchronously.
        let _ = self.0.room_disk(RoomDiskOp::Close);
    }
}

fn installed_spawner(
    place: &Place,
    keeper: &Keeper,
    program: &Path,
    cgroup: Option<&Path>,
) -> Result<CloseDisk, Failed> {
    let command = installed_spawner_command(place, program, cgroup);
    let channel = keeper
        .spawner
        .lock()
        .map_err(|_| "test keeper lock poisoned")?;
    Ok(CloseDisk(Spawner::start_with(command, Some(&channel))?))
}

fn installed_spawner_command(place: &Place, program: &Path, cgroup: Option<&Path>) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin")
        .env("TMPDIR", place.dir.join("keeper/tmp"))
        .env(
            "HATCHLING_BROKER_SOCKET",
            place.dir.join("keeper/sock/broker.sock"),
        )
        .env("HATCHLING_VAULT_DIR", place.dir.join("keeper/vault"))
        .env("HATCHLING_AGENT_UID", "4242")
        .env("HATCHLING_ROOM_DISK_DIR", place.disk.dir())
        .env("HATCHLING_ROOM_DISK_SIZE", "64M");
    if let Some(cgroup) = cgroup {
        command.env("HATCHLING_CELL_CGROUP", cgroup);
    }
    command
}

#[test]
fn the_spawner_prepares_layout_only_on_the_verified_room_disk() -> Result<(), Failed> {
    let Some(_disk_tests) = as_root("the_spawner_prepares_layout_only_on_the_verified_room_disk")
    else {
        return Ok(());
    };
    let place = Place::new("layout")?;
    let keeper = Keeper::start(&place, "keeper")?;
    let program = Path::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    let spawner = installed_spawner(&place, &keeper, program, None)?;
    let run = |op| spawner.0.room_disk(op);
    run(RoomDiskOp::Create)?;
    run(RoomDiskOp::Open)?;

    let mount = place.disk.mount_point();
    let inbox = mount.join("inbox");
    let workspace = mount.join("workspace");
    let attachment = workspace.join("inbox");
    for directory in [&inbox, &workspace, &attachment] {
        let meta = std::fs::symlink_metadata(directory)?;
        assert!(meta.is_dir(), "room layout lacks {}", directory.display());
        assert_eq!((meta.uid(), meta.mode() & 0o777), (0, 0o700));
    }

    let canary = workspace.join("canary");
    std::fs::write(&canary, b"existing work")?;
    run(RoomDiskOp::Open)?;
    assert_eq!(std::fs::read(&canary)?, b"existing work");

    let outside = place.dir.join("outside");
    std::fs::create_dir(&outside)?;
    let outside_canary = outside.join("canary");
    std::fs::write(&outside_canary, b"outside")?;
    std::fs::remove_dir(&attachment)?;
    std::os::unix::fs::symlink(&outside, &attachment)?;
    let refused = run(RoomDiskOp::Open).unwrap_err();
    assert!(refused.contains("pin room mount target"), "{refused}");
    assert!(hatchling_sys::is_mount_point(&mount)?);
    assert_eq!(std::fs::read_link(&attachment)?, outside);
    assert_eq!(std::fs::read(&outside_canary)?, b"outside");
    std::fs::remove_file(&attachment)?;
    run(RoomDiskOp::Open)?;
    assert!(std::fs::symlink_metadata(&attachment)?.is_dir());

    let root_mode = std::fs::metadata(&mount)?.permissions();
    std::fs::set_permissions(&mount, std::fs::Permissions::from_mode(0o770))?;
    let refused = run(RoomDiskOp::Open);
    std::fs::set_permissions(&mount, root_mode)?;
    let refused = refused.unwrap_err();
    assert!(
        refused.contains("verify room volume root ownership"),
        "{refused}"
    );

    std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o770))?;
    run(RoomDiskOp::Close)?;
    let refused = run(RoomDiskOp::Open).unwrap_err();
    assert!(refused.contains("verify room inbox ownership"), "{refused}");
    assert!(!hatchling_sys::is_mount_point(&mount)?);
    assert!(!place.mapped(), "layout refusal left its new mapping open");
    assert_eq!(
        run(RoomDiskOp::Status)?.unwrap().state,
        RoomDiskState::Closed
    );
    assert_eq!(std::fs::read(&outside_canary)?, b"outside");
    Ok(())
}

#[test]
fn the_installed_spawner_hands_the_verified_image_to_its_mount_helper() -> Result<(), Failed> {
    const TEST: &str = "the_installed_spawner_hands_the_verified_image_to_its_mount_helper";
    const PARENT: &str = "HATCHLING_TEST_ROOM_ADMISSION_PARENT";
    if !cfg!(target_arch = "x86_64") {
        writeln!(
            std::io::stderr().lock(),
            "skipped: installed Room supervisor requires x86_64 Linux"
        )?;
        return Ok(());
    }
    let Some(_disk_tests) = as_root(TEST) else {
        return Ok(());
    };
    let namespace = std::fs::read_link("/proc/self/ns/mnt")?;
    let Some(parent) = std::env::var_os(PARENT) else {
        assert!(
            Command::new("unshare")
                .args(["--mount", "--propagation", "private", "--"])
                .arg(std::env::current_exe()?)
                .args(["--exact", TEST, "--nocapture"])
                .env(PARENT, namespace)
                .status()?
                .success()
        );
        return Ok(());
    };
    assert_ne!(namespace, PathBuf::from(parent));
    assert!(
        !std::fs::read_to_string("/proc/self/mountinfo")?
            .split_ascii_whitespace()
            .any(|field| field.starts_with("shared:"))
    );

    // This required root fixture also covers mapper reconciliation, the
    // spawner connection, and dispatch of RoomMount in the mutation audit.
    let supervisor = std::env::var_os("HATCHLING_TEST_ROOM_SUPERVISOR")
        .ok_or("root Room proof requires the static image supervisor")?;
    let fixture = Place::new("installed-image")?;
    let source = fixture.dir.join("foreground.c");
    let probe = fixture.dir.join("foreground");
    std::fs::write(&source, include_str!("fixtures/room-foreground-probe.c"))?;
    assert!(
        Command::new("cc")
            .args(["-static", "-Wall", "-Wextra", "-Werror"])
            .arg(&source)
            .arg("-o")
            .arg(&probe)
            .status()?
            .success()
    );
    let (root, digest, binds) =
        room_admission_fixture(&fixture, Path::new(&supervisor), Some(&probe))?;
    let broker = Path::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    for (tag, program) in [
        ("installed-broker-admission", broker.to_owned()),
        (
            "installed-runtime-admission",
            broker.with_file_name("hatchling"),
        ),
    ] {
        assert!(
            program.is_file(),
            "build {} before the root pass",
            program.display()
        );
        check_installed_room_mount(tag, &program, &root, &digest)?;
    }
    drop(binds);
    for mode in 0..=3 {
        check_supervisor_boundary(broker, mode)?;
    }
    Ok(())
}

#[test]
fn an_installed_room_ends_when_its_spawner_is_killed() -> Result<(), Failed> {
    const TEST: &str = "an_installed_room_ends_when_its_spawner_is_killed";
    const PARENT: &str = "HATCHLING_TEST_ROOM_LOSS_PARENT";
    if !cfg!(target_arch = "x86_64") {
        writeln!(
            std::io::stderr().lock(),
            "skipped: installed Room supervisor requires x86_64 Linux"
        )?;
        return Ok(());
    }
    let Some(_disk_tests) = as_root(TEST) else {
        return Ok(());
    };
    let namespace = std::fs::read_link("/proc/self/ns/mnt")?;
    let Some(parent) = std::env::var_os(PARENT) else {
        assert!(
            Command::new("unshare")
                .args(["--mount", "--propagation", "private", "--"])
                .arg(std::env::current_exe()?)
                .args(["--exact", TEST, "--nocapture"])
                .env(PARENT, namespace)
                .status()?
                .success()
        );
        return Ok(());
    };
    assert_ne!(namespace, PathBuf::from(parent));
    assert!(
        !std::fs::read_to_string("/proc/self/mountinfo")?
            .split_ascii_whitespace()
            .any(|field| field.starts_with("shared:"))
    );
    let image = Place::new("loss-image")?;
    let source = image.dir.join("foreground.c");
    let probe = image.dir.join("foreground");
    std::fs::write(&source, include_str!("fixtures/room-foreground-probe.c"))?;
    assert!(
        Command::new("cc")
            .args(["-static", "-Wall", "-Wextra", "-Werror"])
            .arg(&source)
            .arg("-o")
            .arg(&probe)
            .status()?
            .success()
    );
    let supervisor = std::env::var_os("HATCHLING_TEST_ROOM_SUPERVISOR")
        .ok_or("root Room proof requires the static image supervisor")?;
    let (root, digest, _binds) =
        room_admission_fixture(&image, Path::new(&supervisor), Some(&probe))?;
    std::fs::write(
        "/etc/hatchling/room-image.admission",
        format!("{root}\n{digest}\n"),
    )?;
    let broker = Path::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    for program in [broker.to_owned(), broker.with_file_name("hatchling")] {
        // Told why in a log that takes it; then ended all the same with
        // a log that takes nothing more.
        check_room_spawner_loss(&program, &image, false)?;
        check_room_spawner_loss(&program, &image, true)?;
    }
    Ok(())
}

/// Fills a pipe until it takes not one byte more. The writes go through
/// an open of their own, so no other holder's writes stop blocking.
fn fill_pipe(writer: &std::os::fd::OwnedFd) -> Result<(), Failed> {
    use rustix::fs::{Mode, OFlags};
    let mut own = std::fs::File::from(rustix::fs::open(
        format!("/proc/self/fd/{}", writer.as_raw_fd()),
        OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    for chunk in [&[0_u8; 4096][..], &[0_u8; 1][..]] {
        loop {
            match own.write(chunk) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

/// A Room with silent work under way loses its spawner to a kill, and is
/// gone soon after. Its first process says why in the log the spawner
/// handed down; `stalled`, that log is a full pipe nobody reads, and the
/// Room ends without being able to say so.
fn check_room_spawner_loss(program: &Path, image: &Place, stalled: bool) -> Result<(), Failed> {
    let place = Place::new("spawner-loss")?;
    let keeper = Keeper::start(&place, "keeper")?;
    let delegation = std::env::var_os("HATCHLING_TEST_CGROUP").ok_or("missing root test cgroup")?;
    let mut group = hatchling_sys::cgroup::CellCgroup::create(
        Path::new(&delegation),
        hatchling_sys::cgroup::CgroupLimits::default(),
    )?;
    let channel = keeper
        .spawner
        .lock()
        .map_err(|_| "keeper lock poisoned")?
        .try_clone()?;
    let (control, inherited) = UnixStream::pair()?;
    hatchling_sys::keep_across_exec(inherited.as_fd())?;
    hatchling_sys::keep_across_exec(channel.as_fd())?;
    // The mount helper inherits the spawner's stderr: what it says of
    // its end is read back below, or has nowhere to go.
    let said = place.dir.join("spawner.stderr");
    let (log, pipe) = if stalled {
        let (reader, writer) = hatchling_sys::fds::pipe()?;
        (Stdio::from(writer.try_clone()?), Some((reader, writer)))
    } else {
        (Stdio::from(std::fs::File::create(&said)?), None)
    };
    let before = mapper_names()?;
    let mut child = installed_spawner_command(&place, program, Some(group.path()))
        .arg(hatchling_broker::spawner::SPAWNER_SUBCOMMAND)
        .env("HATCHLING_SPAWNER_FD", inherited.as_raw_fd().to_string())
        .env(
            "HATCHLING_SPAWNER_KEEPER_FD",
            channel.as_raw_fd().to_string(),
        )
        .stderr(log)
        .spawn()?;
    drop((channel, inherited));
    let mut mapped = None;
    let result = (|| -> Result<bool, Failed> {
        let spawner = Spawner::over(control.try_clone()?);
        spawner.room_disk(RoomDiskOp::Create)?;
        spawner.room_disk(RoomDiskOp::Open)?;
        let (request, endpoint) = UnixStream::pair()?;
        hatchling_sys::net::send_fd(control.as_fd(), endpoint.as_fd())?;
        drop(endpoint);
        write_frame(
            &request,
            br#"{"request":"room-run","argv":["/bin/foreground","wait"]}"#,
        )?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let marker = place.disk.mount_point().join("workspace/timeout-marker");
        while !marker.is_file() {
            if std::time::Instant::now() >= deadline {
                return Err("the silent Room workload never started".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let during = mapper_names()?;
        let added = during.difference(&before).cloned().collect::<Vec<_>>();
        let [name] = added.as_slice() else {
            return Err("the silent Room must own exactly one new image mapper".into());
        };
        mapped = Some(name.clone());
        if !group_says(group.path(), "populated 1")? {
            return Err("the silent Room isn't in its cgroup".into());
        }
        if let Some((_, writer)) = &pipe {
            fill_pipe(writer)?;
        }
        // Keep both broker connections open. Kill only the owned spawner;
        // neither request EOF nor service teardown can mask this witness.
        child.kill()?;
        if child.wait()?.success() {
            return Err("the killed spawner reported success".into());
        }
        // Stalled, the Room's first process is held at its log until its
        // grace is up.
        let allowed = Duration::from_secs(if stalled { 6 } else { 3 });
        let deadline = std::time::Instant::now() + allowed;
        while !group_says(group.path(), "populated 0")? {
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(true)
    })();
    let _ = child.kill();
    let _ = child.wait();
    drop(pipe);
    let left = clear_spawner_loss(&mut group, mapped.as_deref(), image, &place.disk, &before);
    let ended = result?;
    assert!(left.is_empty(), "cleanup: {left:?}");
    assert!(ended, "a silent Room workload outlived its killed spawner");
    if stalled {
        return Ok(());
    }
    // The helper names the real cause, not a supervisor's exit status.
    let said = std::fs::read_to_string(said)?;
    assert!(
        said.contains("hatchling room mount helper: the Room was stopped: its spawner ended"),
        "{said}"
    );
    assert!(!said.contains("supervisor exited"), "{said}");
    Ok(())
}

/// Whether a cgroup's events file has `line`.
fn group_says(group: &Path, line: &str) -> Result<bool, Failed> {
    Ok(std::fs::read_to_string(group.join("cgroup.events"))?
        .lines()
        .any(|said| said == line))
}

/// Clears what a killed spawner left: the fixture owns cleanup after
/// abrupt loss, and does not credit the dead spawner with removing its
/// cgroup or read-only image mapper. Every step runs whatever came
/// before it, so one failure doesn't leave the rest behind for the next
/// test to trip on. Returns what couldn't be cleared.
fn clear_spawner_loss(
    group: &mut hatchling_sys::cgroup::CellCgroup,
    mapped: Option<&str>,
    image: &Place,
    disk: &RoomDisk,
    before: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let mut left = Vec::new();
    if let Err(error) = group.finish() {
        left.push(format!("cgroup: {error}"));
    }
    if let Some(name) = mapped {
        // Only a mapper shown to be this fixture's is closed.
        match validate_loss_mapper(name, image).and_then(|()| {
            Ok(Command::new("/usr/sbin/veritysetup")
                .args(["close", name])
                .status()?)
        }) {
            Ok(status) if status.success() => {}
            Ok(status) => left.push(format!("closing {name}: {status}")),
            Err(error) => left.push(format!("mapper {name}: {error}")),
        }
    }
    if let Err(error) = room_disk::close(disk) {
        left.push(format!("room disk: {error}"));
    }
    match mapper_names() {
        Ok(after) if after == *before => {}
        Ok(after) => left.push(format!("mappers left: {after:?}, were {before:?}")),
        Err(error) => left.push(format!("mappers: {error}")),
    }
    left
}

fn validate_loss_mapper(name: &str, image: &Place) -> Result<(), Failed> {
    let nonce = name
        .strip_prefix("hatchling-room-verity-")
        .ok_or("unexpected image mapper")?;
    if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("unexpected image mapper name".into());
    }
    let images = image.dir.join("share/hatchling/room");
    let expected = ["room-system-x86_64.squashfs", "room-system-x86_64.verity"]
        .map(|name| std::fs::metadata(images.join(name)))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    for entry in std::fs::read_dir("/sys/block")? {
        let path = entry?.path();
        if std::fs::read_to_string(path.join("dm/name"))
            .ok()
            .as_deref()
            .map(str::trim)
            != Some(name)
        {
            continue;
        }
        let slaves = std::fs::read_dir(path.join("slaves"))?.collect::<Result<Vec<_>, _>>()?;
        if slaves.len() != 2 {
            return Err("the image mapper isn't over two devices".into());
        }
        for slave in slaves {
            let backing = std::fs::read_to_string(slave.path().join("loop/backing_file"))?;
            let actual = std::fs::metadata(backing.trim())?;
            if !expected
                .iter()
                .any(|e| e.dev() == actual.dev() && e.ino() == actual.ino())
            {
                return Err("the image mapper is over a file that isn't the fixture's".into());
            }
        }
        return Ok(());
    }
    Err("the fixture's image mapper disappeared before cleanup".into())
}

fn check_supervisor_boundary(program: &Path, mode: u8) -> Result<(), Failed> {
    let fixture = Place::new("supervisor-boundary")?;
    let probe = fixture.dir.join("probe");
    let source = fixture.dir.join("probe.c");
    std::fs::write(&source, include_str!("fixtures/room-admission-probe.c"))?;
    let (uid, gid) = hatchling_sys::subid::local_account("hatchling-room")?
        .ok_or("the root fixture requires the hatchling-room account")?;
    let compiled = Command::new("cc")
        .args(["-static", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-DROOM_UID={uid}"))
        .arg(format!("-DROOM_GID={gid}"))
        .arg(format!("-DROOM_PROBE_MODE={mode}"))
        .arg(&source)
        .arg("-o")
        .arg(&probe)
        .status()?;
    assert!(
        compiled.success(),
        "could not build the Room boundary probe"
    );
    let (root, digest, _binds) = room_admission_fixture(&fixture, &probe, None)?;
    let system = Path::new("/usr/bin/unshare");
    let real = fixture.dir.join("unshare-real");
    let shim = fixture.dir.join("unshare-probe");
    std::fs::copy(system, &real)?;
    std::fs::write(
        &shim,
        format!(
            "#!/bin/bash\nset -eu\nexec 57</dev/null\nexec '{}' \"$@\"\n",
            real.display()
        ),
    )?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700))?;
    assert!(
        Command::new("mount")
            .arg("--bind")
            .arg(&shim)
            .arg(system)
            .status()?
            .success()
    );
    let _shim = HeldBind(system.to_owned());
    let place = Place::new("installed-supervisor-boundary")?;
    let keeper = Keeper::start(&place, "keeper")?;
    let delegation = std::env::var_os("HATCHLING_TEST_CGROUP").ok_or("missing root test cgroup")?;
    let spawner = installed_spawner(&place, &keeper, program, Some(Path::new(&delegation)))?;
    spawner.0.room_disk(RoomDiskOp::Create)?;
    spawner.0.room_disk(RoomDiskOp::Open)?;
    std::fs::write(
        "/etc/hatchling/room-image.admission",
        format!("{root}\n{digest}\n"),
    )?;
    let before = mapper_names()?;
    let group = |path| {
        std::fs::read_dir(path)?
            .map(|item| item.map(|entry| entry.file_name()))
            .collect::<std::io::Result<std::collections::BTreeSet<_>>>()
    };
    let groups_before = group(Path::new(&delegation))?;
    if mode == 0 {
        check_supervisor_identity_refusals(&spawner.0, gid, &before)?;
        check_workspace_refusals(
            &spawner.0,
            &place.disk.mount_point().join("workspace"),
            &before,
        )?;
    }
    if mode == 0 {
        spawner.0.check_room_mount()?;
        check_workspace_persists(&spawner.0, &place, gid)?;
    } else if mode == 3 {
        let reason = spawner
            .0
            .run_room_foreground(&[String::from("/bin/unused")])
            .err()
            .ok_or("the Room accepted a stale foreground reply")?;
        assert!(reason.contains("different dispatch"), "{reason}");
    } else {
        let started = std::time::Instant::now();
        let reason = spawner
            .0
            .check_room_mount()
            .err()
            .ok_or("Room accepted a supervisor that did not exit")?;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(25) && elapsed < Duration::from_secs(50),
            "mode {mode} took {elapsed:?}: {reason}"
        );
        assert!(
            reason.contains("deadline") || reason.contains("timed out"),
            "mode {mode}: {reason}"
        );
    }
    assert_eq!(mapper_names()?, before);
    assert_eq!(group(Path::new(&delegation))?, groups_before);
    spawner.0.room_disk(RoomDiskOp::Close)?;
    assert!(!place.mapped());
    Ok(())
}

fn check_workspace_refusals(
    spawner: &Spawner,
    workspace: &Path,
    before: &std::collections::BTreeSet<String>,
) -> Result<(), Failed> {
    let anchor = workspace.join("inbox");
    // A masked named entry must not acquire write access when admission
    // widens the mode. Encode a Linux POSIX ACL with its mask set to zero.
    let mut acl = 2_u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1_u16, 7_u16, u32::MAX),
        (2, 7, 4242),
        (4, 0, u32::MAX),
        (16, 0, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(permissions.to_le_bytes());
        acl.extend(id.to_le_bytes());
    }
    rustix::fs::setxattr(
        workspace,
        "system.posix_acl_access",
        &acl,
        rustix::fs::XattrFlags::empty(),
    )?;
    let refusal = spawner
        .check_room_mount()
        .err()
        .ok_or("accepted a workspace ACL")?;
    assert!(refusal.contains("room mount helper failed"), "{refusal}");
    assert_eq!(std::fs::metadata(workspace)?.mode() & 0o7777, 0o700);
    assert_eq!(&mapper_names()?, before);
    rustix::fs::removexattr(workspace, "system.posix_acl_access")?;

    std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o770))?;
    let refusal = spawner
        .check_room_mount()
        .err()
        .ok_or("accepted a writable inbox attachment")?;
    assert!(refusal.contains("room mount helper failed"), "{refusal}");
    assert_eq!(std::fs::metadata(workspace)?.mode() & 0o7777, 0o700);
    assert_eq!(&mapper_names()?, before);
    std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(workspace.join("root-owned-marker"), b"protected")?;
    Ok(())
}

fn check_workspace_persists(spawner: &Spawner, place: &Place, gid: u32) -> Result<(), Failed> {
    let workspace = place.disk.mount_point().join("workspace");
    let marker = workspace.join("admission-marker");
    let permissions = std::fs::metadata(&workspace)?;
    assert_eq!(
        (
            permissions.uid(),
            permissions.gid(),
            permissions.mode() & 0o7777
        ),
        (0, gid, 0o1770)
    );
    assert_eq!(std::fs::metadata(workspace.join("inbox"))?.uid(), 0);
    let workspace_ctime = (permissions.ctime(), permissions.ctime_nsec());
    assert_eq!(std::fs::read(&marker)?, b"first");
    spawner.room_disk(RoomDiskOp::Close)?;
    assert!(!place.mapped());
    spawner.room_disk(RoomDiskOp::Open)?;
    spawner.check_room_mount()?;
    assert_eq!(std::fs::read(&marker)?, b"firstsecond");
    assert_eq!(
        std::fs::read(workspace.join("root-owned-marker"))?,
        b"protected"
    );
    let reopened = std::fs::metadata(&workspace)?;
    assert_eq!((reopened.ctime(), reopened.ctime_nsec()), workspace_ctime);
    Ok(())
}

fn check_supervisor_identity_refusals(
    spawner: &Spawner,
    gid: u32,
    before: &std::collections::BTreeSet<String>,
) -> Result<(), Failed> {
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    let without_room = passwd
        .lines()
        .filter(|line| !line.starts_with("hatchling-room:"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write("/etc/passwd", format!("{without_room}\n"))?;
    let missing = spawner
        .check_room_mount()
        .err()
        .ok_or("Room admitted without its account")?;
    assert!(missing.contains("account is missing"), "{missing}");
    assert_eq!(&mapper_names()?, before);
    std::fs::write(
        "/etc/passwd",
        format!("{without_room}\nhatchling-room:x:65534:{gid}::/nonexistent:/usr/sbin/nologin\n"),
    )?;
    let shared = spawner
        .check_room_mount()
        .err()
        .ok_or("Room admitted a shared account")?;
    assert!(shared.contains("dedicated uid and gid"), "{shared}");
    assert_eq!(&mapper_names()?, before);
    std::fs::write(
        "/etc/passwd",
        format!("{without_room}\nhatchling-room:x:4242:{gid}::/nonexistent:/usr/sbin/nologin\n"),
    )?;
    let agent = spawner
        .check_room_mount()
        .err()
        .ok_or("Room admitted the configured agent UID")?;
    assert!(agent.contains("dedicated uid and gid"), "{agent}");
    assert_eq!(&mapper_names()?, before);
    std::fs::write("/etc/passwd", passwd)?;
    Ok(())
}

fn check_installed_room_mount(
    tag: &str,
    program: &Path,
    root: &str,
    digest: &str,
) -> Result<(), Failed> {
    let place = Place::new(tag)?;
    let keeper = Keeper::start(&place, "keeper")?;
    let delegation = std::env::var_os("HATCHLING_TEST_CGROUP").ok_or("missing root test cgroup")?;
    let parent = hatchling_sys::cgroup::CellCgroup::create(
        Path::new(&delegation),
        hatchling_sys::cgroup::CgroupLimits::default(),
    )?;
    let spawner = installed_spawner(&place, &keeper, program, Some(parent.path()))?;
    let refusal = || {
        spawner
            .0
            .check_room_mount()
            .err()
            .ok_or("the spawner accepted an invalid room mount")
    };
    spawner.0.room_disk(RoomDiskOp::Create)?;
    let closed = refusal()?;
    assert!(closed.contains("isn't mapped"), "{closed}");
    // A command hears the same reason: no Room is launched for it, and it
    // isn't left to try again.
    let unopened = spawner
        .0
        .run_room_foreground(&["/bin/foreground", "kept"].map(str::to_owned))
        .err()
        .ok_or("a command ran with the room disk closed")?;
    assert!(unopened.contains("isn't mapped"), "{unopened}");
    spawner.0.room_disk(RoomDiskOp::Open)?;
    let mount = place.disk.mount_point();
    let canary = mount.join("workspace/kept");
    std::fs::write(&canary, b"kept in the encrypted room")?;
    let before = mapper_names()?;
    let record = Path::new("/etc/hatchling/room-image.admission");
    check_admission_record(&refusal, record, root, digest)?;
    assert_eq!(mapper_names()?, before);

    {
        let other = Place::new(&format!("{tag}-undelegated"))?;
        let keeper = Keeper::start(&other, "keeper")?;
        let undelegated = installed_spawner(&other, &keeper, program, None)?;
        undelegated.0.room_disk(RoomDiskOp::Create)?;
        undelegated.0.room_disk(RoomDiskOp::Open)?;
        let reason = undelegated
            .0
            .check_room_mount()
            .err()
            .ok_or("room admission ran without cgroup delegation")?;
        assert!(reason.contains("requires a delegated cgroup"), "{reason}");
        assert_eq!(mapper_names()?, before, "refusal activated an image mapper");
    }

    check_substituted_mount(&spawner.0, &mount)?;
    assert_eq!(mapper_names()?, before);
    let inbox = mount.join("inbox");
    std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o770))?;
    let refused = refusal()?;
    assert!(
        refused.contains("trusted room mount helper failed"),
        "{refused}"
    );
    assert_eq!(mapper_names()?, before, "refusal left an image mapper");
    std::fs::set_permissions(inbox, std::fs::Permissions::from_mode(0o700))?;
    check_room_cgroup(&place, &spawner.0, parent.path())?;
    assert_eq!(mapper_names()?, before, "success left an image mapper");
    assert_eq!(std::fs::read(&canary)?, b"kept in the encrypted room");
    check_room_foreground(&spawner.0, &place, &before)?;
    // The Room is kept after its last command, with its image mapped,
    // until its session ends: here, as its disk is closed.
    assert_ne!(
        mapper_names()?,
        before,
        "the Room was not kept after its command"
    );
    spawner.0.room_disk(RoomDiskOp::Close)?;
    assert_eq!(
        mapper_names()?,
        before,
        "closing the disk left the Room's image mapped"
    );
    spawner.0.room_disk(RoomDiskOp::Open)?;
    if tag == "installed-broker-admission" {
        check_room_foreground_limits(&spawner.0, &place, parent.path())?;
        check_room_stays_held_until_cleared(&spawner.0, &place)?;
        check_room_foreground_disconnect(program, parent.path())?;
        check_room_ends_with_its_broker(program, parent.path())?;
    }
    assert_eq!(
        mapper_names()?,
        before,
        "foreground work left an image mapper"
    );
    check_pending_image_cleanup(&spawner.0, &place)?;
    assert!(!place.mapped());
    std::fs::remove_file(record)?;
    Ok(())
}

fn check_room_foreground(
    spawner: &Spawner,
    place: &Place,
    before: &std::collections::BTreeSet<String>,
) -> Result<(), Failed> {
    let first = spawner
        .run_room_foreground(&["/bin/foreground", "seed", "room data"].map(str::to_owned))?;
    assert_eq!(first.status, RoomForegroundStatus::Exited(23));
    assert_eq!(first.stdout, b"/workspace");
    assert_eq!(first.stderr, b"diagnostic");
    assert!(!first.stdout_truncated && !first.stderr_truncated);
    let path = place.disk.mount_point().join("workspace/foreground-file");
    let digest = Sha256::digest(std::fs::read(&path)?);
    assert_eq!(digest, Sha256::digest(b"room data"));

    // The next command runs in the same Room: it finds the scratch the
    // first one left, which no other Room has. A status and an open of
    // the disk under it leave the Room as it is; so does a check.
    let kept = spawner.run_room_foreground(&["/bin/foreground", "kept"].map(str::to_owned))?;
    assert_eq!(
        kept.status,
        RoomForegroundStatus::Exited(0),
        "the second command ran in another Room"
    );
    assert_eq!(
        spawner
            .room_disk(RoomDiskOp::Status)?
            .map(|status| status.state),
        Some(RoomDiskState::Open)
    );
    spawner.room_disk(RoomDiskOp::Open)?;
    spawner.check_room_mount()?;
    let kept = spawner.run_room_foreground(&["/bin/foreground", "kept"].map(str::to_owned))?;
    assert_eq!(
        kept.status,
        RoomForegroundStatus::Exited(0),
        "a status, an open or a check ended the Room"
    );

    // Closing the disk ends the Room first: nothing of it is left, and
    // the next command starts another, with scratch of its own.
    spawner.room_disk(RoomDiskOp::Close)?;
    assert!(!place.mapped());
    assert_eq!(
        &mapper_names()?,
        before,
        "the closed disk's Room left its image mapped"
    );
    spawner.room_disk(RoomDiskOp::Open)?;
    let next = spawner.run_room_foreground(&["/bin/foreground", "resume"].map(str::to_owned))?;
    assert_eq!(next.status, RoomForegroundStatus::Exited(0));
    assert_eq!(Sha256::digest(&next.stdout), digest);

    // A large stderr write precedes stdout and must never stall its capture.
    let capped = spawner.run_room_foreground(&["/bin/foreground", "output"].map(str::to_owned))?;
    assert_eq!(capped.status, RoomForegroundStatus::Exited(0));
    assert_eq!(capped.stdout, vec![0; 4096]);
    assert_eq!(capped.stderr, vec![0; 4096]);
    assert!(!capped.stdout_truncated && capped.stderr_truncated);
    let missing = spawner.run_room_foreground(&[String::from("/bin/absent")])?;
    assert_eq!(missing.status, RoomForegroundStatus::SpawnFailed);
    Ok(())
}

fn check_room_foreground_limits(
    spawner: &Spawner,
    place: &Place,
    parent: &Path,
) -> Result<(), Failed> {
    let full =
        spawner.run_room_foreground(&["/bin/foreground", "fill-scratch"].map(str::to_owned))?;
    assert_eq!(full.status, RoomForegroundStatus::Exited(1));
    assert_eq!(full.stderr, b"ENOSPC");
    let timeout = spawner.run_room_foreground(&["/bin/foreground", "wait"].map(str::to_owned));
    let reason = timeout
        .err()
        .ok_or("the Room deadline allowed the job to finish")?;
    assert!(
        reason.contains("Room foreground deadline expired"),
        "{reason}"
    );
    assert_eq!(
        std::fs::read(place.disk.mount_point().join("workspace/timeout-marker"))?,
        b"started"
    );
    for entry in std::fs::read_dir(parent)? {
        assert!(
            !entry?.file_type()?.is_dir(),
            "the timeout left a Room cgroup"
        );
    }
    Ok(())
}

/// One command at a time: a second is refused while one runs, and the
/// disk under the Room still answers for itself.
fn check_one_command_at_a_time(spawner: &Spawner) -> Result<(), Failed> {
    let busy = spawner
        .run_room_foreground(&["/bin/foreground", "kept"].map(str::to_owned))
        .err()
        .ok_or("a second command ran beside the first")?;
    assert!(busy.contains("busy with another command"), "{busy}");
    assert_eq!(
        spawner
            .room_disk(RoomDiskOp::Status)?
            .map(|status| status.state),
        Some(RoomDiskState::Open)
    );
    Ok(())
}

fn check_room_foreground_disconnect(program: &Path, cgroup: &Path) -> Result<(), Failed> {
    let place = Place::new("foreground-disconnect")?;
    let keeper = Keeper::start(&place, "keeper")?;
    let channel = keeper
        .spawner
        .lock()
        .map_err(|_| "test keeper lock poisoned")?
        .try_clone()?;
    let (control, inherited) = UnixStream::pair()?;
    hatchling_sys::keep_across_exec(inherited.as_fd())?;
    hatchling_sys::keep_across_exec(channel.as_fd())?;
    let mut child = installed_spawner_command(&place, program, Some(cgroup))
        .arg(hatchling_broker::spawner::SPAWNER_SUBCOMMAND)
        .env("HATCHLING_SPAWNER_FD", inherited.as_raw_fd().to_string())
        .env(
            "HATCHLING_SPAWNER_KEEPER_FD",
            channel.as_raw_fd().to_string(),
        )
        .spawn()?;
    drop((channel, inherited));
    let result = (|| -> Result<(), Failed> {
        let before = mapper_names()?;
        let spawner = CloseDisk(Spawner::over(control.try_clone()?));
        spawner.0.room_disk(RoomDiskOp::Create)?;
        spawner.0.room_disk(RoomDiskOp::Open)?;
        let (request, endpoint) = UnixStream::pair()?;
        hatchling_sys::net::send_fd(control.as_fd(), endpoint.as_fd())?;
        drop(endpoint);
        write_frame(
            &request,
            br#"{"request":"room-run","argv":["/bin/foreground","wait"]}"#,
        )?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let marker = place.disk.mount_point().join("workspace/timeout-marker");
        while !marker.is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "the Room workload never started"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(
            mapper_names()?,
            before,
            "the workload did not keep the image mapped"
        );
        check_one_command_at_a_time(&spawner.0)?;
        let (vault, keyring, old) = check_room_maintenance_refusals(&place, program)?;
        let started = std::time::Instant::now();
        drop(request);
        spawner.0.room_disk(RoomDiskOp::Close)?;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the request disconnect waited for the command deadline"
        );
        assert_eq!(mapper_names()?, before);
        assert!(!place.mapped());
        for entry in std::fs::read_dir(cgroup)? {
            assert!(
                !entry?.file_type()?.is_dir(),
                "disconnect left a Room cgroup"
            );
        }
        check_operator_excludes_room(&spawner.0, &place, program, &vault, &keyring)?;
        let mut rekey = room_operator_command(&place, program, &vault, &keyring)?;
        let output = rekey.arg("rekey-vault").output()?;
        assert!(
            output.status.success(),
            "drained Room refused operator rekey"
        );
        // The real CLI handed these files to the keeper account. Use the
        // same root-operator acting API for readback, not relaxed checks.
        let keyring_path = keyring.keyring.as_ref().ok_or("fixture has no keyring")?;
        hatchling_vault::act_for(
            std::fs::metadata(keyring_path)?.uid(),
            &[
                &vault.root,
                vault.key_file.parent().ok_or("fixture key has no parent")?,
                keyring_path
                    .parent()
                    .ok_or("fixture keyring has no parent")?,
            ],
        )?;
        let new = hatchling_broker::vault_keyring::unlock(&vault, &keyring)?;
        assert_eq!(room_disk::opens_with(&place.disk, &old)?, Some(false));
        assert_eq!(room_disk::opens_with(&place.disk, &new)?, Some(true));
        let name = "canary".parse()?;
        let canary = hatchling_vault::SealedVault::open(&vault.root, &new)?.fetch(&name)?;
        assert!(
            canary.as_bytes() == b"operator canary",
            "rekey lost the store's bytes"
        );
        assert!(!place.mapped(), "operator rekey left the Room mapped");
        Ok(())
    })();
    drop(control);
    assert!(child.wait()?.success());
    result
}

/// A session whose Room can't be cleared stays in charge of it: the disk
/// isn't closed under it, nothing runs or is checked beside it, and it
/// keeps the disk's claim, so an operator is refused as during a live
/// Room. Once the Room can be cleared, a close clears it and closes.
fn check_room_stays_held_until_cleared(spawner: &Spawner, place: &Place) -> Result<(), Failed> {
    // The private namespace sees a real veritysetup except that close
    // deliberately fails. The genuine image mapper stays in the kernel.
    let system = Path::new("/usr/sbin/veritysetup");
    let real = place.dir.join("veritysetup-held-real");
    let shim = place.dir.join("veritysetup-held-fail-close");
    std::fs::copy(system, &real)?;
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nif [ \"$1\" = close ]; then exit 1; fi\nexec '{}' \"$@\"\n",
            real.display()
        ),
    )?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700))?;
    let before = mapper_names()?;
    let output = ["/bin/foreground", "output"].map(str::to_owned);
    let ran = spawner.run_room_foreground(&output)?;
    assert_eq!(ran.status, RoomForegroundStatus::Exited(0));
    assert!(
        Command::new("mount")
            .arg("--bind")
            .arg(&shim)
            .arg(system)
            .status()?
            .success()
    );
    let held = HeldBind(system.to_owned());
    let result = (|| -> Result<(), Failed> {
        let close = spawner
            .room_disk(RoomDiskOp::Close)
            .err()
            .ok_or("the disk closed under a Room that wasn't cleared")?;
        assert!(close.contains("isn't cleared yet"), "{close}");
        assert!(close.contains("cannot remove room mapper"), "{close}");
        let beside = spawner
            .run_room_foreground(&output)
            .err()
            .ok_or("a command ran beside a Room that wasn't cleared")?;
        assert!(beside.contains("isn't cleared yet"), "{beside}");
        let check = spawner
            .check_room_mount()
            .err()
            .ok_or("a Room was admitted beside one that wasn't cleared")?;
        assert!(check.contains("isn't cleared yet"), "{check}");
        assert_eq!(
            spawner
                .room_disk(RoomDiskOp::Status)?
                .map(|status| status.state),
            Some(RoomDiskState::Open)
        );
        let reason = room_disk::close(&place.disk)
            .err()
            .ok_or("maintenance closed the disk under a Room that wasn't cleared")?;
        assert!(reason.contains("in use by another process"), "{reason}");
        assert_ne!(mapper_names()?, before, "the fixture left nothing to clear");
        assert!(place.mapped());
        Ok(())
    })();
    drop(held);
    result?;
    spawner.room_disk(RoomDiskOp::Close)?;
    assert_eq!(mapper_names()?, before, "the cleared Room left its mapper");
    assert!(!place.mapped());
    spawner.room_disk(RoomDiskOp::Open)?;
    Ok(())
}

/// A Room kept idle ends when the broker goes: the spawner sees it down
/// and closes its disk before it leaves, with nothing of the Room left.
fn check_room_ends_with_its_broker(program: &Path, cgroup: &Path) -> Result<(), Failed> {
    let place = Place::new("session-broker-loss")?;
    let keeper = Keeper::start(&place, "keeper")?;
    let channel = keeper
        .spawner
        .lock()
        .map_err(|_| "test keeper lock poisoned")?
        .try_clone()?;
    let (control, inherited) = UnixStream::pair()?;
    hatchling_sys::keep_across_exec(inherited.as_fd())?;
    hatchling_sys::keep_across_exec(channel.as_fd())?;
    let before = mapper_names()?;
    let mut child = installed_spawner_command(&place, program, Some(cgroup))
        .arg(hatchling_broker::spawner::SPAWNER_SUBCOMMAND)
        .env("HATCHLING_SPAWNER_FD", inherited.as_raw_fd().to_string())
        .env(
            "HATCHLING_SPAWNER_KEEPER_FD",
            channel.as_raw_fd().to_string(),
        )
        .spawn()?;
    drop((channel, inherited));
    let result = (|| -> Result<(), Failed> {
        let spawner = Spawner::over(control.try_clone()?);
        spawner.room_disk(RoomDiskOp::Create)?;
        spawner.room_disk(RoomDiskOp::Open)?;
        let seeded =
            spawner.run_room_foreground(&["/bin/foreground", "seed", "kept"].map(str::to_owned))?;
        assert_eq!(seeded.status, RoomForegroundStatus::Exited(23));
        assert_ne!(mapper_names()?, before, "the idle Room was not kept");
        assert!(place.mapped());
        Ok(())
    })();
    // The broker goes with the Room idle and nothing asked of it.
    let started = std::time::Instant::now();
    drop(control);
    let status = child.wait()?;
    result?;
    assert!(status.success(), "the spawner failed as its broker went");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the spawner waited out the Room's idle time"
    );
    assert_eq!(mapper_names()?, before, "the Room outlived its broker");
    assert!(!place.mapped(), "the Room's disk stayed open");
    for entry in std::fs::read_dir(cgroup)? {
        assert!(
            !entry?.file_type()?.is_dir(),
            "the broker's going left a Room cgroup"
        );
    }
    Ok(())
}

fn room_operator_command(
    place: &Place,
    program: &Path,
    vault: &hatchling_broker::VaultConfig,
    keyring: &hatchling_broker::vault_keyring::KeyringConfig,
) -> Result<Command, Failed> {
    let mut command = installed_spawner_command(place, program, None);
    command
        .env("HATCHLING_VAULT_DIR", &vault.root)
        .env("HATCHLING_VAULT_KEY_FILE", &vault.key_file)
        .env(
            "HATCHLING_VAULT_KEYRING",
            keyring.keyring.as_ref().ok_or("fixture has no keyring")?,
        )
        .stdin(Stdio::null());
    Ok(command)
}

fn check_operator_excludes_room(
    spawner: &Spawner,
    place: &Place,
    program: &Path,
    vault: &hatchling_broker::VaultConfig,
    keyring: &hatchling_broker::vault_keyring::KeyringConfig,
) -> Result<(), Failed> {
    let (mut prompt, stderr) = UnixStream::pair()?;
    prompt.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut command = room_operator_command(place, program, vault, keyring)?;
    let mut child = command
        .args(["vault-keyring", "enroll-keys"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::os::fd::OwnedFd::from(stderr)))
        .spawn()?;
    drop(command);
    let result = (|| -> Result<(), Failed> {
        // The fixed first prompt proves the constructor finished. Probing
        // the lock instead could win it, or see only a transient claim.
        let expected = b"Enroll two security keys";
        let mut prefix = vec![0; expected.len()];
        std::io::Read::read_exact(&mut prompt, &mut prefix)?;
        if prefix.as_slice() != expected {
            return Err("operator did not reach its input prompt".into());
        }
        let reason = spawner
            .check_room_mount()
            .err()
            .ok_or("Room admitted during maintenance")?;
        if !reason.contains("in use by another process") {
            return Err("the operator did not retain its Room claim while awaiting input".into());
        }
        let reason = spawner
            .room_disk(RoomDiskOp::Open)
            .err()
            .ok_or("Room disk reopened during maintenance")?;
        if !reason.contains("in use by another process") {
            return Err("operator allowed the Room disk to reopen during maintenance".into());
        }
        Ok(())
    })();
    let _ = child.kill();
    let status = child.wait()?;
    result?;
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(9)
    );
    assert_eq!(room_disk::status(&place.disk)?.state, RoomDiskState::Closed);
    Ok(())
}

/// Real operator entrypoints contend with the real installed spawner.
/// A broken guard may offer a seed or recovery codes: assert fixed
/// diagnostics instead of formatting the captured output buffers.
fn check_room_maintenance_refusals(
    place: &Place,
    program: &Path,
) -> Result<
    (
        hatchling_broker::VaultConfig,
        hatchling_broker::vault_keyring::KeyringConfig,
        hatchling_vault::MasterKey,
    ),
    Failed,
> {
    let root = place.dir.join("operator");
    for name in ["", "vault", "vault.key", "keyring"] {
        let dir = root.join(name);
        std::fs::create_dir(&dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // The CLI hands the vault to the real keeper account; it must be able
    // to search this parent, while the vault and key directories stay private.
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o711))?;
    let vault = hatchling_broker::VaultConfig {
        root: root.join("vault"),
        key_file: root.join("vault.key/key"),
        grants: Vec::new(),
    };
    // The operator and real keeper begin with the same master key.
    std::fs::copy(place.dir.join("keeper/vault.key/key"), &vault.key_file)?;
    let old = hatchling_vault::FileKey::new(vault.key_file.clone()).load()?;
    let keyring = hatchling_broker::vault_keyring::KeyringConfig {
        keyring: Some(root.join("keyring/keyring")),
        ..Default::default()
    };
    let name = "canary".parse()?;
    hatchling_vault::SealedVault::open(&vault.root, &old)?.store(
        &name,
        &hatchling_vault::Secret::new(b"operator canary".to_vec())?,
    )?;
    hatchling_broker::vault_keyring::init(&vault, &keyring)?;
    let watched = [
        root.join("keyring/keyring"),
        root.join("vault.key/kek"),
        root.join("vault/.store-key"),
        root.join("vault/canary"),
    ];
    let before = watched
        .iter()
        .map(std::fs::read)
        .collect::<Result<Vec<_>, _>>()?;
    let header = Sha256::digest(hatchling_sys::read_file_head(
        &place.disk.image(),
        usize::try_from(room_disk::HEADER_BYTES)?,
    )?);
    for args in [
        vec!["rekey-vault"],
        vec!["vault-keyring", "enroll-totp"],
        vec!["vault-keyring", "enroll-keys"],
    ] {
        let operation = args.join(" ");
        let output = room_operator_command(place, program, &vault, &keyring)?
            .args(args)
            .output()?;
        assert!(!output.status.success(), "operator mutated an active Room");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("in use by another process"),
            "operator {operation} was refused for a different reason"
        );
    }
    let replacement = root.join("replacement.key");
    let output = room_operator_command(place, program, &vault, &keyring)?
        .env_remove("HATCHLING_VAULT_KEYRING")
        .args([std::ffi::OsStr::new("rekey-vault"), replacement.as_os_str()])
        .output()?;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("in use by another process"),
        "file-key operator was refused for a different reason"
    );
    assert!(
        !replacement.exists() && !vault.key_file.exists(),
        "a refused operator created a key file"
    );
    let fresh = hatchling_vault::MasterKey::generate()?;
    for reason in [
        room_disk::close(&place.disk)
            .err()
            .ok_or("maintenance closed an active Room")?,
        room_disk::wipe(&place.disk)
            .err()
            .ok_or("maintenance wiped an active Room")?,
        room_disk::rotate(&place.disk, &old, &fresh)
            .err()
            .ok_or("maintenance rekeyed an active Room")?,
    ] {
        assert!(reason.contains("in use by another process"), "{reason}");
    }
    for (path, bytes) in watched.iter().zip(before) {
        assert!(
            std::fs::read(path)? == bytes,
            "a refused operator changed vault state"
        );
    }
    assert!(
        Sha256::digest(hatchling_sys::read_file_head(
            &place.disk.image(),
            usize::try_from(room_disk::HEADER_BYTES)?
        )?) == header,
        "a refused operator changed the Room key slots"
    );
    assert!(place.mapped(), "maintenance interrupted the live Room");
    Ok((vault, keyring, old))
}

fn check_room_cgroup(place: &Place, spawner: &Spawner, parent: &Path) -> Result<(), Failed> {
    // Observe the wrapper before it runs the genuine unshare. This fixture
    // alone has a private mount namespace and substitutes the executable.
    let system = Path::new("/usr/bin/unshare");
    let real = place.dir.join("room-unshare-real");
    let shim = place.dir.join("room-unshare-observer");
    let observed = place.dir.join("room-cgroup-observed");
    let block_drain = place.dir.join("block-room-cgroup-drain");
    let parent_name = parent.strip_prefix("/sys/fs/cgroup")?;
    std::fs::copy(system, &real)?;
    std::fs::write(
        &shim,
        format!(
            r#"#!/bin/sh
set -eu
group=$(/bin/sed -n 's/^0:://p' /proc/self/cgroup)
test -n "$group"
{{
    printf '%s\n' "$group"
    for limit in pids.max memory.max memory.swap.max cpu.max; do
        /bin/cat "/sys/fs/cgroup$group/$limit"
    done
}} > '{}'
if [ -e '{}' ]; then
    case "$group" in '/{}'/cell-*) ;; *) exit 1 ;; esac
    # This bind is visible only in the fixture's private mount namespace.
    /usr/bin/mount --bind /dev/full "/sys/fs/cgroup$group/cgroup.kill"
else
    # This benign child stays outside the inner PID namespace. Only the
    # spawner's cgroup drain owns it once the launcher has exited.
    /bin/sleep 8 </dev/null >/dev/null 2>&1 &
    kill -0 "$!"
fi
exec '{}' "$@"
"#,
            observed.display(),
            block_drain.display(),
            parent_name.display(),
            real.display(),
        ),
    )?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700))?;
    assert!(
        Command::new("mount")
            .arg("--bind")
            .arg(&shim)
            .arg(system)
            .status()?
            .success()
    );
    let _observer = HeldBind(system.to_owned());
    spawner.check_room_mount()?;
    let (path, limits) = observed_room_cgroup(&observed, parent)?;
    assert_room_limits(&limits)?;
    assert!(!path.exists(), "room cgroup survived the result");

    let before = mapper_names()?;
    std::fs::remove_file(&observed)?;
    std::fs::write(block_drain, [])?;
    let failed = spawner.check_room_mount();
    let (path, _) = observed_room_cgroup(&observed, parent)?;
    let held = HeldBind(path.join("cgroup.kill"));
    let reason = failed
        .err()
        .ok_or("room admission ignored the failed cgroup drain")?;
    assert!(reason.contains("cgroup.kill"), "{reason}");
    let retained = mapper_names()?;
    assert_eq!(retained.difference(&before).count(), 1);
    for op in [RoomDiskOp::Close, RoomDiskOp::Wipe] {
        let reason = spawner
            .room_disk(op)
            .err()
            .ok_or("disk maintenance ignored the failed cgroup drain")?;
        assert!(reason.contains("cgroup.kill"), "{reason}");
        assert_eq!(mapper_names()?, retained);
        assert!(place.mapped(), "failed drain allowed disk maintenance");
    }
    drop(held);
    spawner.room_disk(RoomDiskOp::Close)?;
    assert!(!path.exists(), "retry left the Room cgroup behind");
    assert_eq!(mapper_names()?, before);
    assert!(!place.mapped());
    spawner.room_disk(RoomDiskOp::Open)?;
    Ok(())
}

fn observed_room_cgroup(observed: &Path, parent: &Path) -> Result<(PathBuf, String), Failed> {
    let report = std::fs::read_to_string(observed)?;
    let (name, limits) = report
        .split_once('\n')
        .ok_or("launcher did not report its cgroup")?;
    let path = Path::new("/sys/fs/cgroup").join(name.trim_start_matches('/'));
    assert_eq!(
        path.parent(),
        Some(parent),
        "launcher ran outside its Room group"
    );
    Ok((path, limits.to_owned()))
}

fn assert_room_limits(limits: &str) -> Result<(), Failed> {
    let mut lines = limits.lines();
    assert_eq!(lines.next(), Some("512"));
    let memory = (hatchling_sys::cgroup::default_memory()? / 2).min(4 << 30);
    let page_size = Command::new("/usr/bin/getconf").arg("PAGESIZE").output()?;
    assert!(page_size.status.success());
    let page_size: u64 = std::str::from_utf8(&page_size.stdout)?.trim().parse()?;
    assert!(page_size > 0);
    let effective_memory = memory - memory % page_size;
    assert_eq!(lines.next(), Some(effective_memory.to_string().as_str()));
    assert_eq!(lines.next(), Some("0"));
    let cores = std::thread::available_parallelism()?.get();
    assert_eq!(
        lines.next(),
        Some(format!("{} 100000", cores * 50_000).as_str())
    );
    assert!(lines.next().is_none());
    Ok(())
}

fn mapper_names() -> Result<std::collections::BTreeSet<String>, Failed> {
    let mut names = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir("/sys/block")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("dm-") {
            let name = std::fs::read_to_string(entry.path().join("dm/name"))?;
            if name.starts_with("hatchling-room-verity-") {
                names.insert(name.trim_end().to_owned());
            }
        }
    }
    Ok(names)
}

fn check_admission_record(
    refusal: &impl Fn() -> Result<String, &'static str>,
    record: &Path,
    root: &str,
    digest: &str,
) -> Result<(), Failed> {
    let missing = refusal()?;
    assert!(
        missing.contains("cannot read the installed room image admission record"),
        "{missing}"
    );
    let short_root = root.get(..63).ok_or("missing verity root")?;
    std::fs::write(record, format!("{short_root}\n{digest}\n"))?;
    let short = refusal()?;
    assert!(short.contains("admission record is invalid"), "{short}");
    for bad in [
        format!("{root} {digest}\n"),
        format!("{short_root}\n0{digest}\n"),
    ] {
        std::fs::write(record, bad)?;
        let malformed = refusal()?;
        assert!(
            malformed.contains("admission record is malformed"),
            "{malformed}"
        );
    }
    let wrong = if root.starts_with('0') { '1' } else { '0' };
    let suffix = root.get(1..).ok_or("missing verity root")?;
    std::fs::write(record, format!("{wrong}{suffix}\n{digest}\n"))?;
    let mismatched = refusal()?;
    assert!(mismatched.contains("verify"), "{mismatched}");
    let valid = format!("{root}\n{digest}\n");
    let other = record.with_extension("fixture-target");
    std::fs::write(&other, &valid)?;
    std::fs::remove_file(record)?;
    std::os::unix::fs::symlink(&other, record)?;
    let linked = refusal()?;
    assert!(linked.contains("admission record is invalid"), "{linked}");
    std::fs::remove_file(record)?;
    std::fs::hard_link(&other, record)?;
    let linked = refusal()?;
    assert!(linked.contains("admission record is invalid"), "{linked}");
    std::fs::remove_file(record)?;
    std::fs::remove_file(other)?;
    std::fs::write(record, valid)?;
    std::fs::set_permissions(record, std::fs::Permissions::from_mode(0o600))?;
    assert!(
        Command::new("/usr/bin/chown")
            .arg("+4242")
            .arg(record)
            .status()?
            .success()
    );
    assert_eq!(std::fs::metadata(record)?.uid(), 4242);
    let unowned = refusal()?;
    assert!(unowned.contains("admission record is invalid"), "{unowned}");
    rustix::fs::chown(record, Some(rustix::process::Uid::ROOT), None)?;
    std::fs::set_permissions(record, std::fs::Permissions::from_mode(0o666))?;
    let writable = refusal()?;
    assert!(
        writable.contains("admission record is invalid"),
        "{writable}"
    );
    std::fs::set_permissions(record, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn check_pending_image_cleanup(spawner: &Spawner, place: &Place) -> Result<(), Failed> {
    // The private namespace sees a real veritysetup except that close
    // deliberately fails. The genuine image mapper stays in the kernel.
    let system = Path::new("/usr/sbin/veritysetup");
    let real = place.dir.join("veritysetup-real");
    let shim = place.dir.join("veritysetup-fail-close");
    std::fs::copy(system, &real)?;
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nif [ \"$1\" = close ]; then exit 1; fi\nexec '{}' \"$@\"\n",
            real.display()
        ),
    )?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700))?;
    let before = mapper_names()?;
    assert!(
        Command::new("mount")
            .arg("--bind")
            .arg(&shim)
            .arg(system)
            .status()?
            .success()
    );
    let held = HeldBind(system.to_owned());
    let admission = spawner.check_room_mount();
    let after = mapper_names()?;
    let close = spawner.room_disk(RoomDiskOp::Close);
    let status = spawner.room_disk(RoomDiskOp::Status);
    drop(held);
    let retry = spawner.room_disk(RoomDiskOp::Close);
    let remaining = mapper_names()?;
    let admission = admission
        .err()
        .ok_or("admission ignored the failed mapper close")?;
    let name = admission
        .strip_prefix("cannot remove room mapper /dev/mapper/")
        .and_then(|detail| detail.split_once(':'))
        .map(|(name, _)| name)
        .ok_or_else(|| format!("admission did not report its pending mapper: {admission}"))?;
    let nonce = name
        .strip_prefix("hatchling-room-verity-")
        .ok_or("unexpected mapper name")?;
    assert!(nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    if !before.contains(name) && after.contains(name) && remaining.contains(name) {
        // Clean up only the new mapper named by this request, even if a
        // deliberate guard break loses its retry handle.
        let _ = Command::new(&real).args(["close", name]).status()?;
    }
    assert!(
        !before.contains(name) && after.contains(name),
        "fixture left no mapper to retry"
    );
    let close = close
        .err()
        .ok_or("disk closed before pending image cleanup succeeded")?;
    assert!(close.contains("cannot remove room mapper"), "{close}");
    let state = status?
        .ok_or("missing room disk status after refused close")?
        .state;
    assert_eq!(state, RoomDiskState::Open);
    retry?;
    assert_eq!(remaining, before, "retry left the image mapper open");
    Ok(())
}

fn check_substituted_mount(spawner: &Spawner, mount: &Path) -> Result<(), Failed> {
    let view = || -> Result<(String, String, String, String), Failed> {
        // Match the opened mount by ID. A path lookup in findmnt can
        // describe the hidden lower mount when mounts share that path.
        let opened = std::fs::File::open(mount)?;
        let fdinfo = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", opened.as_raw_fd()))?;
        let id = fdinfo
            .lines()
            .find_map(|line| line.strip_prefix("mnt_id:").map(str::trim))
            .ok_or("missing open mount ID")?;
        let info = std::fs::read_to_string("/proc/self/mountinfo")?;
        let (head, tail) = info
            .lines()
            .find(|line| line.split_whitespace().next() == Some(id))
            .and_then(|line| line.split_once(" - "))
            .ok_or("missing mount record for the open descriptor")?;
        let mut fields = head.split_whitespace().skip(2).map(str::to_owned);
        let device = fields.next().ok_or("missing device number")?;
        let root = fields.next().ok_or("missing filesystem root")?;
        let flags = fields.nth(1).ok_or("missing mount flags")?;
        let filesystem = tail
            .split_whitespace()
            .next()
            .ok_or("missing filesystem type")?;
        Ok((device, filesystem.to_owned(), root, flags))
    };
    let (device, filesystem, original_root, flags) = view()?;
    assert_eq!(original_root, "/");
    assert!(
        Command::new("mount")
            .arg("--bind")
            .arg(mount.join("workspace"))
            .arg(mount)
            .status()?
            .success()
    );
    let _substitution = HeldBind(mount.to_owned());
    assert_eq!(view()?, (device, filesystem, "/workspace".into(), flags));
    let substituted = spawner
        .check_room_mount()
        .err()
        .ok_or("spawner accepted the workspace subtree")?;
    assert!(
        substituted.contains("expected encrypted ext4 volume"),
        "spawner did not reject the subtree before launching the helper: {substituted}"
    );
    Ok(())
}

/// Stage ordinary room artifacts and inject a record in the private
/// namespace. Only the record publisher is a fixture here.
fn room_admission_fixture(
    place: &Place,
    supervisor: &Path,
    foreground: Option<&Path>,
) -> Result<(String, String, [HeldBind; 2]), Failed> {
    let fixture_etc = place.dir.join("etc");
    let fixture_share = place.dir.join("share");
    let system = place.dir.join("system");
    let image_dir = fixture_share.join("hatchling/room");
    std::fs::create_dir_all(fixture_etc.join("hatchling"))?;
    std::fs::create_dir_all(&image_dir)?;
    for name in ["passwd", "group", "nsswitch.conf", "ld.so.cache"] {
        let source = Path::new("/etc").join(name);
        if source.is_file() {
            std::fs::copy(source, fixture_etc.join(name))?;
        }
    }
    for name in ["workspace", "session", "proc", "dev"] {
        std::fs::create_dir_all(system.join(name))?;
    }
    let libexec = system.join("usr/local/libexec");
    std::fs::create_dir_all(&libexec)?;
    std::fs::copy(supervisor, libexec.join("hatchling-room"))?;
    if let Some(foreground) = foreground {
        std::fs::create_dir(system.join("bin"))?;
        std::fs::copy(foreground, system.join("bin/foreground"))?;
    }
    // Give verity a data image with more than one block.
    let mut data = vec![0_u8; 8192];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut data)?;
    std::fs::write(system.join("fixture"), data)?;
    let image = image_dir.join("room-system-x86_64.squashfs");
    let tree = image_dir.join("room-system-x86_64.verity");
    let root_path = image_dir.join("room-system-x86_64.root-hash");
    assert!(
        Command::new("mksquashfs")
            .arg(&system)
            .arg(&image)
            .args(["-noappend", "-all-root", "-no-progress", "-processors", "1"])
            .status()?
            .success()
    );
    let digest = format!("{:x}", Sha256::digest(std::fs::read(&image)?));
    assert!(
        Command::new("/usr/sbin/veritysetup")
            .args([
                "format",
                "--no-superblock",
                "--format=1",
                "--hash=sha256",
                "--data-block-size=4096",
                "--hash-block-size=4096",
                "--salt"
            ])
            .arg(&digest)
            .arg("--root-hash-file")
            .arg(&root_path)
            .arg(&image)
            .arg(tree)
            .status()?
            .success()
    );
    let root = std::fs::read_to_string(root_path)?.trim().to_owned();
    let bind = |from: &Path, to: &str| -> Result<HeldBind, Failed> {
        assert!(
            Command::new("mount")
                .arg("--bind")
                .arg(from)
                .arg(to)
                .status()?
                .success()
        );
        Ok(HeldBind(PathBuf::from(to)))
    };
    let binds = [
        bind(&fixture_etc, "/etc")?,
        bind(&fixture_share, "/usr/local/share")?,
    ];
    Ok((root, digest, binds))
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
    let Some(_disk_tests) = as_root("a_room_disk_is_made_opened_closed_and_wiped_for_real") else {
        return;
    };
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

/// On a machine whose root mount isn't shared (WSL, a container), a
/// service started while the room disk was open kept a copy of its mount:
/// a close took the mount down here and left it there, the mapping stayed
/// open with the disk's key in the kernel, and the next change to the
/// vault's key was refused. The room disk's mount is under a shared mount
/// now, so its unmount reaches the copy, and the mapping closes.
#[test]
fn a_service_started_while_the_room_disk_is_open_does_not_keep_it_open() {
    let Some(_disk_tests) =
        as_root("a_service_started_while_the_room_disk_is_open_does_not_keep_it_open")
    else {
        return;
    };
    let place = Place::private("service").unwrap();
    let keeper = Keeper::start(&place, "keeper").unwrap();
    let run = |op| room_disk::run(&place.disk, &keeper.spawner, op);
    run(RoomDiskOp::Create).unwrap();
    run(RoomDiskOp::Open).unwrap();
    let mount = place.disk.mount_point();
    let room = mount.parent().unwrap();

    let service = Service::start().unwrap();
    assert!(
        service.sees_mounted(&mount).unwrap(),
        "the service took no copy of the room disk's mount"
    );
    run(RoomDiskOp::Close).unwrap();
    assert!(
        !service.sees_mounted(&mount).unwrap(),
        "the room disk came down here, and a service kept it mounted"
    );
    assert!(!place.mapped(), "a service's copy kept the mapping open");
    assert!(
        !hatchling_sys::is_mount_point(room).unwrap(),
        "the room directory's own bind was left after the close"
    );
    // And it opens and closes again, the service still there.
    run(RoomDiskOp::Open).unwrap();
    run(RoomDiskOp::Close).unwrap();
    assert!(!place.mapped());
    drop(service);
}

/// A change of the vault's key, for real: the room disk's key slot moves
/// from one vault's key to another's, closed, without touching the data
/// area; the old key then fails, and the new one opens it and it mounts.
/// Then it moves back while the disk is open and mounted, which goes on
/// working, and the first vault's key opens it again.
#[test]
fn a_room_disks_key_moves_with_the_vaults_for_real() {
    let Some(_disk_tests) = as_root("a_room_disks_key_moves_with_the_vaults_for_real") else {
        return;
    };
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
    // Rotation closes the disk before changing a key slot, then its bytes
    // are available only after the keeper with the new key reopens it.
    assert_eq!(room_disk::rotate(&place.disk, &two, &one)?, Rotated::Done);
    assert!(!place.mapped(), "rotation left the room disk mapped");
    assert!(room_disk::run(&place.disk, &other.spawner, RoomDiskOp::Open).is_err());
    room_disk::run(&place.disk, &keeper.spawner, RoomDiskOp::Open)?;
    assert_eq!(std::fs::read(mount.join("canary"))?, b"moved");
    std::fs::write(mount.join("after"), b"still writable")?;
    room_disk::run(&place.disk, &keeper.spawner, RoomDiskOp::Close)?;
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
    let Some(_disk_tests) =
        as_root("an_image_that_is_a_link_is_refused_and_what_it_names_untouched")
    else {
        return;
    };
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
