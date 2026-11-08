//! The room disk (0.9): the agent's room keeps its work on an encrypted disk
//! image of its own, made, opened and closed by the spawner, as root.
//!
//! **What it is.** One sparse file of a fixed size (`HATCHLING_ROOM_DISK_SIZE`,
//! 10 GiB by default) in a directory of root's alone
//! (`HATCHLING_ROOM_DISK_DIR`, `/var/lib/hatchling/room`): LUKS2,
//! `aes-xts-plain64` with a 512-bit key, PBKDF2 at its floor, ext4 inside,
//! mounted `nodev,nosuid,noexec` and `errors=remount-ro` at `mnt/` beside the
//! image. Its record, `disk.json`, root's and 0600, sits beside it, never in
//! it. The room disk is on only where the spawner runs as root and has a
//! vault keeper.
//!
//! **When it's open.** The broker opens it at its start, once the vault is
//! unlocked, making it first if there's none ([`open_at_start`]): one that
//! can't be turns the room off, and the agent talks. Before the vault is
//! unlocked, a broker that starts closes what an earlier one left open
//! ([`close_stale`]), and stopping the service closes it
//! (`hatchling-broker room-disk close`, the service's `ExecStopPost`). So a
//! locked or stopped install holds none open. `lock`, `reset` and `restore`
//! close it with the service stopped, and confirm it's closed
//! ([`crate::backup`]).
//!
//! **When the vault's key changes,** the room disk's key slot changes with
//! it ([`rotate_with_vault`]): the process that changes it holds both vault
//! keys, derives the image's key from each, and adds the new key before it
//! removes the old one, so a crash at any point leaves a key that opens it,
//! and the next run picks up from what opens it then. That changes the lock,
//! not the disk's own key inside it: an old copy of the image, with the old
//! vault key, still reads a later copy. `vault-keyring lost-key` offers to
//! reset the room disk when a key is stolen for that reason.
//!
//! **Its key** is derived by the vault keeper from the master key, for that
//! image's own LUKS UUID ([`hatchling_vault::RoomRootKey`]). The spawner
//! makes a pipe, gives its reading end to cryptsetup as stdin, closes its own
//! copy, and hands the writing end to the keeper with the ask: the key goes
//! from the keeper into cryptsetup, and neither the spawner nor the broker
//! ever holds it.
//!
//! **What runs.** `/usr/sbin/cryptsetup` and `/usr/sbin/mke2fs` (`-t ext4`;
//! `mkfs.ext4` is a link to it, and a link is never run), each a regular
//! file of root's, writable by no one else, with every directory above it
//! the same ([`hatchling_sys::trusted_dir_chain`]). Each runs from an
//! argument list, never a shell, with only `PATH=/usr/sbin:/usr/bin` and
//! `LC_ALL=C` in its environment, under a deadline.
//!
//! **What's checked first.** Before any of them runs: the room directory and
//! every directory above it, the image (a regular file of root's, 0600, with
//! no other name, never a link), the mapping's name and, if one by that name
//! is there, that it's this image's, and the mount point (a private
//! directory, empty, and not mounted on). The broker names an operation and
//! nothing else: no path, size or name is ever its word.
//!
//! **What it doesn't do.** Discard: deleting files frees nothing on the disk
//! below, so the image never shrinks, and the shape of what's in use isn't
//! written there. Open never formats, so no mistake can make a workspace
//! empty: only create formats, and only an image it has just made.

use std::ffi::OsString;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use hatchling_core::framing::write_frame;
use hatchling_core::ui::{RoomDiskCondition, RoomDiskOff, RoomDiskView};
use hatchling_isolation::{IsolationBackend as _, Supervised as _};
use hatchling_vault::{MasterKey, RoomImageId, RoomKey, RoomRootKey};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::keeper::{RoomKeyAsk, RoomKeyFor};

/// Where the room disk is unless configured: beside Hatchling's state.
pub const DEFAULT_DIR: &str = "/var/lib/hatchling/room";

/// A mebibyte.
const MIB: u64 = 1024 * 1024;

/// A gibibyte.
const GIB: u64 = 1024 * MIB;

/// The room disk's size unless configured.
pub const DEFAULT_SIZE: u64 = 10 * GIB;

/// The smallest room disk: room for its header and a small file system.
pub const MIN_SIZE: u64 = 64 * MIB;

/// The largest room disk.
pub const MAX_SIZE: u64 = 1024 * GIB;

/// The LUKS2 header and key slots at the image's head, by `--offset`: what
/// a wipe overwrites, after which nothing opens what's left.
pub const HEADER_BYTES: u64 = 16 * MIB;

/// The head as `--offset` takes it: 512-byte sectors.
const OFFSET_SECTORS: &str = "32768";

/// How much more than the image's size must be free to make one: the
/// image is sparse, and a disk that fills under it fails its writes.
const FREE_MARGIN: u64 = GIB;

/// The image's name in the room directory.
const IMAGE: &str = "disk.img";

/// An image being made, until it's whole.
const NEW_IMAGE: &str = "disk.img.new";

/// The room disk's record.
const RECORD: &str = "disk.json";

/// The mount point's name in the room directory.
const MOUNT: &str = "mnt";

/// What a room disk's mapping is named, before its image's UUID.
pub const MAPPING_PREFIX: &str = "hatchling-room-";

/// The label of the image and of its file system.
const LABEL: &str = "hatchling-room";

/// cryptsetup, at the one path it's run from.
const CRYPTSETUP: &str = "/usr/sbin/cryptsetup";

/// mke2fs, run as `-t ext4`.
const MKE2FS: &str = "/usr/sbin/mke2fs";

/// Where a mapping's device is.
const MAPPER: &str = "/dev/mapper";

/// Where the kernel lists its block devices, a mapping's among them.
const SYS_BLOCK: &str = "/sys/block";

/// The programs' whole environment, but `LC_ALL`.
const TOOL_PATH: &str = "/usr/sbin:/usr/bin";

/// How long one program may take. Making the file system of the largest
/// room disk is the longest of them, and takes seconds.
const TOOL_TIMEOUT: Duration = Duration::from_secs(300);

/// The most of a program's error output a refusal quotes.
const SAID_BYTES: u64 = 2048;

/// What the broker asks of the room disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoomDiskOp {
    /// Make one: refused if one is there.
    Create,
    /// Open it and mount it.
    Open,
    /// Unmount it and close it.
    Close,
    /// Say how it is, in numbers.
    Status,
    /// Close it and wipe it: nothing opens what's left.
    Wipe,
}

/// Whether there's a room disk, and whether it's open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoomDiskState {
    /// None has been made.
    Absent,
    /// Made, and closed: no mapping of it.
    Closed,
    /// Its mapping is there, decrypted, but it isn't mounted: a close that
    /// failed (something else still holds it) or an open cut short. Never
    /// said to be closed.
    Mapped,
    /// Open, and mounted.
    Open,
}

