//! Open the authenticated room image for the trusted launcher.
//!
//! The caller supplies the root and salt authenticated by the installer.
//! Neither is read from a file beside the image. The same pinned files
//! are used for full verification and activation, and the kernel keeps
//! checking blocks after activation.

use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use rustix::fs::{FileType, Mode, OFlags, ResolveFlags, Stat};
use sha2::{Digest as _, Sha256};

const IMAGE: &str = "room-system-x86_64.squashfs";
const TREE: &str = "room-system-x86_64.verity";
const TOOL: &str = "/usr/sbin/veritysetup";
const DEADLINE: Duration = Duration::from_secs(30);
const BLOCK_SIZE: i64 = 4096;

/// The installer-owned image and hash tree, pinned before verification.
#[derive(Debug)]
pub struct PinnedRoomImage {
    image: OwnedFd,
    tree: OwnedFd,
}

impl PinnedRoomImage {
    /// Pin the two regular files in an already trusted installation
    /// directory. The installer must publish fresh root-owned inodes
    /// without leaving another writer holding them open.
    ///
    /// # Errors
    ///
    /// Refuses missing, linked, writable-by-others, or unaligned inputs.
    pub fn pin(directory: BorrowedFd<'_>) -> io::Result<Self> {
        let metadata = rustix::fs::fstat(directory)?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
            || !root_only_writes(metadata.st_uid, metadata.st_mode)
        {
            return Err(refuse("room image directory is not controlled by root"));
        }
        Ok(Self {
            image: pin_file(directory, IMAGE)?,
            tree: pin_file(directory, TREE)?,
        })
    }

    /// Verify all image blocks, then activate the same files as read-only.
    /// The caller supplies the expected verity root and image SHA-256
    /// (the build's salt) from attested release data.
    ///
    /// # Errors
    ///
    /// Refuses invalid hashes, failed verification, and failed activation.
    /// After attempting activation, a failure returns the mapper identity
    /// together with any cleanup error so the spawner can reconcile it.
    pub fn activate(
        self,
        root_hash: &str,
        image_sha256: &str,
    ) -> Result<RoomImageMapper, RoomImageActivationError> {
        if !valid_digest(root_hash) || !valid_digest(image_sha256) {
            return Err(refuse("room image requires a lowercase SHA-256 root and salt").into());
        }
        // Verity reads the geometry from the file. A hash-tree block can
        // act as the whole data image under the original root. Bind the
        // exact bytes before giving the pinned files to veritysetup.
        verify_image_digest(File::from(self.image.try_clone()?), image_sha256)?;
        let mut nonce = [0_u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let name = format!("hatchling-room-verity-{:032x}", u128::from_be_bytes(nonce));
        let mut mapper = Mapper {
            device: Path::new("/dev/mapper").join(&name),
            name,
            tool: None,
        };
        if mapper.sysfs()?.is_some() {
            return Err(refuse("room image mapper name is already in use").into());
        }
        // Activation alone accepts a bad root until the first kernel read.
        // Finish full verification before creating a mapper for the room.
        if let Err(failure) = self.run("verify", None, root_hash, image_sha256) {
            if failure.pending.is_none() {
                return Err(failure.source.into());
            }
            mapper.tool = failure.pending;
            return Err(PendingRoomImageMapper(mapper).rollback(failure.source));
        }
        if let Err(failure) = self.run("open", Some(&mapper.name), root_hash, image_sha256) {
            mapper.tool = failure.pending;
            return Err(PendingRoomImageMapper(mapper).rollback(failure.source));
        }
        let mapper = RoomImageMapper(mapper);
        if let Err(error) = mapper.pin_device() {
            return Err(mapper.rollback(error));
        }
        Ok(mapper)
    }

    fn run(
        &self,
        action: &str,
        name: Option<&str>,
        root_hash: &str,
        salt: &str,
    ) -> Result<(), ToolFailure> {
        let image = self.image.try_clone()?;
        let tree = self.tree.try_clone()?;
        let mut command = verity_command()?;
        command
            .args([
                action,
                "--no-superblock",
                "--format=1",
                "--hash=sha256",
                "--data-block-size=4096",
                "--hash-block-size=4096",
                "--salt",
                salt,
            ])
            .arg(format!("/proc/self/fd/{}", image.as_raw_fd()));
        command.args(name);
        command
            .arg(format!("/proc/self/fd/{}", tree.as_raw_fd()))
            .arg(root_hash);
        run_tool(command, action, Some((image, tree)))
    }
}

fn verify_image_digest(mut image: impl io::Read, expected: &str) -> io::Result<()> {
    let mut digest = Sha256::new();
    io::copy(&mut image, &mut digest)?;
    if format!("{:x}", digest.finalize()) != expected {
        return Err(refuse(
            "cannot verify room image SHA-256 against the attested digest",
        ));
    }
    Ok(())
}

/// An image could not be admitted. If a helper or mapper might remain,
/// the caller retains the handle even when the mapper appears absent.
#[derive(Debug, thiserror::Error)]
pub enum RoomImageActivationError {
    /// Startup failed with no mapper or helper to reconcile.
    #[error(transparent)]
    BeforeActivation(#[from] io::Error),

    /// Startup failed and the spawner owns the cleanup handle.
    #[error("room image startup failed for {mapper:?}: {source} (cleanup error: {cleanup:?})")]
    StartupCleanup {
        /// The error that aborted startup.
        #[source]
        source: io::Error,
        /// A failed attempt to remove the mapper, if any.
        cleanup: Option<io::Error>,
        /// The trusted spawner's handle for retrying or confirming cleanup.
        mapper: PendingRoomImageMapper,
    },
}

/// A kernel mapper owned by the spawner. Its owner closes it
/// explicitly after all mounts and room processes are gone.
#[derive(Debug)]
#[must_use = "close the verified mapper after draining the room"]
pub struct RoomImageMapper(Mapper);

impl RoomImageMapper {
    pub(crate) fn pin_device(&self) -> io::Result<(OwnedFd, u64)> {
        self.0.pin_device()
    }

