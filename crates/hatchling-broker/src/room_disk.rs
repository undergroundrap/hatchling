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
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use hatchling_core::framing::write_frame;
use hatchling_isolation::{IsolationBackend as _, Supervised as _};
use hatchling_vault::RoomImageId;
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
    /// Made, and closed.
    Closed,
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
        key: Option<(&Mutex<UnixStream>, &RoomImageId, RoomKeyFor)>,
    ) -> Result<(), String> {
        let named = || {
            let first = args.first().map(|arg| arg.to_string_lossy().into_owned());
            format!("{} {}", program.display(), first.unwrap_or_default())
        };
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", TOOL_PATH)
            .env("LC_ALL", "C")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
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
        // Its copy of the pipe's reading end goes with it: cryptsetup holds
        // the only one.
        drop(command);
        let said = child.stderr.take().map(read_said);
        let supervised = hatchling_isolation::platform()
            .supervise(&mut child)
            .map_err(|error| format!("{} can't be supervised: {error}", named()))?;
        drop(child);
        if let (Some(write), Some((keeper, image, purpose))) = (into, key) {
            if let Err(reason) = ask_key(keeper, image, purpose, write) {
                let _ = supervised.terminate();
                return Err(reason);
            }
        }
        let exit = supervised
            .wait_deadline(self.timeout)
            .map_err(|error| format!("{} didn't finish: {error}", named()))?;
        if exit.code == 0 {
            return Ok(());
        }
        let said = said
            .and_then(|said| said.recv_timeout(Duration::from_secs(2)).ok())
            .unwrap_or_default();
        Err(format!(
            "{} failed with status {}: {}",
            named(),
            exit.code,
            said.trim()
        ))
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
            Some((keeper, image, RoomKeyFor::Create)),
        )?;
        self.run_tool(
            &self.cryptsetup,
            &open_args(&new, name),
            Some((keeper, image, RoomKeyFor::Open)),
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
                Some((keeper, &image, RoomKeyFor::Open)),
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
        if mounted && self.mapped(&image, &mapping_name(&image))? {
            let space = hatchling_sys::space(&mount).map_err(|error| error.to_string())?;
            status.state = RoomDiskState::Open;
            status.used = Some(space.size.saturating_sub(space.free));
            status.free = Some(space.available);
        }
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
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let closed = disk.close();
    drop(turn);
    if let Err(reason) = closed {
        tracing::error!(%reason, "the room disk wasn't closed as the broker went");
    }
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

/// What a program says on its error output, cut short, once it's done.
fn read_said(stderr: std::process::ChildStderr) -> std::sync::mpsc::Receiver<String> {
    use std::io::Read as _;
    let (told, said) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(SAID_BYTES).read_to_end(&mut bytes);
        let _ = told.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    said
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