/// How the room disk is, in numbers alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomDiskStatus {
    /// Whether there's one, and whether it's open.
    pub state: RoomDiskState,
    /// The size it's configured to be made at, in bytes.
    pub cap: u64,
    /// The image's size, when there is one.
    pub image_bytes: Option<u64>,
    /// What the image takes of the disk below: it never shrinks.
    pub on_disk: Option<u64>,
    /// What's in use inside, when it's open.
    pub used: Option<u64>,
    /// What's free inside, when it's open.
    pub free: Option<u64>,
    /// What's free on the disk below, for the image to grow into.
    pub host_free: Option<u64>,
}

/// The room disk the spawner keeps: where, how large, and what it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomDisk {
    /// The room directory: root's alone.
    dir: PathBuf,
    /// The size an image is made at.
    size: u64,
    /// cryptsetup.
    cryptsetup: PathBuf,
    /// mke2fs.
    mke2fs: PathBuf,
    /// Who may own the room directory, the directories above it, the
    /// programs and the image: the spawner's user, root.
    trusted: Vec<u32>,
    /// Where directory chains are checked from: `/`.
    top: PathBuf,
    /// Where the kernel lists its block devices.
    sys_block: PathBuf,
    /// How long one program may take.
    timeout: Duration,
}

impl RoomDisk {
    /// The room disk that `get` configures, made and kept by root.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] for a directory that isn't absolute, or a
    /// size that isn't one, or is outside [`MIN_SIZE`] to [`MAX_SIZE`].
    pub fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        use crate::environment as env;
        let dir = get(env::HATCHLING_ROOM_DISK_DIR.name)
            .filter(|raw| !raw.trim().is_empty())
            .map_or_else(|| PathBuf::from(DEFAULT_DIR), PathBuf::from);
        let size = get(env::HATCHLING_ROOM_DISK_SIZE.name)
            .filter(|raw| !raw.trim().is_empty())
            .map_or(Ok(DEFAULT_SIZE), |raw| parse_size(&raw))
            .map_err(|reason| Error::InvalidConfig {
                key: "HATCHLING_ROOM_DISK_SIZE",
                reason,
            })?;
        Self::at(dir, size)
    }

    /// A room disk at `dir`, made at `size`, made and kept by root, with
    /// the programs at their fixed paths.
    ///
    /// # Errors
    ///
    /// As [`RoomDisk::parse`].
    pub fn at(dir: PathBuf, size: u64) -> Result<Self, Error> {
        let plain = dir.is_absolute()
            && dir
                .components()
                .skip(1)
                .all(|part| matches!(part, std::path::Component::Normal(_)));
        if !plain {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_ROOM_DISK_DIR",
                reason: format!("must be a plain absolute directory, not {}", dir.display()),
            });
        }
        if !(MIN_SIZE..=MAX_SIZE).contains(&size) {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_ROOM_DISK_SIZE",
                reason: format!("must be from 64M to 1024G, not {size} bytes"),
            });
        }
        Ok(Self {
            dir,
            size,
            cryptsetup: PathBuf::from(CRYPTSETUP),
            mke2fs: PathBuf::from(MKE2FS),
            trusted: vec![0],
            top: PathBuf::from("/"),
            sys_block: PathBuf::from(SYS_BLOCK),
            timeout: TOOL_TIMEOUT,
        })
    }

    /// The room directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The image.
    #[must_use]
    pub fn image(&self) -> PathBuf {
        self.dir.join(IMAGE)
    }

    /// Where it's mounted when it's open.
    #[must_use]
    pub fn mount_point(&self) -> PathBuf {
        self.dir.join(MOUNT)
    }

    /// The image being made.
    fn new_image(&self) -> PathBuf {
        self.dir.join(NEW_IMAGE)
    }

    /// The record.
    fn record(&self) -> PathBuf {
        self.dir.join(RECORD)
    }

    /// Why the room directory, and every directory above it, isn't one
    /// only the trusted users can change, if it isn't; and it's private.
    fn check_dir(&self) -> Result<(), String> {
        hatchling_sys::trusted_dir_chain_from(&self.top, &self.dir, &self.trusted)
            .map_err(|error| format!("the room directory can't be trusted: {error}"))?;
        let mode = std::fs::symlink_metadata(&self.dir)
            .map_err(|error| format!("the room directory can't be looked at: {error}"))?
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "the room directory {} isn't private: mode {:o}",
                self.dir.display(),
                mode & 0o777
            ));
        }
        Ok(())
    }

    /// The room directory, made if absent (its directory checked first),
    /// then checked.
    fn ensure_dir(&self) -> Result<(), String> {
        if std::fs::symlink_metadata(&self.dir).is_err() {
            let above = self
                .dir
                .parent()
                .ok_or_else(|| String::from("the room directory has no directory above it"))?;
            hatchling_sys::trusted_dir_chain_from(&self.top, above, &self.trusted)
                .map_err(|error| format!("the room directory can't be made there: {error}"))?;
            hatchling_sys::create_private_dir(&self.dir)
                .map_err(|error| format!("the room directory can't be made: {error}"))?;
        }
        self.check_dir()
    }

    /// Why `program` can't be run as root, if it can't: it must be a
    /// regular file itself, a trusted user's, writable by no one else, in
    /// a trusted chain of directories.
    fn trusted_program(&self, program: &Path) -> Result<(), String> {
        let refused = |why: &str| format!("{} can't be run: {why}", program.display());
        let meta =
            std::fs::symlink_metadata(program).map_err(|error| refused(&error.to_string()))?;
        if !meta.file_type().is_file() {
            return Err(refused("it isn't a regular file itself"));
        }
        if !self.trusted.contains(&meta.uid()) {
            return Err(refused("it isn't root's"));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(refused("others can write to it"));
        }
        let above = program
            .parent()
            .ok_or_else(|| refused("it has no directory"))?;
        hatchling_sys::trusted_dir_chain_from(&self.top, above, &self.trusted)
            .map_err(|error| refused(&error.to_string()))
    }

    /// The image at `path`, if there is one: a regular file itself, a
    /// trusted user's, 0600 or tighter, with no other name.
    fn image_at(&self, path: &Path) -> Result<Option<std::fs::Metadata>, String> {
        let refused = |why: &str| format!("{} can't be the room disk: {why}", path.display());
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(refused(&error.to_string())),
        };
        if !meta.file_type().is_file() {
            return Err(refused("it isn't a regular file itself"));
        }
        if !self.trusted.contains(&meta.uid()) {
            return Err(refused("it isn't root's"));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(refused("others may read or write it"));
        }
        if meta.nlink() != 1 {
            return Err(refused("it has another name"));
        }
        Ok(Some(meta))
    }

    /// The image's own UUID, from its LUKS2 header: opened without following
    /// a link.
    fn image_id(&self) -> Result<RoomImageId, String> {
        let head = hatchling_sys::read_file_head(&self.image(), 512)
            .map_err(|error| format!("the room disk's header can't be read: {error}"))?;
        header_id(&head)
    }

    /// Whether the mapping named `name` is there: `false` if not, `true`
    /// if it's this image's, and refused if it's anything else's.
    fn mapped(&self, image: &RoomImageId, name: &str) -> Result<bool, String> {
        match self.mapping_uuid(name)? {
            None => Ok(false),
            Some(found) if found == dm_uuid(image, name) => Ok(true),
            Some(_) => Err(format!(
                "a mapping named {name} is there, and it isn't this room disk's"
            )),
        }
    }

    /// The device-mapper UUID of the mapping named `name`, if there's one,
    /// as the kernel lists it.
    fn mapping_uuid(&self, name: &str) -> Result<Option<String>, String> {
        let listed = match std::fs::read_dir(&self.sys_block) {
            Ok(listed) => listed,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("the block devices can't be listed: {error}")),
        };
        for entry in listed.filter_map(Result::ok) {
            if !entry.file_name().to_string_lossy().starts_with("dm-") {
                continue;
            }
            let dm = entry.path().join("dm");
            let named = std::fs::read_to_string(dm.join("name")).unwrap_or_default();
            if named.trim_end() == name {
                let uuid = std::fs::read_to_string(dm.join("uuid"))
                    .map_err(|error| format!("{name}'s uuid can't be read: {error}"))?;
                return Ok(Some(uuid.trim_end().to_owned()));
            }
        }
        Ok(None)
    }

    /// The mount point, made if absent, and whether it's mounted on: it
    /// must be a private directory of a trusted user's, and empty when it
    /// isn't mounted on.
    fn mount_point_mounted(&self) -> Result<bool, String> {
        let mount = self.mount_point();
        if std::fs::symlink_metadata(&mount).is_err() {
            hatchling_sys::create_private_dir(&mount)
                .map_err(|error| format!("the mount point can't be made: {error}"))?;
        }
        let meta = std::fs::symlink_metadata(&mount)
            .map_err(|error| format!("the mount point can't be looked at: {error}"))?;
        if !meta.file_type().is_dir() {
            return Err(String::from("the mount point isn't a directory itself"));
        }
        let mounted = hatchling_sys::is_mount_point(&mount).map_err(|error| error.to_string())?;
        if mounted {
            return Ok(true);
        }
        if !self.trusted.contains(&meta.uid()) || meta.mode() & 0o077 != 0 {
            return Err(String::from(
                "the mount point isn't a private directory of root's",
            ));
        }
        let empty = std::fs::read_dir(&mount)
            .map_err(|error| format!("the mount point can't be read: {error}"))?
            .next()
            .is_none();
        if empty {
            Ok(false)
        } else {
            Err(String::from("the mount point isn't empty"))
        }
    }

    /// Runs `program` with `args`: its environment cleared but for `PATH`
    /// and `LC_ALL`, its stdin the room disk's key from the keeper when
    /// `key` says which, else nothing, within the deadline. Its error
    /// output, cut short, says why it failed.
    fn run_tool(
        &self,
        program: &Path,
        args: &[OsString],
        key: Option<KeyFrom<'_>>,
    ) -> Result<(), String> {
        self.run_tool_giving(program, args, key, None)
    }

    /// [`RoomDisk::run_tool`], giving the program `second` too: a key on a
    /// pipe of its own, which the arguments name.
    fn run_tool_giving(
        &self,
        program: &Path,
        args: &[OsString],
        key: Option<KeyFrom<'_>>,
        second: Option<SecondKey<'_>>,
    ) -> Result<(), String> {
        let (code, said) = self.run_tool_keyed(program, args, key, second)?;
        if code == 0 {
            return Ok(());
        }
        let first = args.first().map(|arg| arg.to_string_lossy().into_owned());
        Err(format!(
            "{} {} failed with status {code}: {}",
            program.display(),
            first.unwrap_or_default(),
            said.trim()
        ))
    }

    /// [`RoomDisk::run_tool`]'s run, and how it ended: its exit status and
    /// what it said on its error output, cut short. A program that can't
    /// start, or runs past its deadline, fails.
    fn run_tool_exit(
        &self,
        program: &Path,
        args: &[OsString],
        key: Option<KeyFrom<'_>>,
    ) -> Result<(i32, String), String> {
        self.run_tool_keyed(program, args, key, None)
    }

    /// [`RoomDisk::run_tool_exit`], giving the program `second` too.
    fn run_tool_keyed(
        &self,
        program: &Path,
        args: &[OsString],
        key: Option<KeyFrom<'_>>,
        second: Option<SecondKey<'_>>,
    ) -> Result<(i32, String), String> {
        let named = || {
            let first = args.first().map(|arg| arg.to_string_lossy().into_owned());
            format!("{} {}", program.display(), first.unwrap_or_default())
        };
        // Its error output is a socket, which can be read without waiting
        // once it's done.
        let (stderr, its_stderr) =
            UnixStream::pair().map_err(|error| format!("{} can't start: {error}", named()))?;
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", TOOL_PATH)
            .env("LC_ALL", "C")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::from(OwnedFd::from(its_stderr)));
        let mut into = None;
        command.stdin(Stdio::null());
        if key.is_some() {
            let (read, write) = hatchling_sys::pipe().map_err(|error| error.to_string())?;
            command.stdin(Stdio::from(read));
            into = Some(write);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("{} can't start: {error}", named()))?;
        // Its copies of the pipe's reading end and of its error output's
        // end go with it, and so does the second key's reading end, which
        // crossed into the program: it holds the only ones.
        drop(command);
        let second = second.map(|second| {
            drop(second.read);
            (second.write, second.key)
        });
        let supervised = hatchling_isolation::platform()
            .supervise(&mut child)
            .map_err(|error| format!("{} can't be supervised: {error}", named()))?;
        drop(child);
        if let (Some(write), Some(key)) = (into, key) {
            let handed = match key {
                KeyFrom::Keeper(keeper, image, purpose) => ask_key(keeper, image, purpose, write),
                KeyFrom::Held(key) => hatchling_sys::write_whole_to_pipe(write, key.expose())
                    .map_err(|error| format!("the room disk's key can't be given: {error}")),
            };
            if let Err(reason) = handed {
                let _ = supervised.terminate();
                return Err(reason);
            }
        }
        if let Some((write, key)) = second {
            if let Err(error) = hatchling_sys::write_whole_to_pipe(write, key.expose()) {
                let _ = supervised.terminate();
                return Err(format!("the room disk's new key can't be given: {error}"));
            }
        }
        let exit = supervised
            .wait_deadline(self.timeout)
            .map_err(|error| format!("{} didn't finish: {error}", named()))?;
        // What it said is read here, once it's done, on this thread: a
        // broker's stale close runs before its keeper starts, while it must
        // have one thread.
        let said = if exit.code == 0 {
            String::new()
        } else {
            said_now(&stderr)
        };
        Ok((exit.code, said))
    }

    /// Whether `master`, a vault's key, opens the room disk: `None` when
    /// there's no image. Its key is derived here and goes to cryptsetup's
    /// test of it alone; nothing is mapped. Only cryptsetup's own answer
    /// that the key doesn't open it (status 2) is a no: anything else that
    /// fails is refused, so a missing program is never taken as a wrong key.
    fn opens_with(&self, master: &MasterKey) -> Result<Option<bool>, String> {
        if std::fs::symlink_metadata(&self.dir).is_err() {
            return Ok(None);
        }
        self.check_dir()?;
        if self.image_at(&self.image())?.is_none() {
            return Ok(None);
        }
        self.trusted_program(&self.cryptsetup)?;
        let image = self.image_id()?;
        let key = RoomRootKey::of(master).image_key(&image);
        self.opens(&key).map(Some)
    }

    /// Whether `key` opens the image, by cryptsetup's test of it: nothing
    /// is mapped. Only its own answer that the key doesn't (status 2) is a
    /// no; anything else that fails is refused.
    fn opens(&self, key: &RoomKey) -> Result<bool, String> {
        let mut args: Vec<OsString> = [
            "open",
            "--test-passphrase",
            "--type",
            "luks2",
            "--key-file",
            "-",
            "--keyfile-size",
            "64",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        args.push(self.image().into_os_string());
        match self.run_tool_exit(&self.cryptsetup, &args, Some(KeyFrom::Held(key)))? {
            (0, _) => Ok(true),
            (2, _) => Ok(false),
            (code, said) => Err(format!(
                "cryptsetup couldn't test the room disk's key, status {code}: {}",
                said.trim()
            )),
        }
    }

    /// Moves the image's key slot from the vault key `old`'s key to `new`'s,
    /// in place, open or not: the data and the disk's own key inside are
    /// untouched.
    ///
    /// It looks first at which keys open the image, so it picks up where a
    /// run that stopped left it: the new key is added, authorized by the
    /// old, and only once the new key is seen to open the image is the old
    /// one removed. So there's always a key that opens it. An image neither
    /// opens is left as it is: nothing could read it before either.
    fn rotate(&self, old: &MasterKey, new: &MasterKey) -> Result<Rotated, String> {
        if std::fs::symlink_metadata(&self.dir).is_err() {
            return Ok(Rotated::NoImage);
        }
        self.check_dir()?;
        if self.image_at(&self.image())?.is_none() {
            return Ok(Rotated::NoImage);
        }
        self.trusted_program(&self.cryptsetup)?;
        let image = self.image_id()?;
        let old = RoomRootKey::of(old).image_key(&image);
        let new = RoomRootKey::of(new).image_key(&image);
        // The same key twice: removing the "old" would remove the only one.
        if old.expose() == new.expose() {
            return Ok(Rotated::Done);
        }
        match (self.opens(&old)?, self.opens(&new)?) {
            (false, false) => return Ok(Rotated::NeitherOpens),
            (false, true) => return Ok(Rotated::Done),
            (true, false) => {
                self.add_key(&old, &new)?;
                if !self.opens(&new)? {
                    return Err(String::from(
                        "the new key was added to the room disk but doesn't open it, so the old key was kept",
                    ));
                }
            }
            (true, true) => {}
        }
        self.run_tool(
            &self.cryptsetup,
            &key_args("luksRemoveKey", &self.image()),
            Some(KeyFrom::Held(&old)),
        )?;
        if self.opens(&old)? {
            return Err(String::from(
                "the old key still opens the room disk after it was removed",
            ));
        }
        Ok(Rotated::Done)
    }

    /// Adds `new` to the image's key slots, authorized by `old`: `old` on
    /// cryptsetup's stdin, `new` on a pipe of its own, named `/dev/fd/N`.
    fn add_key(&self, old: &RoomKey, new: &RoomKey) -> Result<(), String> {
        let given = |error: hatchling_sys::Error| {
            format!("the room disk's new key can't be given: {error}")
        };
        let (read, write) = hatchling_sys::pipe().map_err(given)?;
        // The reading end crosses into cryptsetup, at its own number: the
        // one descriptor kept open across the exec. It's closed here once
        // cryptsetup has started. Nothing else is started meanwhile: a
        // rotation runs in an operator's command, on its one thread.
        hatchling_sys::keep_across_exec(read.as_fd()).map_err(given)?;
        let mut args = key_args("luksAddKey", &self.image());
        let at = args.len() - 1;
        for (offset, arg) in [
            "--pbkdf",
            "pbkdf2",
            "--pbkdf-force-iterations",
            "1000",
            "--new-keyfile-size",
            "64",
        ]
        .into_iter()
        .enumerate()
        {
            args.insert(at + offset, OsString::from(arg));
        }
        args.push(OsString::from(format!("/dev/fd/{}", read.as_raw_fd())));
        self.run_tool_giving(
            &self.cryptsetup,
            &args,
            Some(KeyFrom::Held(old)),
            Some(SecondKey {
                read,
                write,
                key: new,
            }),
        )
    }

    /// cryptsetup's version, as it says it: the program checked first.
    fn cryptsetup_version(&self) -> Result<String, String> {
        self.trusted_program(&self.cryptsetup)?;
        let said = Command::new(&self.cryptsetup)
            .env_clear()
            .env("PATH", TOOL_PATH)
            .env("LC_ALL", "C")
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|error| format!("cryptsetup can't start: {error}"))?;
        let text = String::from_utf8_lossy(&said.stdout);
        let version = text
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        if said.status.success() && !version.is_empty() {
            Ok(version)
        } else {
            Err(String::from("cryptsetup didn't say its version"))
        }
    }

    /// Makes the room disk: refused if one is there.
    fn create(&self, keeper: &Mutex<UnixStream>) -> Result<(), String> {
        self.trusted_program(&self.cryptsetup)?;
        self.trusted_program(&self.mke2fs)?;
        self.ensure_dir()?;
        if self.image_at(&self.image())?.is_some() {
            return Err(String::from(
                "a room disk is there already: wipe it before making another",
            ));
        }
        // What a create cut short left goes first.
        hatchling_sys::wipe_head_of(&self.new_image(), &self.trusted, HEADER_BYTES)
            .map_err(|error| format!("what an earlier create left can't be wiped: {error}"))?;
        let space = hatchling_sys::space(&self.dir).map_err(|error| error.to_string())?;
        let needed = self.size.saturating_add(FREE_MARGIN);
        if space.available < needed {
            return Err(format!(
                "a room disk of {} bytes needs {needed} free here, and {} is",
                self.size, space.available
            ));
        }
        hatchling_sys::make_sparse_file(&self.new_image(), self.size)
            .map_err(|error| format!("the room disk's image can't be made: {error}"))?;
        let image = new_image_id()?;
        let name = mapping_name(&image);
        if self.mapping_uuid(&name)?.is_some() {
            return Err(format!("a mapping named {name} is there already"));
        }
        let made = self.format(keeper, &image, &name);
        if made.is_err() {
            let _ = hatchling_sys::wipe_head_of(&self.new_image(), &self.trusted, HEADER_BYTES);
        }
        made
    }

    /// Formats the new image as `image`, mapped as `name` to make its file
    /// system, records it, and puts it in place.
    fn format(
        &self,
        keeper: &Mutex<UnixStream>,
        image: &RoomImageId,
        name: &str,
    ) -> Result<(), String> {
        let new = self.new_image();
        self.run_tool(
            &self.cryptsetup,
            &format_args(image, &new),
            Some(KeyFrom::Keeper(keeper, image, RoomKeyFor::Create)),
        )?;
        self.run_tool(
            &self.cryptsetup,
            &open_args(&new, name),
            Some(KeyFrom::Keeper(keeper, image, RoomKeyFor::Open)),
        )?;
        let made = self.run_tool(&self.mke2fs, &mkfs_args(name), None);
        let closed = self.close_mapping(name);
        made?;
        closed?;
        let record = serde_json::to_vec(&Record {
            format: 1,
            image: image.to_string(),
            size: self.size,
        })
        .map_err(|error| format!("the room disk's record can't be written: {error}"))?;
        hatchling_sys::replace_file(&self.record(), &record, None)
            .map_err(|error| format!("the room disk's record can't be written: {error}"))?;
        std::fs::hard_link(&new, self.image())
            .and_then(|()| std::fs::remove_file(&new))
            .map_err(|error| format!("the room disk can't be put in place: {error}"))
    }

    /// Opens the room disk and mounts it locked down. Never formats.
    fn open(&self, keeper: &Mutex<UnixStream>) -> Result<(), String> {
        self.trusted_program(&self.cryptsetup)?;
        self.check_dir()?;
        if self.image_at(&self.image())?.is_none() {
            return Err(String::from("there's no room disk: make one first"));
        }
        let image = self.image_id()?;
        self.check_record(&image)?;
        let name = mapping_name(&image);
        let mapped = self.mapped(&image, &name)?;
        let mount = self.mount_point();
        if self.mount_point_mounted()? {
            if mapped {
                return hatchling_sys::verify_locked_down(&mount)
                    .map_err(|error| error.to_string());
            }
            return Err(String::from(
                "something else is mounted at the room disk's mount point",
            ));
        }
        if !mapped {
            self.run_tool(
                &self.cryptsetup,
                &open_args(&self.image(), &name),
                Some(KeyFrom::Keeper(keeper, &image, RoomKeyFor::Open)),
            )?;
        }
        let device = Path::new(MAPPER).join(&name);
        if let Err(error) =
            hatchling_sys::mount_locked_down(&device, &mount, "ext4", "errors=remount-ro")
        {
            let _ = self.close_mapping(&name);
            return Err(format!("the room disk can't be mounted: {error}"));
        }
        Ok(())
    }

    /// Unmounts the room disk and closes its mapping. A disk in use is
    /// refused as busy, and stays as it is.
    fn close(&self) -> Result<(), String> {
        // No room directory, no room disk: nothing to close.
        if std::fs::symlink_metadata(&self.dir).is_err() {
            return Ok(());
        }
        self.check_dir()?;
        let mount = self.mount_point();
        if std::fs::symlink_metadata(&mount).is_ok()
            && hatchling_sys::is_mount_point(&mount).map_err(|error| error.to_string())?
        {
            hatchling_sys::unmount_strict(&mount).map_err(|error| unmount_refusal(&error))?;
        }
        if self.image_at(&self.image())?.is_none() {
            return Ok(());
        }
        let image = self.image_id()?;
        let name = mapping_name(&image);
        if self.mapped(&image, &name)? {
            self.trusted_program(&self.cryptsetup)?;
            self.close_mapping(&name)?;
        }
        Ok(())
    }

    /// Closes the mapping `name`, and confirms it's gone.
    fn close_mapping(&self, name: &str) -> Result<(), String> {
        self.run_tool(
            &self.cryptsetup,
            &[OsString::from("close"), OsString::from(name)],
            None,
        )?;
        if self.mapping_uuid(name)?.is_some() {
            return Err(format!("{name} is still mapped after it was closed"));
        }
        Ok(())
    }

    /// How the room disk is, in numbers.
    fn status(&self) -> Result<RoomDiskStatus, String> {
        let mut status = RoomDiskStatus {
            state: RoomDiskState::Absent,
            cap: self.size,
            image_bytes: None,
            on_disk: None,
            used: None,
            free: None,
            host_free: None,
        };
        let below = if self.dir.exists() {
            self.check_dir()?;
            self.dir.as_path()
        } else {
            self.dir.parent().unwrap_or(&self.dir)
        };
        status.host_free = hatchling_sys::space(below)
            .ok()
            .map(|space| space.available);
        let Some(meta) = self.image_at(&self.image())? else {
            return Ok(status);
        };
        status.state = RoomDiskState::Closed;
        status.image_bytes = Some(meta.len());
        status.on_disk = Some(meta.blocks().saturating_mul(512));
        let mount = self.mount_point();
        let image = self.image_id()?;
        let mounted = std::fs::symlink_metadata(&mount).is_ok()
            && hatchling_sys::is_mount_point(&mount).map_err(|error| error.to_string())?;
        // A mapping is looked for whether or not it's mounted: one a close
        // left is decrypted all the same, and is never said to be closed.
        if !self.mapped(&image, &mapping_name(&image))? {
            return Ok(status);
        }
        if !mounted {
            status.state = RoomDiskState::Mapped;
            return Ok(status);
        }
        let space = hatchling_sys::space(&mount).map_err(|error| error.to_string())?;
        status.state = RoomDiskState::Open;
        status.used = Some(space.size.saturating_sub(space.free));
        status.free = Some(space.available);
        Ok(status)
    }

    /// Closes the room disk and wipes it: its head, where its key slots are,
    /// overwritten, then the image and its record removed.
    fn wipe(&self) -> Result<(), String> {
        self.close()?;
        for image in [self.image(), self.new_image()] {
            hatchling_sys::wipe_head_of(&image, &self.trusted, HEADER_BYTES)
                .map_err(|error| format!("{} can't be wiped: {error}", image.display()))?;
        }
        hatchling_sys::remove_file_of(
            &self.record(),
            hatchling_sys::FileKind::Regular,
            &self.trusted,
        )
        .map(drop)
        .map_err(|error| format!("the room disk's record can't be removed: {error}"))
    }

    /// Why the record isn't `image`'s, if it isn't.
    fn check_record(&self, image: &RoomImageId) -> Result<(), String> {
        let text = std::fs::read(self.record())
            .map_err(|error| format!("the room disk's record can't be read: {error}"))?;
        let record: Record = serde_json::from_slice(&text)
            .map_err(|error| format!("the room disk's record doesn't read: {error}"))?;
        if record.image == image.as_str() {
            Ok(())
        } else {
            Err(String::from(
                "the room disk isn't the one its record says was made here",
            ))
        }
    }
}