    /// Send the verified block pin to the trusted launcher over a dedicated
    /// private socket. The spawner keeps this mapper and closes it only once
    /// all room processes have exited.
    ///
    /// # Errors
    ///
    /// Fails if the mapper cannot be pinned or the handoff cannot be sent.
    /// The caller must terminate the launch on error.
    pub fn send_to_launcher(&self, mut control: &UnixStream) -> Result<(), crate::Error> {
        let (block, expected) = self.pin_device().map_err(handoff_error)?;
        control
            .set_write_timeout(Some(DEADLINE))
            .map_err(handoff_error)?;
        control
            .write_all(&expected.to_le_bytes())
            .map_err(handoff_error)?;
        crate::net::send_fd(control.as_fd(), block.as_fd())
    }

    /// The device path for the launcher's private read-only mount.
    /// The launch child must check its mounted device before entering.
    #[must_use]
    pub fn device(&self) -> &Path {
        &self.0.device
    }

    /// Try to remove a mapper after failed startup. The resulting error
    /// carries its identity, so the spawner can retry if removal is busy
    /// or reconcile a mapping that may still be coming up after timeout.
    #[must_use]
    pub fn rollback(self, source: io::Error) -> RoomImageActivationError {
        PendingRoomImageMapper(self.0).rollback(source)
    }

    /// Remove the mapper without deferring removal. If it is busy, the
    /// caller keeps ownership and can retry after the holder is gone.
    ///
    /// # Errors
    ///
    /// Returns an error if the kernel still lists the mapper or cannot
    /// confirm that it is gone.
    pub fn close(&mut self) -> io::Result<()> {
        self.0.close()
    }
}

pub(crate) fn receive_launcher_device(
    mut control: &UnixStream,
) -> Result<(OwnedFd, u64), crate::Error> {
    control
        .set_read_timeout(Some(DEADLINE))
        .map_err(handoff_error)?;
    let mut device = [0; 8];
    control.read_exact(&mut device).map_err(handoff_error)?;
    let expected = u64::from_le_bytes(device);
    let block = crate::net::recv_fd(control.as_fd())?;
    let stat = rustix::fs::fstat(&block).map_err(|error| handoff_error(error.into()))?;
    let flags = rustix::fs::fcntl_getfl(&block).map_err(|error| handoff_error(error.into()))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::BlockDevice
        || !flags.contains(OFlags::PATH)
        || stat.st_rdev != expected
    {
        return Err(handoff_error(refuse(
            "descriptor is not the pinned room block device",
        )));
    }
    Ok((block, expected))
}

const fn handoff_error(source: io::Error) -> crate::Error {
    crate::Error::Mount {
        operation: "hand off verified room image",
        source,
    }
}

/// A mapper to reconcile after failed activation. It cannot admit a room.
#[derive(Debug)]
#[must_use = "retain this mapper until cleanup succeeds"]
pub struct PendingRoomImageMapper(Mapper);

impl PendingRoomImageMapper {
    /// The kernel name to record for restart reconciliation.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0.name
    }