/// Where a program's key comes from: the keeper, for an image and a
/// purpose, or a key this process derived itself (an operator's command,
/// which holds the vault's key).
enum KeyFrom<'a> {
    /// Asked of the keeper.
    Keeper(&'a Mutex<UnixStream>, &'a RoomImageId, RoomKeyFor),
    /// Held here.
    Held(&'a RoomKey),
}

/// A second key a program is given, beside the one on its stdin: the pipe
/// it's read from, whose reading end the program inherits.
struct SecondKey<'a> {
    /// The reading end, closed here once the program has started.
    read: OwnedFd,
    /// The writing end, the key written into it whole.
    write: OwnedFd,
    /// The key.
    key: &'a RoomKey,
}

/// What a change of the room disk's key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotated {
    /// There's no room disk: nothing to change.
    NoImage,
    /// The new vault key's key opens it, and the old one's doesn't.
    Done,
    /// Neither opens it, so nothing was changed: nothing could read it
    /// before either.
    NeitherOpens,
}

/// The arguments of `command`, a key-slot change of `image` authorized by
/// a key on stdin.
fn key_args(command: &str, image: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        command,
        "--batch-mode",
        "--key-file",
        "-",
        "--keyfile-size",
        "64",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(image.as_os_str().to_owned());
    args
}

/// The room disk's record, beside the image.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    /// The record's own format.
    format: u32,
    /// The image's LUKS UUID.
    image: String,
    /// The size it was made at.
    size: u64,
}

/// What a failed unmount is said as: one in use is busy, and stays.
fn unmount_refusal(error: &hatchling_sys::Error) -> String {
    let busy = matches!(
        error,
        hatchling_sys::Error::Mount { source, .. }
            if source.kind() == std::io::ErrorKind::ResourceBusy
    );
    if busy {
        String::from("the room disk is busy: something has a file open in it")
    } else {
        format!("the room disk can't be unmounted: {error}")
    }
}

/// Held while the room disk is changed or looked at: one op at a time,
/// whoever asks.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Does the room disk's `op`, one at a time however many connections ask:
/// a status, or nothing.
///
/// # Errors
///
/// Why not, in a sentence: a check that failed, or a program's failure
/// with what it said.
pub fn run(
    disk: &RoomDisk,
    keeper: &Mutex<UnixStream>,
    op: RoomDiskOp,
) -> Result<Option<RoomDiskStatus>, String> {
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let done = match op {
        RoomDiskOp::Create => disk.create(keeper).map(|()| None),
        RoomDiskOp::Open => disk.open(keeper).map(|()| None),
        RoomDiskOp::Close => disk.close().map(|()| None),
        RoomDiskOp::Status => disk.status().map(Some),
        RoomDiskOp::Wipe => disk.wipe().map(|()| None),
    };
    drop(turn);
    done
}