    /// Retry cleanup after any outstanding verity helper has exited.
    ///
    /// # Errors
    ///
    /// Returns an error while a helper or mapper is still present or
    /// the kernel cannot confirm removal.
    pub fn close(&mut self) -> io::Result<()> {
        self.0.close()
    }

    fn rollback(mut self, source: io::Error) -> RoomImageActivationError {
        let cleanup = self.close().err();
        RoomImageActivationError::StartupCleanup {
            source,
            cleanup,
            mapper: self,
        }
    }
}

#[derive(Debug)]
struct Mapper {
    name: String,
    device: PathBuf,
    tool: Option<ToolChild>,
}

impl Mapper {
    fn close(&mut self) -> io::Result<()> {
        // An outstanding opener can still create a mapper, and an
        // outstanding closer must not race a second close.
        if let Some(tool) = &mut self.tool {
            if !tool.exited()? {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "verity helper has not exited",
                ));
            }
            self.tool = None;
        }
        if self.sysfs()?.is_none() {
            return Ok(());
        }
        let mut command = verity_command()?;
        command.args(["close", &self.name]);
        if let Err(failure) = run_tool(command, "close", None) {
            self.tool = failure.pending;
            return Err(failure.source);
        }
        if self.sysfs()?.is_some() {
            return Err(io::Error::other(format!("{} is still mapped", self.name)));
        }
        Ok(())
    }

    fn sysfs(&self) -> io::Result<Option<PathBuf>> {
        find_mapper(Path::new("/sys/block"), &self.name)
    }

    fn pin_device(&self) -> io::Result<(OwnedFd, u64)> {
        let kernel = self
            .sysfs()?
            .ok_or_else(|| refuse("room image mapper is absent"))?;
        let node = rustix::fs::open(
            &self.device,
            OFlags::PATH.union(OFlags::CLOEXEC),
            Mode::empty(),
        )?;
        let stat = rustix::fs::fstat(&node)?;
        let rdev = stat.st_rdev;
        let device = format!("{}:{}", rustix::fs::major(rdev), rustix::fs::minor(rdev));
        validate_mapper_device(
            FileType::from_raw_mode(stat.st_mode) == FileType::BlockDevice,
            &device,
            fs::read_to_string(kernel.join("dev"))?.trim(),
            fs::read_to_string(kernel.join("ro"))?.trim(),
        )?;
        Ok((node, rdev))
    }
}