/// Everything the room directory must stay apart from.
///
/// Every store, the vault and its keeper's directories, the broker's own
/// and its state, and the agent socket's directory. A directory handed to
/// a service user, or cleared or backed up as Hatchling's state, must never
/// hold the room disk, nor sit inside it.
#[must_use]
pub fn kept_apart(
    config: &crate::Config,
    redaction: Option<&crate::RedactionConfig>,
    keyring: &crate::vault_keyring::KeyringConfig,
) -> Vec<PathBuf> {
    let mut apart: Vec<PathBuf> = crate::sealed_stores(config, redaction)
        .into_iter()
        .map(|(_, root)| root)
        .collect();
    apart.extend(crate::broker_user::broker_dirs(config, redaction));
    apart.extend(crate::broker_user::state_dirs(config));
    apart.extend(crate::broker_user::state_files(config));
    apart.extend(crate::sweep::state_dirs(config, redaction));
    apart.extend(crate::keeper::keeper_dirs(config, keyring));
    apart.extend(config.socket.as_path().parent().map(Path::to_path_buf));
    apart.retain(|path| !path.as_os_str().is_empty());
    apart
}

/// Why the room directory `room` isn't apart from each of `apart`, if it
/// isn't: neither may hold the other, as each resolves now, through any
/// link on the way.
///
/// # Errors
///
/// The first that overlaps, named.
pub fn check_apart(room: &Path, apart: &[PathBuf]) -> Result<(), String> {
    let room_is = resolve(room);
    for other in apart {
        let other_is = resolve(other);
        if room_is.starts_with(&other_is) || other_is.starts_with(&room_is) {
            return Err(format!(
                "the room disk's directory {} overlaps {}: it must be apart from every store and state directory",
                room.display(),
                other.display()
            ));
        }
    }
    Ok(())
}

/// `path` as it resolves now: through every link on the way to the
/// nearest part of it that's there, with the rest as it's named.
fn resolve(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(found) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(found, |path, part| path.join(part));
        }
        let (Some(above), Some(name)) = (at.parent(), at.file_name()) else {
            return path.to_path_buf();
        };
        rest.push(name.to_owned());
        at = above;
    }
}

/// Closes the room disk when the broker has gone: what the spawner does as
/// it leaves.
pub(crate) fn close_on_leaving(disk: &RoomDisk) {
    if let Err(reason) = close(disk) {
        tracing::error!(%reason, "the room disk wasn't closed as the broker went");
    }
}

/// The room disk the settings in this process's environment configure.
///
/// # Errors
///
/// As [`RoomDisk::parse`].
pub fn here() -> Result<RoomDisk, Error> {
    RoomDisk::parse(|key| std::env::var(key).ok())
}

/// Closes the room disk, one op at a time with the spawner's: unmounted, its
/// mapping closed and confirmed gone. Nothing open, nothing to do.
///
/// # Errors
///
/// Why not: a check that failed, or a disk in use (busy).
pub fn close(disk: &RoomDisk) -> Result<(), String> {
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let closed = disk.close();
    drop(turn);
    closed
}

/// Closes the room disk and wipes it, one op at a time with the spawner's.
///
/// # Errors
///
/// As [`close`], and why the image or its record can't be removed.
pub fn wipe(disk: &RoomDisk) -> Result<(), String> {
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let wiped = disk.wipe();
    drop(turn);
    wiped
}

/// Whether the vault key `master` opens the room disk: `None` when there's
/// no image.
///
/// For an operator's command, which holds the vault's key: the image's key
/// is derived here and goes to cryptsetup's test alone.
///
/// # Errors
///
/// A check that failed, or cryptsetup failing for anything but the key.
pub fn opens_with(disk: &RoomDisk, master: &MasterKey) -> Result<Option<bool>, String> {
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let opens = disk.opens_with(master);
    drop(turn);
    opens
}

/// Moves the room disk's key slot from the vault key `old` to `new`, one op
/// at a time with the spawner's. See [`RoomDisk`]'s `rotate`.
///
/// # Errors
///
/// A check that failed, or a step of cryptsetup's: there's still a key
/// that opens it, and running it again picks up from there.
pub fn rotate(disk: &RoomDisk, old: &MasterKey, new: &MasterKey) -> Result<Rotated, String> {
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let rotated = disk.rotate(old, new);
    drop(turn);
    rotated
}