fn find_mapper(devices: &Path, expected: &str) -> io::Result<Option<PathBuf>> {
    for entry in fs::read_dir(devices)? {
        let entry = entry?;
        if !entry.file_name().as_encoded_bytes().starts_with(b"dm-") {
            continue;
        }
        let path = entry.path();
        match fs::read_to_string(path.join("dm/name")) {
            Ok(name) if name.trim_end() == expected => return Ok(Some(path)),
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn validate_mapper_device(
    block_device: bool,
    node: &str,
    kernel: &str,
    read_only: &str,
) -> io::Result<()> {
    if !block_device || node != kernel || read_only != "1" {
        return Err(refuse(
            "room image mapper is not the kernel's read-only device",
        ));
    }
    Ok(())
}

const fn root_only_writes(uid: u32, mode: u32) -> bool {
    uid == 0 && mode & 0o022 == 0
}

fn pin_file(directory: BorrowedFd<'_>, name: &str) -> io::Result<OwnedFd> {
    // Inspect an O_PATH descriptor before reopening it for reads. Special
    // files cannot make a privileged reader wait or touch a device.
    let pin = rustix::fs::openat2(
        directory,
        name,
        OFlags::PATH.union(OFlags::CLOEXEC),
        Mode::empty(),
        ResolveFlags::BENEATH
            .union(ResolveFlags::NO_SYMLINKS)
            .union(ResolveFlags::NO_XDEV),
    )?;
    let metadata = rustix::fs::fstat(&pin)?;
    check_artifact(&metadata)?;
    Ok(rustix::fs::open(
        format!("/proc/self/fd/{}", pin.as_raw_fd()),
        OFlags::RDONLY.union(OFlags::CLOEXEC),
        Mode::empty(),
    )?)
}

fn check_artifact(metadata: &Stat) -> io::Result<()> {
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
        || !root_only_writes(metadata.st_uid, metadata.st_mode)
        || metadata.st_size <= 0
        || metadata.st_size % BLOCK_SIZE != 0
    {
        return Err(refuse(
            "room image input is not a root-controlled aligned regular file",
        ));
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn refuse(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

fn verity_command() -> io::Result<Command> {
    let metadata = fs::symlink_metadata(TOOL)?;
    check_tool(metadata.is_file(), metadata.uid(), metadata.mode())?;
    crate::trusted_dir_chain(Path::new("/usr/sbin"), &[0]).map_err(io::Error::other)?;
    Ok(Command::new(TOOL))
}

fn check_tool(regular_file: bool, uid: u32, mode: u32) -> io::Result<()> {
    if !regular_file || !root_only_writes(uid, mode) {
        return Err(refuse("veritysetup is not a root-controlled executable"));
    }
    Ok(())
}

#[derive(Debug)]
enum ToolChild {
    Supervised(crate::Child),
    Spawned(std::process::Child),
}

impl ToolChild {
    fn exited(&mut self) -> io::Result<bool> {
        match self {
            Self::Supervised(child) => {
                child.try_wait().map_err(io::Error::other)?;
                Ok(child.is_reaped())
            }
            Self::Spawned(child) => Ok(child.try_wait()?.is_some()),
        }
    }
}

#[derive(Debug)]
struct ToolFailure {
    source: io::Error,
    pending: Option<ToolChild>,
}

impl From<io::Error> for ToolFailure {
    fn from(source: io::Error) -> Self {
        Self {
            source,
            pending: None,
        }
    }
}

fn run_tool(
    mut command: Command,
    action: &str,
    inputs: Option<(OwnedFd, OwnedFd)>,
) -> Result<(), ToolFailure> {
    command
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: this hook performs only descriptor syscalls in the child.
    // Owned clones keep the selected descriptors valid until exec. The
    // parent's flags stay close-on-exec and other child fds stay sealed.
    unsafe {
        command.pre_exec(move || {
            crate::fds::seal_inherited_fds()?;
            if let Some((image, tree)) = &inputs {
                let flags = rustix::io::FdFlags::empty();
                rustix::io::fcntl_setfd(image, flags)?;
                rustix::io::fcntl_setfd(tree, flags)?;
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let supervised = i32::try_from(child.id())
        .ok()
        .and_then(crate::ChildPid::new)
        .ok_or_else(|| refuse("veritysetup child has no valid pid"))
        .and_then(|pid| crate::Child::new(pid).map_err(io::Error::other));
    let child = match supervised {
        Ok(supervised) => supervised,
        Err(source) => {
            let _ = child.kill();
            return Err(ToolFailure {
                source,
                pending: Some(ToolChild::Spawned(child)),
            });
        }
    };
    let status = match child.wait_deadline(DEADLINE) {
        Ok(status) => status,
        Err(error) => {
            let pending = if child.is_reaped() {
                None
            } else {
                let _ = child.terminate();
                Some(ToolChild::Supervised(child))
            };
            return Err(ToolFailure {
                source: io::Error::other(error),
                pending,
            });
        }
    };
    let code = crate::child_exit_code(status);
    if code != 0 {
        return Err(
            io::Error::other(format!("veritysetup {action} failed (status {code})")).into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::DirBuilderExt as _;

    use super::*;

    #[test]
    fn launcher_device_refuses_a_non_block_pin_with_the_claimed_identity() {
        let (owner, launcher) = UnixStream::pair().unwrap();
        let null = rustix::fs::open("/dev/null", OFlags::PATH, Mode::empty()).unwrap();
        let expected = rustix::fs::fstat(&null).unwrap().st_rdev;
        (&owner).write_all(&expected.to_le_bytes()).unwrap();
        crate::net::send_fd(owner.as_fd(), null.as_fd()).unwrap();
        assert!(matches!(
            receive_launcher_device(&launcher),
            Err(crate::Error::Mount { operation: "hand off verified room image", source })
                if source.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn launcher_device_refuses_an_incomplete_handoff() {
        let (owner, launcher) = UnixStream::pair().unwrap();
        (&owner).write_all(&[0; 7]).unwrap();
        drop(owner);
        assert!(matches!(
            receive_launcher_device(&launcher),
            Err(crate::Error::Mount { operation: "hand off verified room image", source })
                if source.kind() == io::ErrorKind::UnexpectedEof
        ));

        let (owner, launcher) = UnixStream::pair().unwrap();
        (&owner).write_all(&[0; 8]).unwrap();
        drop(owner);
        assert!(matches!(
            receive_launcher_device(&launcher),
            Err(crate::Error::Net {
                operation: "receive fd",
                ..
            })
        ));
    }

    #[test]
    fn handoff_error_preserves_the_underlying_io_failure() {
        let error = handoff_error(io::Error::from_raw_os_error(libc::EPIPE));
        assert!(matches!(
            error,
            crate::Error::Mount { operation: "hand off verified room image", source }
                if source.raw_os_error() == Some(libc::EPIPE)
        ));
    }

    #[test]
    fn expected_hashes_use_the_exact_lowercase_sha256_encoding() {
        // The input below is /dev/null, so its digest is the SHA-256 of empty data.
        let valid = format!("{:x}", Sha256::digest([]));
        assert!(valid_digest(&valid));
        for refused in [
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
        ] {
            assert!(!valid_digest(&refused));
        }
        let malformed = valid.get(..63).unwrap();
        for (root, digest) in [(malformed, valid.as_str()), (valid.as_str(), malformed)] {
            let pinned = PinnedRoomImage {
                image: File::open("/dev/null").unwrap().into(),
                tree: File::open("/dev/null").unwrap().into(),
            };
            assert!(
                pinned
                    .activate(root, digest)
                    .unwrap_err()
                    .to_string()
                    .contains("lowercase")
            );
        }
    }

    #[test]
    fn whole_image_digest_distinguishes_different_bytes() {
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_image_digest(b"abc".as_slice(), expected).is_ok());
        assert!(verify_image_digest(b"abc\0".as_slice(), expected).is_err());
    }

    #[test]
    fn artifact_mode_requires_root_and_disallows_both_other_writers() {
        let mut metadata = rustix::fs::fstat(File::open("/dev/null").unwrap()).unwrap();
        metadata.st_uid = 0;
        metadata.st_mode = libc::S_IFREG | 0o644;
        metadata.st_size = BLOCK_SIZE;
        assert!(root_only_writes(0, 0o644));
        assert!(check_artifact(&metadata).is_ok());
        for mode in [0o664, 0o646] {
            assert!(!root_only_writes(0, mode));
            metadata.st_mode = libc::S_IFREG | mode;
            assert!(check_artifact(&metadata).is_err());
        }
        metadata.st_mode = libc::S_IFREG | 0o644;
        metadata.st_uid = 1;
        assert!(!root_only_writes(1, 0o644));
        assert!(check_artifact(&metadata).is_err());
        metadata.st_uid = 0;
        for size in [0, -BLOCK_SIZE, BLOCK_SIZE + 1] {
            metadata.st_size = size;
            assert!(check_artifact(&metadata).is_err(), "accepted size {size}");
        }
        metadata.st_size = BLOCK_SIZE;
        metadata.st_mode = libc::S_IFBLK | 0o644;
        assert!(check_artifact(&metadata).is_err());
    }

    #[test]
    fn every_mapper_device_attribute_must_match() {
        assert!(validate_mapper_device(true, "254:1", "254:1", "1").is_ok());
        assert!(validate_mapper_device(false, "254:1", "254:1", "1").is_err());
        assert!(validate_mapper_device(true, "254:2", "254:1", "1").is_err());
        assert!(validate_mapper_device(true, "254:1", "254:1", "0").is_err());
    }

    struct TestDevices(PathBuf);

    impl Drop for TestDevices {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn find_mapper_ignores_unrelated_and_vanished_devices_but_preserves_errors() {
        let path =
            std::env::temp_dir().join(format!("hatchling-verity-sysfs-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let devices = TestDevices(path);
        for path in ["dm-1/dm", "dm-2/dm", "loop1/dm"] {
            fs::create_dir_all(devices.0.join(path)).unwrap();
        }
        fs::write(devices.0.join("dm-1/dm/name"), "other\n").unwrap();
        fs::write(devices.0.join("loop1/dm/name"), "expected\n").unwrap();
        assert_eq!(find_mapper(&devices.0, "expected").unwrap(), None);
        fs::write(devices.0.join("dm-2/dm/name"), "expected\n").unwrap();
        assert_eq!(
            find_mapper(&devices.0, "expected").unwrap(),
            Some(devices.0.join("dm-2"))
        );
        fs::create_dir_all(devices.0.join("dm-3/dm/name")).unwrap();
        assert!(find_mapper(&devices.0, "absent").is_err());
    }

    #[test]
    fn pin_file_refuses_a_symlink_before_opening_its_target() {
        let dir = File::open("/tmp").unwrap();
        let name = format!("hatchling-verity-symlink-{}", std::process::id());
        let path = Path::new("/tmp").join(&name);
        std::os::unix::fs::symlink("/dev/null", &path).unwrap();
        let result = pin_file(dir.as_fd(), &name);
        fs::remove_file(path).unwrap();
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ELOOP));
    }

    #[test]
    fn a_missing_kernel_mapping_cannot_be_supplied_as_a_device() {
        let mut mapper = Mapper {
            name: format!("hatchling-verity-absent-{}", std::process::id()),
            device: PathBuf::from("/dev/null"),
            tool: None,
        };
        assert!(mapper.sysfs().unwrap().is_none());
        assert!(
            mapper
                .pin_device()
                .unwrap_err()
                .to_string()
                .contains("absent")
        );
        mapper.close().unwrap();
    }

    #[test]
    fn cleanup_cannot_report_absence_while_the_opener_is_alive() {
        let process = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = i32::try_from(process.id())
            .ok()
            .and_then(crate::ChildPid::new)
            .unwrap();
        let child = crate::Child::new(pid).unwrap();
        drop(process);
        let mut pending = PendingRoomImageMapper(Mapper {
            name: format!("hatchling-verity-absent-{}", std::process::id()),
            device: PathBuf::from("/dev/null"),
            tool: Some(ToolChild::Supervised(child)),
        });
        assert!(!pending.0.tool.as_mut().unwrap().exited().unwrap());
        let result = pending.close();
        // Kill and reap before asserting so a hand break cannot leave a process.
        if let Some(ToolChild::Supervised(child)) = &mut pending.0.tool {
            assert!(!child.is_reaped());
            child.terminate().unwrap();
            child.wait_deadline(Duration::from_secs(3)).unwrap();
            assert!(child.is_reaped());
        }
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        pending.close().unwrap();
    }

    #[test]
    fn cleanup_keeps_and_reaps_the_standard_child_fallback() {
        let process = Command::new("/bin/sleep").arg("2").spawn().unwrap();
        let proc_path = Path::new("/proc").join(process.id().to_string());
        let mut pending = PendingRoomImageMapper(Mapper {
            name: format!("hatchling-verity-fallback-{}", std::process::id()),
            device: PathBuf::from("/dev/null"),
            tool: Some(ToolChild::Spawned(process)),
        });
        let result = pending.close();
        if let Some(ToolChild::Spawned(process)) = &mut pending.0.tool {
            process.kill().unwrap();
        }
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        let until = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            match pending.close() {
                Ok(()) => break,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < until =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                other => other.unwrap(),
            }
        }
        assert!(!proc_path.exists(), "the helper was left unreaped");
    }

    #[test]
    fn verity_command_uses_the_system_executable_when_available() {
        if Path::new(TOOL).exists() {
            assert_eq!(verity_command().unwrap().get_program(), TOOL);
        } else {
            assert!(verity_command().is_err());
        }
        assert!(check_tool(true, 0, 0o755).is_ok());
        assert!(check_tool(false, 0, 0o755).is_err());
        assert!(check_tool(true, 1, 0o755).is_err());
        assert!(check_tool(true, 0, 0o775).is_err());
    }

    #[test]
    fn run_tool_passes_only_the_two_selected_files_and_clears_environment() {
        let image: OwnedFd = File::open("/dev/null").unwrap().into();
        let tree: OwnedFd = File::open("/dev/null").unwrap().into();
        let spare = rustix::io::fcntl_dupfd_cloexec(&image, 100).unwrap();
        crate::keep_across_exec(spare.as_fd()).unwrap();
        let mut command = Command::new("/bin/sh");
        command.env("HATCHLING_TEST_VERITY_SENTINEL", "must be cleared");
        command.args([
            "-c",
            "test -e \"/proc/self/fd/$1\" && test -e \"/proc/self/fd/$2\" && \
             test ! -e \"/proc/self/fd/$3\" && test -z \"$HATCHLING_TEST_VERITY_SENTINEL\"",
            "test-verity-fds",
            &image.as_raw_fd().to_string(),
            &tree.as_raw_fd().to_string(),
            &spare.as_raw_fd().to_string(),
        ]);
        run_tool(command, "test", Some((image, tree))).unwrap();
    }
}