/// The room disk's key slot moved with a change of the vault's key, from
/// `old` to `new`: what each change of it runs once every store is under
/// the new key, and before the change is finished. One that fails leaves
/// the change unfinished, so the vault isn't served until it's run again.
///
/// # Errors
///
/// [`Error::Usage`], saying why the room disk's key wasn't changed.
pub(crate) fn rotate_with_vault(
    disk: &RoomDisk,
    old: &MasterKey,
    new: &MasterKey,
) -> Result<(), Error> {
    match rotate(disk, old, new) {
        Ok(Rotated::NeitherOpens) => {
            tracing::warn!(
                "the room disk opens with neither the old nor the new vault key, so its key wasn't changed"
            );
            Ok(())
        }
        Ok(Rotated::NoImage | Rotated::Done) => Ok(()),
        Err(reason) => Err(Error::Usage {
            reason: format!(
                "the vault's key changed, but the agent's room disk's key didn't, so the change isn't finished: {reason}. Run the same command again to finish it"
            ),
        }),
    }
}

/// How the room disk is, one op at a time with the spawner's.
///
/// # Errors
///
/// A check that failed.
pub fn status(disk: &RoomDisk) -> Result<RoomDiskStatus, String> {
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let status = disk.status();
    drop(turn);
    status
}

/// cryptsetup's version, as the room disk would run it.
///
/// # Errors
///
/// Why it can't be run, or didn't say.
pub fn cryptsetup_version(disk: &RoomDisk) -> Result<String, String> {
    disk.cryptsetup_version()
}

/// `hatchling-broker room-disk close`: the room disk closed, now, as root,
/// with the service's settings alone. What stopping the service runs
/// (`ExecStopPost`). Nothing open is nothing to do.
#[must_use]
pub fn close_main(program: &str) -> std::process::ExitCode {
    use std::io::Write as _;
    let said = if hatchling_sys::euid() == 0 {
        here()
            .map_err(|error| error.to_string())
            .and_then(|disk| close(&disk))
    } else {
        Err(String::from(
            "run it with sudo: only root closes the room disk",
        ))
    };
    match said {
        Ok(()) => {
            let _ = writeln!(
                std::io::stdout().lock(),
                "{program} room-disk close: closed"
            );
            std::process::ExitCode::SUCCESS
        }
        Err(reason) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{program} room-disk close: {reason}"
            );
            std::process::ExitCode::FAILURE
        }
    }
}

/// What [`close_stale`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stale {
    /// Nothing: a broker that isn't root has no room disk.
    NotRoot,
    /// Closed, or there was nothing open.
    Closed,
    /// Left open, said in the log.
    LeftOpen,
}

/// Closes what an earlier broker left of `disk`, before the vault is
/// unlocked: a broker killed, or a stop whose close didn't run, leaves its
/// mapping and its mount, and a locked install must hold none. Only for a
/// broker that runs as root (`euid` 0): one that doesn't has no room disk.
pub(crate) fn close_stale(disk: Result<RoomDisk, Error>, euid: u32) -> Stale {
    if euid != 0 {
        return Stale::NotRoot;
    }
    let closed = disk
        .map_err(|error| error.to_string())
        .and_then(|disk| close(&disk));
    match closed {
        Ok(()) => Stale::Closed,
        Err(reason) => {
            tracing::error!(%reason, "the room disk left open by an earlier broker wasn't closed");
            Stale::LeftOpen
        }
    }
}

/// How the room disk came out of the broker's start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AtStart {
    /// Open: it was already, or opened now.
    Opened,
    /// Made now, and opened.
    Made,
    /// Off, and why: the room is off, and the agent talks.
    Off(String),
}

/// Opens the room disk at the broker's start, once the vault is unlocked:
/// made first if there's none. Anything that stops it turns the room off,
/// says why in the log, and leaves the rest of Hatchling as it is.
pub(crate) fn open_at_start(spawner: Option<&crate::spawner::Spawner>) -> AtStart {
    let Some(spawner) = spawner else {
        return off(String::from(NO_SPAWNER));
    };
    let state = match spawner.room_disk(RoomDiskOp::Status) {
        Ok(Some(status)) => status.state,
        Ok(None) => return off(String::from("the spawner didn't say how it is")),
        Err(reason) => return off(reason),
    };
    let made = state == RoomDiskState::Absent;
    if made {
        if let Err(reason) = spawner.room_disk(RoomDiskOp::Create) {
            return off(reason);
        }
    }
    if state != RoomDiskState::Open {
        if let Err(reason) = spawner.room_disk(RoomDiskOp::Open) {
            return off(reason);
        }
    }
    tracing::info!(made, "the room disk is open");
    if made { AtStart::Made } else { AtStart::Opened }
}

/// The room off, for `reason`, said in the log.
fn off(reason: String) -> AtStart {
    tracing::warn!(%reason, "the room disk is off: the room is off, and the agent talks");
    AtStart::Off(reason)
}

/// Why the room is off when no spawner keeps a room disk.
const NO_SPAWNER: &str = "no spawner keeps one";

/// The spawner's refusal of every room disk op where it keeps none: not
/// root, or no vault keeper.
pub(crate) const NOT_HERE: &str =
    "there's no room disk here: it needs a broker run as root, with a vault keeper";

/// How the room disk came out of this broker's start, once it's started.
static STARTED: OnceLock<AtStart> = OnceLock::new();

/// Keeps how the room disk came out of this broker's start, for the app to
/// be told. Once: a later one is ignored.
pub(crate) fn remember_start(started: AtStart) {
    let _ = STARTED.set(started);
}

/// Whether `why` says this install can have no room disk at all.
fn not_here(why: &str) -> bool {
    why == NO_SPAWNER || why == NOT_HERE
}

/// The room disk as the app is told it, from how it came out of the start
/// (`started`, none before one) and the spawner's `status` of it now:
/// numbers, and a fixed reason when it's off. Nothing a program said, and
/// no path, ever reaches the app: a reason is one of [`RoomDiskOff`]'s.
pub(crate) fn view(
    started: Option<&AtStart>,
    status: Result<Option<RoomDiskStatus>, String>,
) -> RoomDiskView {
    let off_at_start = match started {
        Some(AtStart::Off(why)) if not_here(why) => Some(RoomDiskOff::NotHere),
        Some(AtStart::Off(_)) => Some(RoomDiskOff::NotOpened),
        Some(AtStart::Opened | AtStart::Made) | None => None,
    };
    let off_now = |reason: RoomDiskOff| RoomDiskView {
        state: RoomDiskCondition::Off,
        off: Some(off_at_start.unwrap_or(reason)),
        cap: None,
        used: None,
        free: None,
        on_disk: None,
        host_free: None,
    };
    match status {
        Ok(Some(status)) => RoomDiskView {
            state: match (off_at_start, status.state) {
                (Some(_), _) => RoomDiskCondition::Off,
                (None, RoomDiskState::Absent) => RoomDiskCondition::Absent,
                (None, RoomDiskState::Closed) => RoomDiskCondition::Closed,
                (None, RoomDiskState::Mapped) => RoomDiskCondition::Mapped,
                (None, RoomDiskState::Open) => RoomDiskCondition::Open,
            },
            off: off_at_start,
            cap: Some(status.cap),
            used: status.used,
            free: status.free,
            on_disk: status.on_disk,
            host_free: status.host_free,
        },
        Err(why) if not_here(&why) => off_now(RoomDiskOff::NotHere),
        Ok(None) | Err(_) => off_now(RoomDiskOff::NotRead),
    }
}

/// The room disk as the app is told it now: [`view`], of this broker's
/// start and its spawner's status.
pub(crate) fn view_here() -> RoomDiskView {
    let status = crate::spawner::spawner().map_or_else(
        || Err(String::from(NO_SPAWNER)),
        |spawner| spawner.room_disk(RoomDiskOp::Status),
    );
    view(STARTED.get(), status)
}

/// Hands the keeper the ask for `image`'s key, for `purpose`, with `into`,
/// the writing end of the pipe cryptsetup reads it from: this end is closed
/// here once it's handed over.
fn ask_key(
    keeper: &Mutex<UnixStream>,
    image: &RoomImageId,
    purpose: RoomKeyFor,
    into: OwnedFd,
) -> Result<(), String> {
    let asked = serde_json::to_vec(&RoomKeyAsk {
        image: image.to_string(),
        room_key: purpose,
    })
    .map_err(|error| format!("the ask for the room disk's key can't be written: {error}"))?;
    let keeper = keeper
        .lock()
        .map_err(|_| String::from("an earlier handover to the keeper panicked"))?;
    write_frame(&keeper, &asked)
        .map_err(|error| format!("the keeper can't be asked for the key: {error}"))?;
    let handed = hatchling_sys::net::send_fd(keeper.as_fd(), into.as_fd());
    drop(keeper);
    drop(into);
    handed.map_err(|error| format!("the keeper can't be handed the key's pipe: {error}"))?;
    Ok(())
}

/// What a program said on its error output, `stderr`, once it's done:
/// what's there now, cut short, read without waiting. A program it started
/// that still holds the output open adds nothing, and isn't waited for. The
/// socket holds far more than a tool's error says, so a tool never waits to
/// say it.
fn said_now(stderr: &UnixStream) -> String {
    use std::io::Read as _;
    let _ = stderr.set_nonblocking(true);
    let mut bytes = Vec::new();
    // Read until there's no more now (`WouldBlock`) or none at all (its
    // end); what was read before either is kept.
    let _ = stderr.take(SAID_BYTES).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A size as `HATCHLING_ROOM_DISK_SIZE` gives it: bytes, or a whole number
/// with `M` or `G` (MiB, GiB).
fn parse_size(raw: &str) -> Result<u64, String> {
    let raw = raw.trim();
    let (digits, unit) = match raw.strip_suffix(['M', 'G']) {
        Some(digits) if raw.ends_with('M') => (digits, MIB),
        Some(digits) => (digits, GIB),
        None => (raw, 1),
    };
    let refused = || format!("not a size: {raw:?} (bytes, or a whole number with M or G)");
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(refused());
    }
    digits
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(unit))
        .ok_or_else(refused)
}

/// The image's UUID, from the LUKS2 header at `head`: its magic and version
/// first, then the UUID field, which must be a canonical one.
fn header_id(head: &[u8]) -> Result<RoomImageId, String> {
    const MAGIC: &[u8] = b"LUKS\xba\xbe\x00\x02";
    if head.get(..MAGIC.len()) != Some(MAGIC) {
        return Err(String::from("the room disk has no LUKS2 header"));
    }
    let field = head
        .get(168..208)
        .ok_or_else(|| String::from("the room disk's header is cut short"))?;
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    std::str::from_utf8(field.get(..end).unwrap_or_default())
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| String::from("the room disk's header has no UUID of ours"))
}

/// A fresh image's UUID: version 4, random.
fn new_image_id() -> Result<RoomImageId, String> {
    let hex = crate::approval::random_hex("")
        .map_err(|error| format!("no randomness for the room disk's UUID: {error}"))?;
    let mut digits: Vec<char> = hex.chars().take(32).collect();
    if let Some(version) = digits.get_mut(12) {
        *version = '4';
    }
    // RFC 4122's variant, of the four it allows the one that's always 8.
    if let Some(variant) = digits.get_mut(16) {
        *variant = '8';
    }
    let mut text = String::with_capacity(36);
    for (at, digit) in digits.into_iter().enumerate() {
        if matches!(at, 8 | 12 | 16 | 20) {
            text.push('-');
        }
        text.push(digit);
    }
    text.parse()
        .map_err(|error| format!("a room disk's UUID that isn't one: {error}"))
}

/// The mapping's name for `image`: its prefix and the UUID's first twelve
/// digits.
#[must_use]
pub fn mapping_name(image: &RoomImageId) -> String {
    let digits: String = image
        .as_str()
        .chars()
        .filter(|digit| *digit != '-')
        .take(12)
        .collect();
    format!("{MAPPING_PREFIX}{digits}")
}

/// The device-mapper UUID cryptsetup gives the LUKS2 mapping `name` of
/// `image`.
fn dm_uuid(image: &RoomImageId, name: &str) -> String {
    format!("CRYPT-LUKS2-{}-{name}", image.as_str().replace('-', ""))
}

/// What cryptsetup makes the image at `path` with, as `image`.
fn format_args(image: &RoomImageId, path: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "luksFormat",
        "--batch-mode",
        "--type",
        "luks2",
        "--cipher",
        "aes-xts-plain64",
        "--key-size",
        "512",
        "--hash",
        "sha256",
        "--pbkdf",
        "pbkdf2",
        "--pbkdf-force-iterations",
        "1000",
        "--offset",
        OFFSET_SECTORS,
        "--uuid",
        image.as_str(),
        "--label",
        LABEL,
        "--key-file",
        "-",
        "--keyfile-size",
        "64",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(path.as_os_str().to_owned());
    args
}

/// What cryptsetup opens the image at `path` with, mapped as `name`: no
/// discards.
fn open_args(path: &Path, name: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "open",
        "--type",
        "luks2",
        "--key-file",
        "-",
        "--keyfile-size",
        "64",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(path.as_os_str().to_owned());
    args.push(OsString::from(name));
    args
}

/// What mke2fs makes the file system with, on the mapping `name`: ext4,
/// no reserve, no discards, its root root's.
fn mkfs_args(name: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-t",
        "ext4",
        "-q",
        "-m",
        "0",
        "-E",
        "nodiscard,root_owner=0:0",
        "-L",
        LABEL,
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(Path::new(MAPPER).join(name).into_os_string());
    args
}

#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "crate-visible on purpose: the spawner's tests use the room disk's bench"
)]
pub(crate) mod tests;
