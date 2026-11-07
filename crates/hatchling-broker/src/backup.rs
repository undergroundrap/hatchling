//! `backup`, `restore` and `reset`: Hatchling as something you can copy,
//! bring back, and start over.
//!
//! **A backup** is one sealed file ([`hatchling_vault::archive`]): the
//! agent's state, with the keyring of the day beside it. It opens with what
//! opened the vault that day. The key file isn't in it, so a backup and a
//! stolen disk image aren't one and the same.
//!
//! **Reset** has three levels, each with a backup first:
//!
//! - `conversation`: the conversation, and nothing else.
//! - `agent`: its mind as well: memory, goals, schedules and what it made.
//!   Your API keys, sign-ins, security keys and codes stay.
//! - `everything`: the lot, your keys in the vault included. Security keys
//!   aren't touched, as nothing's stored on them.
//!
//! Who can open the vault is a separate matter, and only `everything`
//! crosses it.
//!
//! All three are root's alone, at this machine: no agent reaches them. The
//! service is stopped for them, as a running broker would keep writing.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use hatchling_isolation::CellIdentity;
use hatchling_sys::{FileKind, Removed, remove_dir_tree, remove_dir_tree_of, remove_file_of};
use hatchling_vault::archive::{ArchiveReader, ArchiveWriter, Entry, archive_keyring, safe_path};
use hatchling_vault::{FileKey, KeySource as _, Keyring, MasterKey};

use crate::Error;
use crate::config::Config;
use crate::vault_keyring::{self as ring, Ask, Devices, KeyringConfig};

/// Where backups go unless told otherwise.
pub const BACKUPS: &str = "/var/lib/hatchling-backups";
/// How many of the backups kept there stay.
const KEEP: usize = 5;
/// What a backup's name starts with.
const PREFIX: &str = "hatchling-";
/// What a backup's name ends with.
const SUFFIX: &str = ".backup";
/// What marks when the last backup was made, by its time: wherever that
/// backup went. Root's, like the rest of [`BACKUPS`].
const LAST: &str = "last";

/// When the last backup was made, read while the broker was still root:
/// [`BACKUPS`] is root's alone, and making a backup restarts the broker.
static LAST_BACKUP: OnceLock<Option<u64>> = OnceLock::new();

/// How deep a reset goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The conversation alone.
    Conversation,
    /// The conversation and the agent's mind.
    Agent,
    /// All of it, the keys in the vault included.
    Everything,
}

impl Level {
    /// The level named `name`.
    fn named(name: &str) -> Option<Self> {
        match name {
            "conversation" => Some(Self::Conversation),
            "agent" => Some(Self::Agent),
            "everything" => Some(Self::Everything),
            _ => None,
        }
    }

    /// What it takes, in words.
    const fn takes(self) -> &'static str {
        match self {
            Self::Conversation => {
                "This clears the conversation. Memory, goals, schedules and your keys stay."
            }
            Self::Agent => {
                "This clears the conversation, memory, goals, schedules and what the agent made. Your API keys, sign-ins, security keys and backup codes stay."
            }
            Self::Everything => {
                "This erases everything: the agent's data, and your API keys and sign-ins in the vault. Security keys aren't changed, and work again once you register them."
            }
        }
    }

    /// What's typed to mean it.
    const fn confirm(self) -> &'static str {
        match self {
            Self::Conversation | Self::Agent => "reset",
            Self::Everything => "erase everything",
        }
    }
}

/// What a level a place belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// Cleared at every level.
    Conversation,
    /// Cleared from `agent`.
    Mind,
    /// Cleared only by `everything`.
    Keys,
}

impl Part {
    /// Whether `level` clears this.
    const fn cleared_by(self, level: Level) -> bool {
        matches!(
            (self, level),
            (Self::Conversation, _)
                | (Self::Mind, Level::Agent | Level::Everything)
                | (Self::Keys, Level::Everything)
        )
    }
}

/// One place state lives: a directory, or a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// Its name in a backup.
    name: String,
    /// Where it is.
    path: PathBuf,
    /// Which levels clear it.
    part: Part,
}

/// Every place `config` keeps state.
fn places(config: &Config) -> Result<Vec<Place>, Error> {
    let redaction = crate::vault_ops::redaction_config(config)?;
    let mut found: Vec<Place> = crate::sealed_stores(config, redaction.as_ref())
        .into_iter()
        .map(|(store, path)| Place {
            name: store.label().to_owned(),
            part: part_of(store.label()),
            path,
        })
        .collect();
    let state = crate::broker_user::state_dirs(config)
        .into_iter()
        .chain(crate::broker_user::state_files(config));
    for path in state {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        found.push(Place {
            name,
            path,
            part: Part::Mind,
        });
    }
    Ok(found)
}

/// Which level clears the sealed store `label`.
fn part_of(label: &str) -> Part {
    match label {
        "conversation" => Part::Conversation,
        "secrets" | "sites" | "profiles" | "credentials" => Part::Keys,
        _ => Part::Mind,
    }
}

/// `root`'s files, as `name/relative`, never through a link.
fn files_in(root: &Path, name: &str, found: &mut Vec<(String, PathBuf)>) -> Result<(), Error> {
    let Ok(kind) = fs::symlink_metadata(root) else {
        return Ok(());
    };
    if kind.is_file() {
        found.push((name.to_owned(), root.to_path_buf()));
    }
    if !kind.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(ring::io_error)? {
        let entry = entry.map_err(ring::io_error)?;
        let child = format!("{name}/{}", entry.file_name().to_string_lossy());
        files_in(&entry.path(), &child, found)?;
    }
    Ok(())
}

/// Seals `places` into a new file at `to`, with `keyring` beside them.
fn write_backup(
    places: &[Place],
    key: &MasterKey,
    keyring: &[u8],
    to: &Path,
) -> Result<usize, Error> {
    let partial = to.with_extension("partial");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .map_err(ring::io_error)?;
    let written = (|| {
        let mut writer = ArchiveWriter::new(file, key, keyring).map_err(ring::vault_error)?;
        let mut count = 0;
        for place in places {
            let mut files = Vec::new();
            files_in(&place.path, &place.name, &mut files)?;
            for (path, on_disk) in files {
                let mode = fs::symlink_metadata(&on_disk)
                    .map_err(ring::io_error)?
                    .permissions()
                    .mode()
                    & 0o777;
                let bytes = fs::read(&on_disk).map_err(ring::io_error)?;
                writer
                    .add(&Entry { path, mode, bytes })
                    .map_err(ring::vault_error)?;
                count += 1;
            }
        }
        writer
            .finish()
            .map_err(ring::vault_error)?
            .sync_all()
            .map_err(ring::io_error)?;
        Ok(count)
    })();
    match written {
        Ok(count) => {
            fs::rename(&partial, to).map_err(ring::io_error)?;
            Ok(count)
        }
        Err(error) => {
            drop_partial(&partial);
            Err(error)
        }
    }
}

/// What a backup that failed left at `partial`: the file it made, and
/// nothing that took its name since.
fn drop_partial(partial: &Path) {
    let _ = remove_file_of(partial, FileKind::Regular, &[hatchling_sys::euid()]);
}

/// The most of a cell's uid range a clear takes as Hatchling's. Past it,
/// what a cell left stops the clear and is named: the safe way to be wrong.
const MAX_CELL_UIDS: usize = 65_536;

/// Whose a reset or a restore clears under `config`: Hatchling's own users
/// ([`crate::broker_user::own_users`]) and its cells', whose run trees a
/// crash can leave behind.
fn owners(config: &Config) -> Vec<u32> {
    let get = |key: &str| crate::cell::env_value(std::env::var(key));
    with_cells(
        crate::broker_user::own_users(),
        config.agent_uid.as_u32(),
        crate::cell::agent_identity(&get).ok().map(|(cell, _)| cell),
    )
}

/// `own`, with the agent's uid and each of `cell`'s, if it can be told. One
/// that can't be is a setting the broker wouldn't start with, and adds
/// nothing.
fn with_cells(mut own: Vec<u32>, agent: u32, cell: Option<CellIdentity>) -> Vec<u32> {
    own.push(agent);
    if let Some(cell) = cell {
        let last = cell.principal.saturating_add(cell.count);
        own.extend((cell.principal..last).take(MAX_CELL_UIDS));
    }
    own
}

/// Clears `place`, of what `owners` own. A sealed store's dotfiles, its
/// wrapped key among them, stay unless `all`: a store with no key can't
/// take new secrets. Anything else in `place` stops it, and stays: root
/// clearing a store goes no further than its owner could.
fn clear_of(place: &Place, all: bool, owners: &[u32]) -> Result<(), Error> {
    let Ok(kind) = fs::symlink_metadata(&place.path) else {
        return Ok(());
    };
    if !kind.is_dir() {
        return remove_file_of(&place.path, FileKind::Regular, owners)
            .map(drop)
            .map_err(|error| left_at(&place.path, &error));
    }
    for entry in fs::read_dir(&place.path).map_err(ring::io_error)? {
        let entry = entry.map_err(ring::io_error)?;
        if !all && entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let inner = fs::symlink_metadata(&path).map_err(ring::io_error)?;
        if inner.is_dir() {
            clear_tree(&path, inner.uid(), owners)?;
        } else {
            remove_file_of(&path, FileKind::NotDirectory, owners)
                .map_err(|error| left_at(&path, &error))?;
        }
    }
    Ok(())
}

/// What stopped a clear at `path`, by name: the owner has to look at it.
fn left_at(path: &Path, why: &dyn std::fmt::Display) -> Error {
    Error::Usage {
        reason: format!("{} wasn't removed: {why}", path.display()),
    }
}

/// Removes the tree at `path`, `owner`'s when looked at, if that's one of
/// `owners`: all of it, or it's said what wasn't.
///
/// Not [`remove_dir_tree`], which keeps to this process's user: run as
/// root, as a reset is, every store here is a service user's.
fn clear_tree(path: &Path, owner: u32, owners: &[u32]) -> Result<(), Error> {
    if !owners.contains(&owner) {
        return Err(left_at(path, &"it isn't Hatchling's"));
    }
    match remove_dir_tree_of(path, owner).map_err(sys_error)? {
        Removed::Whole => Ok(()),
        Removed::Partly => Err(left_at(
            path,
            &"some of it isn't Hatchling's, or can't be removed",
        )),
    }
}

/// Clears what `level` clears of `places`, of what `owners` own.
fn clear_level(places: &[Place], level: Level, owners: &[u32]) -> Result<(), Error> {
    places
        .iter()
        .filter(|place| place.part.cleared_by(level))
        .try_for_each(|place| clear_of(place, level == Level::Everything, owners))
}

/// The opened backup's files, each under `staging`, ready to swap in:
/// nothing in place is touched until every one has been read and checked.
fn stage(
    reader: &mut ArchiveReader<fs::File>,
    places: &BTreeMap<&str, &Place>,
    staging: &Path,
) -> Result<usize, Error> {
    let mut count = 0;
    while let Some(entry) = reader.next_entry().map_err(ring::vault_error)? {
        let place = entry.path.split('/').next().unwrap_or_default();
        if !places.contains_key(place) || !safe_path(&entry.path) {
            return Err(Error::Usage {
                reason: format!(
                    "this backup has {place}, which this install doesn't keep: nothing was changed"
                ),
            });
        }
        let path = staging.join(&entry.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(ring::io_error)?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(entry.mode & 0o700)
            .open(&path)
            .map_err(ring::io_error)?;
        file.write_all(&entry.bytes).map_err(ring::io_error)?;
        count += 1;
    }
    Ok(count)
}

/// Puts `staging`'s files in `places`, clearing each first of what
/// `owners` own.
fn swap_in(places: &[Place], staging: &Path, owners: &[u32]) -> Result<(), Error> {
    for place in places {
        clear_of(place, true, owners)?;
        let staged = staging.join(&place.name);
        if !staged.exists() {
            continue;
        }
        if let Some(parent) = place.path.parent() {
            fs::create_dir_all(parent).map_err(ring::io_error)?;
        }
        if fs::symlink_metadata(&staged)
            .map_err(ring::io_error)?
            .is_dir()
        {
            fs::create_dir_all(&place.path).map_err(ring::io_error)?;
            for entry in fs::read_dir(&staged).map_err(ring::io_error)? {
                let entry = entry.map_err(ring::io_error)?;
                fs::rename(entry.path(), place.path.join(entry.file_name()))
                    .map_err(ring::io_error)?;
            }
        } else {
            fs::rename(&staged, &place.path).map_err(ring::io_error)?;
        }
    }
    Ok(())
}

/// The backups in `dir` past the newest [`KEEP`] go; nothing else there
/// is touched.
fn prune(dir: &Path) -> Result<(), Error> {
    let mut kept: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(ring::io_error)?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(PREFIX) && name.ends_with(SUFFIX)
        })
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .collect();
    // Their names start with the time, so they sort by it.
    kept.sort();
    let extra = kept.len().saturating_sub(KEEP);
    for old in kept.iter().take(extra) {
        drop_backup(old)?;
    }
    Ok(())
}

/// The backup at `old` goes, if it's still a file of this user's.
fn drop_backup(old: &Path) -> Result<(), Error> {
    remove_file_of(old, FileKind::Regular, &[hatchling_sys::euid()])
        .map(drop)
        .map_err(sys_error)
}

/// A backup's file name for `seconds` after the epoch.
fn backup_name(seconds: u64) -> String {
    format!("{PREFIX}{seconds:012}{SUFFIX}")
}

/// Stops or starts the service, if there is one.
fn service(action: &str) -> Result<(), Error> {
    if !Path::new(crate::doctor::SERVICE_SETTINGS).exists() {
        return Ok(());
    }
    // Quick stops and starts count against systemd's limit, and a limit
    // reached would leave the service down over nothing.
    let _ = Command::new("systemctl")
        .args(["reset-failed", "hatchling"])
        .status();
    let done = Command::new("systemctl")
        .args([action, "hatchling"])
        .status()
        .map_err(ring::io_error)?;
    if done.success() {
        Ok(())
    } else {
        Err(Error::Usage {
            reason: format!("couldn't {action} the service: sudo systemctl {action} hatchling"),
        })
    }
}

fn must_be_root() -> Result<(), Error> {
    if hatchling_sys::euid() == 0 {
        Ok(())
    } else {
        Err(Error::Usage {
            reason: String::from("run it with sudo: only root may do this"),
        })
    }
}

/// The vault's key and keyring, opened as the tier says.
fn opened(
    config: &Config,
    keyring: &KeyringConfig,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<(MasterKey, Vec<u8>), Error> {
    let Some(path) = keyring.keyring.as_deref() else {
        let key = FileKey::new(config.vault.key_file.clone())
            .load()
            .map_err(ring::vault_error)?;
        return Ok((key, Vec::new()));
    };
    let mut loaded = Keyring::load(path).map_err(ring::vault_error)?;
    let unlocked = ring::unlock_current(&mut loaded, &config.vault, keyring, ask, devices)?;
    let bytes = fs::read(path).map_err(ring::io_error)?;
    Ok((unlocked.into_key(), bytes))
}

/// A new backup: at `to`, or among the ones Hatchling keeps.
fn make_backup(
    config: &Config,
    (key, ring_bytes): &(MasterKey, Vec<u8>),
    to: Option<&Path>,
) -> Result<PathBuf, Error> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let path = to.map_or_else(
        || Path::new(BACKUPS).join(backup_name(seconds)),
        Path::to_path_buf,
    );
    if to.is_none() {
        private_dir(Path::new(BACKUPS))?;
    }
    write_backup(&places(config)?, key, ring_bytes, &path)?;
    if to.is_none() {
        prune(Path::new(BACKUPS))?;
    }
    if let Err(error) = mark_backup(Path::new(BACKUPS)) {
        // The backup is made: the app's checklist just won't know of it.
        tracing::warn!(error = %error, "the last backup's time couldn't be kept");
    }
    Ok(path)
}

/// Makes `dir`, and what's above it, and makes `dir` root's alone. Done on
/// the directory itself, never through a link: root's own mode change
/// would otherwise land on the link's target.
fn private_dir(dir: &Path) -> Result<(), Error> {
    if let Some(above) = dir.parent() {
        fs::create_dir_all(above).map_err(ring::io_error)?;
    }
    hatchling_sys::ensure_private_dir(dir).map_err(sys_error)
}

fn sys_error(source: hatchling_sys::Error) -> Error {
    ring::io_error(std::io::Error::other(source))
}

/// Marks a backup made now, in `dir`.
fn mark_backup(dir: &Path) -> Result<(), Error> {
    private_dir(dir)?;
    let marked = dir.join(LAST);
    remove_file_of(&marked, FileKind::Regular, &[hatchling_sys::euid()]).map_err(sys_error)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(marked)
        .map_err(ring::io_error)?;
    Ok(())
}

/// When a backup was last marked in `dir`, in Unix seconds.
fn backup_marked(dir: &Path) -> Option<u64> {
    let made = fs::metadata(dir.join(LAST)).ok()?.modified().ok()?;
    made.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|since| since.as_secs())
}

/// Reads when the last backup was made, from `dir`. For the broker to call
/// while it's root, at its start.
pub fn note_last_backup(dir: &Path) {
    let _ = LAST_BACKUP.set(backup_marked(dir));
}

/// When the last backup was made, as of this start of the broker.
#[must_use]
pub fn last_backup() -> Option<u64> {
    LAST_BACKUP.get().copied().flatten()
}

/// What to do once a restart has shut the app out: a vault that needs a
/// person is locked again, and every start makes the app a new address.
const fn after_restart(locked: bool) -> &'static str {
    if locked {
        "Hatchling restarted, and it's locked. Unlock it, and it says how to open the app: sudo hatchling-broker unlock"
    } else {
        "Hatchling restarted, so the app needs its new address. As yourself: hatchling-host url"
    }
}

/// Whether the vault with the keyring `keyring` configures needs a person
/// to open it.
fn needs_a_person(keyring: &KeyringConfig) -> bool {
    keyring
        .keyring
        .as_deref()
        .and_then(|path| Keyring::load(path).ok())
        .is_some_and(|ring| locks_at(ring.tier()))
}

/// `backup [file]`.
///
/// # Errors
///
/// Why not; the service is started again whatever happened.
pub fn run_backup(config: &Config, to: Option<&Path>) -> Result<(), Error> {
    must_be_root()?;
    let keyring = KeyringConfig::from_env()?;
    // The key before the service: stopped at the prompt, nothing's down.
    let (mut ask, mut devices) = ring::terminal_operator();
    let key = opened(config, &keyring, &mut ask, &mut devices)?;
    service("stop")?;
    let made = make_backup(config, &key, to);
    let started = service("start");
    let made = made?;
    started?;
    let _ = writeln!(
        std::io::stdout().lock(),
        "Backed up to {}.\nIt opens with what opens your vault today. To bring it back: sudo hatchling-broker restore <file>\n{}",
        made.display(),
        crate::windows_setup::here(after_restart(needs_a_person(&keyring)))
    );
    Ok(())
}

/// `restore <file>`.
///
/// # Errors
///
/// Why not; nothing in place changes until the whole backup has been read.
pub fn run_restore(config: &Config, from: &Path) -> Result<(), Error> {
    must_be_root()?;
    let keyring_config = KeyringConfig::from_env()?;
    let mut file = fs::File::open(from).map_err(ring::io_error)?;
    let ring_bytes = archive_keyring(&mut file).map_err(ring::vault_error)?;
    let places = places(config)?;
    let by_name: BTreeMap<&str, &Place> = places
        .iter()
        .map(|place| (place.name.as_str(), place))
        .collect();
    let (mut ask, mut devices) = ring::terminal_operator();
    let (key, staged_ring) =
        open_for_restore(config, &keyring_config, &ring_bytes, &mut ask, &mut devices)?;
    service("stop")?;
    let restored = (|| {
        let staging = config.vault.root.with_extension("restoring");
        fresh_staging(&staging)?;
        let mut reader = ArchiveReader::new(file, &key, &ring_bytes).map_err(ring::vault_error)?;
        let count = stage(&mut reader, &by_name, &staging)?;
        swap_in(&places, &staging, &owners(config))?;
        if let (Some(path), false) = (keyring_config.keyring.as_deref(), ring_bytes.is_empty()) {
            fs::rename(&staged_ring, path).map_err(ring::io_error)?;
        }
        drop_staging(&staging);
        Ok::<usize, Error>(count)
    })();
    let started = service("start");
    let count = restored?;
    started?;
    let _ = writeln!(
        std::io::stdout().lock(),
        "Restored {count} files from {}.\n{}",
        from.display(),
        crate::windows_setup::here(after_restart(needs_a_person(&keyring_config)))
    );
    Ok(())
}

/// A new directory at `staging`, this user's alone. What an earlier restore
/// left there goes. Anything else stands, and stops the restore: a link
/// isn't followed, and the directory is only ever a new one.
fn fresh_staging(staging: &Path) -> Result<(), Error> {
    drop_staging(staging);
    if let Some(above) = staging.parent() {
        fs::create_dir_all(above).map_err(ring::io_error)?;
    }
    hatchling_sys::create_private_dir(staging).map_err(sys_error)
}

/// What a restore staged goes, if it's still this user's tree.
fn drop_staging(staging: &Path) {
    let _ = remove_dir_tree(staging);
}

/// The key that opens a backup with keyring `ring_bytes`: the keyring is
/// put beside the real one to be opened as one, as a backup from another
/// machine brings its own. Its path comes back, for the swap.
fn open_for_restore(
    config: &Config,
    keyring: &KeyringConfig,
    ring_bytes: &[u8],
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<(MasterKey, PathBuf), Error> {
    let Some(path) = keyring.keyring.as_deref() else {
        let key = FileKey::new(config.vault.key_file.clone())
            .load()
            .map_err(ring::vault_error)?;
        return Ok((key, PathBuf::new()));
    };
    if ring_bytes.is_empty() {
        return Err(Error::Usage {
            reason: String::from(
                "this backup was made without a keyring, and this install has one: nothing was changed",
            ),
        });
    }
    let staged = path.with_extension("restoring");
    // As root, in a directory the keeper's user owns: a name it can plant.
    hatchling_sys::replace_file(
        &staged,
        ring_bytes,
        crate::keeper::keeper_user().ok().flatten(),
    )
    .map_err(sys_error)?;
    let mut loaded = Keyring::load(&staged).map_err(ring::vault_error)?;
    let unlocked = ring::unlock_current(&mut loaded, &config.vault, keyring, ask, devices)?;
    Ok((unlocked.into_key(), staged))
}

/// `reset <level> [--yes] [--no-backup]`.
///
/// # Errors
///
/// Why not. With no backup, nothing is cleared, unless told not to take
/// one: losing every way into the vault is the reason to.
pub fn run_reset(config: &Config, args: &[String]) -> Result<(), Error> {
    must_be_root()?;
    let usage = || Error::Usage {
        reason: String::from(USAGE),
    };
    let (level, flags) = args.split_first().ok_or_else(usage)?;
    let level = Level::named(level).ok_or_else(usage)?;
    if flags
        .iter()
        .any(|flag| !matches!(flag.as_str(), "--yes" | "--no-backup"))
    {
        return Err(usage());
    }
    let has = |flag: &str| flags.iter().any(|given| given == flag);
    let keyring = KeyringConfig::from_env()?;
    let (mut ask, mut devices) = ring::terminal_operator();
    if !has("--yes") {
        let word = level.confirm();
        let answer = ask(&format!("{} Type {word} to go on:", level.takes()))?;
        if answer.trim() != word {
            return Err(ring::stopped());
        }
    }
    let key = if has("--no-backup") {
        None
    } else {
        Some(opened(config, &keyring, &mut ask, &mut devices)?)
    };
    service("stop")?;
    let done = (|| {
        if let Some(key) = &key {
            let made = make_backup(config, key, None)?;
            let _ = writeln!(
                std::io::stdout().lock(),
                "Backed up first, to {}.",
                made.display()
            );
        }
        clear_level(&places(config)?, level, &owners(config))?;
        if level == Level::Everything {
            erase_keys(config, &keyring)?;
        }
        Ok::<(), Error>(())
    })();
    let started = service("start");
    done?;
    started?;
    let _ = writeln!(
        std::io::stdout().lock(),
        "Done. {}",
        crate::windows_setup::here(match level {
            Level::Everything => "Your next step is the same as a new install: docs/install.md",
            Level::Conversation | Level::Agent => after_restart(needs_a_person(&keyring)),
        })
    );
    Ok(())
}

/// What opens the vault goes too, for a start from nothing.
fn erase_keys(config: &Config, keyring: &KeyringConfig) -> Result<(), Error> {
    if let Some(path) = keyring.keyring.as_deref() {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(crate::key_setup::codes_marker(path));
        crate::key_seen::erase(path);
    }
    let _ = fs::remove_file(keyring.kek_file(&config.vault));
    let _ = fs::remove_file(&config.vault.key_file);
    // A new key, for stores with nothing in them to seal.
    drop(
        FileKey::new(config.vault.key_file.clone())
            .load_or_create()
            .map_err(ring::vault_error)?,
    );
    if keyring.keyring.is_some() {
        ring::init(&config.vault, keyring)?;
    }
    Ok(())
}

/// Whether a restart leaves a vault at `tier` locked: only one that needs
/// its owner does. One opened by its key file comes straight back.
const fn locks_at(tier: hatchling_vault::Tier) -> bool {
    !matches!(tier, hatchling_vault::Tier::File)
}

/// `lock`: the vault's key out of memory, now. For a machine left somewhere
/// it shouldn't be: a restart forgets the key, and it waits for its owner.
///
/// # Errors
///
/// Why not. A vault its key file opens is refused, not pretended locked.
pub fn run_lock() -> Result<(), Error> {
    must_be_root()?;
    let keyring = KeyringConfig::from_env()?;
    let tier = keyring
        .keyring
        .as_deref()
        .map(Keyring::load)
        .transpose()
        .map_err(ring::vault_error)?
        .map(|keyring| keyring.tier());
    if !tier.is_some_and(locks_at) {
        return Err(Error::Usage {
            reason: String::from(
                "this vault opens with its key file, so it would come straight back: a security key is what makes it lockable (docs/security-keys.md)",
            ),
        });
    }
    service("restart")?;
    let _ = writeln!(
        std::io::stdout().lock(),
        "{}",
        crate::windows_setup::here(
            "Locked. The agent and its schedules are stopped until you unlock it: sudo hatchling-broker unlock"
        )
    );
    Ok(())
}

/// How `reset` is used.
pub const USAGE: &str =
    "usage: hatchling-broker reset <conversation | agent | everything> [--yes] [--no-backup]";

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    /// Clears `place` as this process's user.
    fn clear(place: &Place, all: bool) -> Result<(), Error> {
        clear_of(place, all, &[hatchling_sys::euid()])
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("hatchling-backup-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn place(&self, name: &str, part: Part) -> Place {
            Place {
                name: name.to_owned(),
                path: self.0.join(name),
                part,
            }
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }

        fn read(&self, relative: &str) -> Option<Vec<u8>> {
            fs::read(self.0.join(relative)).ok()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn three(scratch: &Scratch) -> Vec<Place> {
        scratch.write("conversation/chat", b"hello");
        scratch.write("conversation/.key", b"wrapped");
        scratch.write("memory/deep/note", b"remember");
        scratch.write("secrets/token", b"sealed");
        vec![
            scratch.place("conversation", Part::Conversation),
            scratch.place("memory", Part::Mind),
            scratch.place("secrets", Part::Keys),
        ]
    }

    /// Each level clears what it says, and no more; a store's key stays
    /// short of everything.
    #[test]
    fn each_level_clears_what_it_says() {
        let scratch = Scratch::new("levels");
        let places = three(&scratch);
        clear_level(&places, Level::Conversation, &[hatchling_sys::euid()]).unwrap();
        assert_eq!(scratch.read("conversation/chat"), None);
        assert_eq!(scratch.read("conversation/.key").unwrap(), b"wrapped");
        assert_eq!(scratch.read("memory/deep/note").unwrap(), b"remember");
        let scratch = Scratch::new("levels-agent");
        let places = three(&scratch);
        clear_level(&places, Level::Agent, &[hatchling_sys::euid()]).unwrap();
        assert_eq!(scratch.read("memory/deep/note"), None);
        assert_eq!(scratch.read("secrets/token").unwrap(), b"sealed");
        let scratch = Scratch::new("levels-all");
        let places = three(&scratch);
        clear_level(&places, Level::Everything, &[hatchling_sys::euid()]).unwrap();
        assert_eq!(scratch.read("secrets/token"), None);
        assert_eq!(scratch.read("conversation/.key"), None);
    }

    #[test]
    fn a_backup_is_marked_with_when_and_only_root_reads_that() {
        let scratch = Scratch::new("backup-marked");
        let dir = scratch.0.join("backups");
        assert_eq!(backup_marked(&dir), None, "none yet");
        mark_backup(&dir).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(LAST)), 0o600);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let marked = backup_marked(&dir).unwrap();
        assert!(marked.abs_diff(now) < 60, "{marked} against {now}");
        // Marked again, it's the new one's time, and a prune leaves it be.
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000);
        fs::File::options()
            .write(true)
            .open(dir.join(LAST))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(backup_marked(&dir), Some(1000));
        prune(&dir).unwrap();
        assert_eq!(backup_marked(&dir), Some(1000));
        mark_backup(&dir).unwrap();
        assert!(backup_marked(&dir).unwrap().abs_diff(now) < 60);
        note_last_backup(&dir);
        assert_eq!(last_backup(), backup_marked(&dir));
    }

    /// A link planted where the backups' directory goes isn't followed:
    /// what it points at keeps its mode, and nothing is marked in it.
    #[test]
    fn a_backups_directory_is_never_reached_through_a_link() {
        let scratch = Scratch::new("backup-link");
        let target = scratch.0.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = scratch.0.join("backups");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(mark_backup(&link).is_err());
        let meta = fs::metadata(&target).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
        assert!(fs::read_dir(&target).unwrap().next().is_none());
    }

    /// A restore stages a backup's keyring beside the real one, as root, in
    /// a directory the keeper's user owns. A link planted there, or a stale
    /// file, is replaced; what the link pointed at isn't written, nor
    /// changed in mode, and the keyring there is what's staged.
    #[test]
    fn a_restores_keyring_never_goes_through_a_planted_link() {
        let scratch = Scratch::new("restore-link");
        let dir = scratch.0.join("ring");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = crate::api_auth::tests::owner_config(None);
        config.vault.root = scratch.0.join("vault");
        config.vault.key_file = dir.join("vault.key");
        drop(
            FileKey::new(config.vault.key_file.clone())
                .load_or_create()
                .unwrap(),
        );
        let keyring = KeyringConfig {
            keyring: Some(dir.join("keyring")),
            kek_file: Some(dir.join("kek")),
            ..KeyringConfig::default()
        };
        ring::init(&config.vault, &keyring).unwrap();
        let ring_bytes = fs::read(dir.join("keyring")).unwrap();
        let target = scratch.0.join("target");
        fs::write(&target, b"untouched").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let staged = dir.join("keyring.restoring");
        let mut ask = |_: &str| Ok(zeroize::Zeroizing::default());
        let open = |ask: Ask<'_>| {
            open_for_restore(&config, &keyring, &ring_bytes, ask, &mut ring::NoKeys).unwrap()
        };

        std::os::unix::fs::symlink(&target, &staged).unwrap();
        let (_, path) = open(&mut ask);
        assert_eq!(path, staged);
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the link's target changed mode");
        let file = fs::symlink_metadata(&staged).unwrap();
        assert!(file.is_file(), "the link is still there");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read(&staged).unwrap(), ring_bytes);

        // A stale file of an earlier restore goes the same way.
        fs::write(&staged, b"stale").unwrap();
        open(&mut ask);
        assert_eq!(fs::read(&staged).unwrap(), ring_bytes);
    }

    #[test]
    fn a_restart_says_what_comes_next() {
        assert!(after_restart(true).contains("sudo hatchling-broker unlock"));
        assert!(after_restart(false).contains("hatchling-host url"));
        assert!(!needs_a_person(&KeyringConfig::default()));
    }

    #[test]
    fn only_a_vault_that_needs_its_owner_can_lock() {
        use hatchling_vault::Tier;
        assert!(locks_at(Tier::HardwareKey));
        assert!(locks_at(Tier::Totp));
        assert!(!locks_at(Tier::File));
    }

    #[test]
    fn levels_and_parts_say_which_is_which() {
        for (name, level) in [
            ("conversation", Level::Conversation),
            ("agent", Level::Agent),
            ("everything", Level::Everything),
        ] {
            assert_eq!(Level::named(name), Some(level));
        }
        assert_eq!(Level::named("all"), None);
        assert_eq!(Level::Everything.confirm(), "erase everything");
        assert_eq!(Level::Agent.confirm(), "reset");
        assert!(Level::Agent.takes().contains("stay"));
        for label in ["secrets", "sites", "profiles", "credentials"] {
            assert_eq!(part_of(label), Part::Keys, "{label}");
        }
        assert_eq!(part_of("conversation"), Part::Conversation);
        assert_eq!(part_of("artifacts"), Part::Mind);
    }

    /// A backup brought back over changes gives the state it was taken of,
    /// and a file added since goes.
    #[test]
    fn a_backup_brings_back_what_it_took() {
        let scratch = Scratch::new("round");
        let places = three(&scratch);
        let key = MasterKey::generate().unwrap();
        let file = scratch.0.join("hatchling.backup");
        assert_eq!(write_backup(&places, &key, b"ring", &file).unwrap(), 4);
        assert!(!scratch.0.join("hatchling.partial").exists());
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        clear_level(&places, Level::Agent, &[hatchling_sys::euid()]).unwrap();
        scratch.write("memory/newer", b"added since");

        let mut opened = fs::File::open(&file).unwrap();
        assert_eq!(archive_keyring(&mut opened).unwrap(), b"ring");
        let mut reader = ArchiveReader::new(opened, &key, b"ring").unwrap();
        let by_name: BTreeMap<&str, &Place> = places
            .iter()
            .map(|place| (place.name.as_str(), place))
            .collect();
        let staging = scratch.0.join("staging");
        fresh_staging(&staging).unwrap();
        assert_eq!(stage(&mut reader, &by_name, &staging).unwrap(), 4);
        swap_in(&places, &staging, &[hatchling_sys::euid()]).unwrap();
        assert_eq!(scratch.read("conversation/chat").unwrap(), b"hello");
        assert_eq!(scratch.read("conversation/.key").unwrap(), b"wrapped");
        assert_eq!(scratch.read("memory/deep/note").unwrap(), b"remember");
        assert_eq!(scratch.read("memory/newer"), None);
    }

    /// A backup of something this install doesn't keep changes nothing.
    #[test]
    fn a_backup_from_elsewhere_changes_nothing() {
        let scratch = Scratch::new("elsewhere");
        let places = three(&scratch);
        let key = MasterKey::generate().unwrap();
        let file = scratch.0.join("h.backup");
        write_backup(&places, &key, b"", &file).unwrap();
        let mut opened = fs::File::open(&file).unwrap();
        archive_keyring(&mut opened).unwrap();
        let mut reader = ArchiveReader::new(opened, &key, b"").unwrap();
        let only = places.first().unwrap();
        let by_name: BTreeMap<&str, &Place> = BTreeMap::from([("other", only)]);
        let staging = scratch.0.join("staging");
        let refused = stage(&mut reader, &by_name, &staging).unwrap_err();
        assert!(refused.to_string().contains("doesn't keep"), "{refused}");
        assert_eq!(scratch.read("memory/deep/note").unwrap(), b"remember");
    }

    /// Links aren't followed in, and a file that's a place is taken.
    #[test]
    fn links_are_left_out() {
        let scratch = Scratch::new("links");
        scratch.write("outside/secret", b"not ours");
        scratch.write("memory/note", b"ours");
        scratch.write("activity", b"log");
        std::os::unix::fs::symlink(scratch.0.join("outside"), scratch.0.join("memory/link"))
            .unwrap();
        let mut found = Vec::new();
        files_in(&scratch.0.join("memory"), "memory", &mut found).unwrap();
        files_in(&scratch.0.join("activity"), "activity", &mut found).unwrap();
        files_in(&scratch.0.join("absent"), "absent", &mut found).unwrap();
        let names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["memory/note", "activity"]);
    }

    /// Only the oldest backups of Hatchling's own go.
    #[test]
    fn the_newest_backups_stay() {
        let scratch = Scratch::new("prune");
        for seconds in 1..=7 {
            scratch.write(&backup_name(seconds), b"x");
        }
        scratch.write("my-notes.txt", b"mine");
        prune(&scratch.0).unwrap();
        assert_eq!(scratch.read(&backup_name(1)), None);
        assert_eq!(scratch.read(&backup_name(2)), None);
        assert!(scratch.read(&backup_name(3)).is_some());
        assert!(scratch.read("my-notes.txt").is_some());
        assert_eq!(backup_name(9).len(), backup_name(10_000).len());
    }

    /// Named as the oldest backups, a link and a directory aren't backups:
    /// neither counts, neither goes, and what the link points at stays.
    #[test]
    fn only_files_are_pruned() {
        let scratch = Scratch::new("prune-kind");
        let backups = scratch.0.join("backups");
        fs::create_dir(&backups).unwrap();
        let target = scratch.0.join("mine");
        fs::write(&target, b"mine").unwrap();
        let link = backups.join(backup_name(1));
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let sub = backups.join(backup_name(2));
        fs::create_dir(&sub).unwrap();
        let file = |seconds| backups.join(backup_name(seconds));
        for seconds in 3..=8 {
            fs::write(file(seconds), b"x").unwrap();
        }
        prune(&backups).unwrap();
        assert!(fs::symlink_metadata(&link).is_ok(), "the link went");
        assert!(sub.is_dir(), "the directory went");
        assert_eq!(fs::read(&target).unwrap(), b"mine");
        assert!(!file(3).exists(), "six files, so the oldest of them goes");
        assert!(file(4).exists());
        // Past the listing, the delete itself looks again.
        assert!(drop_backup(&link).is_err());
        assert!(fs::symlink_metadata(&link).is_ok(), "the link went");
        assert!(drop_backup(&sub).is_err());
        assert!(sub.is_dir(), "the directory went");
        drop_backup(&file(4)).unwrap();
        assert!(!file(4).exists());
    }

    /// A place that's a link isn't cleared, and neither is what it points
    /// at. One that's a file is, and a link in a place goes itself.
    #[test]
    fn clearing_follows_no_link() {
        let scratch = Scratch::new("clear-link");
        scratch.write("outside/note", b"not ours");
        let outside = scratch.0.join("outside");
        std::os::unix::fs::symlink(&outside, scratch.0.join("linked")).unwrap();
        let place = scratch.place("linked", Part::Mind);
        assert!(clear(&place, true).is_err());
        assert!(fs::symlink_metadata(&place.path).is_ok(), "the link went");
        assert_eq!(scratch.read("outside/note").unwrap(), b"not ours");

        scratch.write("activity", b"log");
        clear(&scratch.place("activity", Part::Mind), true).unwrap();
        assert_eq!(scratch.read("activity"), None);

        scratch.write("memory/note", b"ours");
        std::os::unix::fs::symlink(&outside, scratch.0.join("memory/link")).unwrap();
        clear(&scratch.place("memory", Part::Mind), true).unwrap();
        assert!(fs::symlink_metadata(scratch.0.join("memory/link")).is_err());
        assert_eq!(scratch.read("memory/note"), None);
        assert_eq!(scratch.read("outside/note").unwrap(), b"not ours");
    }

    /// Root clears a store a service user owns, and nobody else's: what
    /// isn't an owner's stops the clear and stays, a file, a directory or
    /// a place that's a file; and a tree named as an owner's that isn't
    /// goes no further for being named.
    #[test]
    fn clearing_takes_only_what_its_owners_own() {
        let me = hatchling_sys::euid();
        let stranger = [me.wrapping_add(1)];
        let scratch = Scratch::new("clear-owner");
        scratch.write("memory/note", b"ours");
        let memory = scratch.place("memory", Part::Mind);
        assert!(clear_of(&memory, true, &stranger).is_err());
        assert_eq!(scratch.read("memory/note").unwrap(), b"ours");
        scratch.write("files/deep/note", b"ours");
        let files = scratch.place("files", Part::Mind);
        let refused = clear_of(&files, true, &stranger).unwrap_err();
        assert!(
            refused.to_string().contains("isn't Hatchling's"),
            "{refused}"
        );
        assert_eq!(scratch.read("files/deep/note").unwrap(), b"ours");
        scratch.write("activity", b"log");
        let activity = scratch.place("activity", Part::Mind);
        assert!(clear_of(&activity, true, &stranger).is_err());
        assert_eq!(scratch.read("activity").unwrap(), b"log");
        // The owners' look isn't the tree's own check, which stands alone.
        let tree = scratch.0.join("files/deep");
        assert!(clear_tree(&tree, stranger[0], &stranger).is_err());
        assert_eq!(scratch.read("files/deep/note").unwrap(), b"ours");
        // With its owner named, all of it goes, whoever else is.
        clear_of(&files, true, &[stranger[0], me]).unwrap();
        clear_of(&memory, true, &[stranger[0], me]).unwrap();
        clear_of(&activity, true, &[stranger[0], me]).unwrap();
        assert_eq!(scratch.read("files/deep/note"), None);
        assert_eq!(scratch.read("memory/note"), None);
        assert_eq!(scratch.read("activity"), None);
    }

    /// A crash leaves a run's tree owned by the cell's user. Taken as this
    /// process's uid, a stranger standing for Hatchling's own users: a clear
    /// takes it for the agent's uid, or anywhere in the cell's range, first
    /// and last. Anything else stops the clear, names what stopped it, and
    /// leaves it, a uid either side of the range included. A cell that can't
    /// be told adds nothing, and an empty range nothing.
    #[test]
    fn a_clear_takes_the_cells_leftovers_and_nothing_else() {
        let me = hatchling_sys::euid();
        let own = vec![me.wrapping_add(1)];
        let other = me.wrapping_add(2);
        let cell = |first: u32, count: u32| Some(CellIdentity::direct(first, count));
        let scratch = Scratch::new("clear-cell");
        let left = |scratch: &Scratch| {
            scratch.write("memory/run-1/spec", b"left by a crash");
            scratch.write("files/note", b"left by a crash");
            [
                scratch.place("memory", Part::Mind),
                scratch.place("files", Part::Mind),
            ]
        };
        let places = left(&scratch);
        for stops in [
            with_cells(own.clone(), other, None),
            with_cells(own.clone(), other, cell(me.wrapping_add(1), 3)),
            with_cells(own.clone(), other, cell(me.wrapping_sub(3), 3)),
            with_cells(own.clone(), other, cell(me, 0)),
        ] {
            for (place, named) in places.iter().zip(["memory/run-1", "files/note"]) {
                let refused = clear_of(place, true, &stops).unwrap_err().to_string();
                assert!(refused.contains(named), "not named: {refused}");
            }
            assert_eq!(
                scratch.read("memory/run-1/spec").unwrap(),
                b"left by a crash"
            );
            assert_eq!(scratch.read("files/note").unwrap(), b"left by a crash");
        }
        let takes = [
            with_cells(own.clone(), me, None),
            with_cells(own.clone(), other, cell(me, 1)),
            with_cells(own, other, cell(me.wrapping_sub(2), 3)),
        ];
        for owners in takes {
            for place in left(&scratch) {
                clear_of(&place, true, &owners).unwrap();
            }
            assert_eq!(scratch.read("memory/run-1/spec"), None);
            assert_eq!(scratch.read("files/note"), None);
        }
    }

    /// Who's cleared for: Hatchling's own, the agent's uid, and the cell's
    /// range, held to its bound.
    #[test]
    fn a_clear_is_for_hatchlings_own_and_its_cells() {
        assert_eq!(with_cells(vec![1], 5, None), [1, 5]);
        assert_eq!(
            with_cells(vec![1], 5, Some(CellIdentity::direct(10, 3))),
            [1, 5, 10, 11, 12]
        );
        let wide = with_cells(Vec::new(), 5, Some(CellIdentity::direct(10, u32::MAX)));
        assert_eq!(wide.len(), 1 + MAX_CELL_UIDS);
        assert_eq!(wide.last(), Some(&(9 + 65_536)));
        let mut config = crate::api_auth::tests::owner_config(None);
        let agent = hatchling_sys::euid().wrapping_add(7);
        config.agent_uid = hatchling_core::PeerUid::from_kernel(agent);
        let owners = owners(&config);
        assert!(owners.contains(&agent), "{owners:?}");
        assert!(owners.contains(&hatchling_sys::euid()), "{owners:?}");
    }

    /// What a tree too deep to clear whole leaves is said, not passed over.
    #[test]
    fn a_tree_left_partly_is_said() {
        let scratch = Scratch::new("clear-deep");
        let top = scratch.0.join("memory/top");
        let deep = (0..=hatchling_sys::MAX_DEPTH + 1).fold(top, |path, _| path.join("d"));
        fs::create_dir_all(deep).unwrap();
        let refused = clear(&scratch.place("memory", Part::Mind), true).unwrap_err();
        assert!(refused.to_string().contains("wasn't removed"), "{refused}");
    }

    /// A restore's staging starts empty and private. What's left there by
    /// an earlier restore goes; a link stands and stops it, and what it
    /// points at isn't touched; a file stands and stops it.
    #[test]
    fn staging_is_always_new() {
        let scratch = Scratch::new("staging");
        let staging = scratch.0.join("vault.restoring");
        fresh_staging(&staging).unwrap();
        fs::write(staging.join("left"), b"from before").unwrap();
        fresh_staging(&staging).unwrap();
        assert!(!staging.join("left").exists());
        assert_eq!(
            fs::metadata(&staging).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir(&staging).unwrap();

        scratch.write("outside/note", b"not ours");
        let outside = scratch.0.join("outside");
        std::os::unix::fs::symlink(&outside, &staging).unwrap();
        assert!(fresh_staging(&staging).is_err());
        drop_staging(&staging);
        assert!(fs::symlink_metadata(&staging).is_ok(), "the link went");
        assert_eq!(scratch.read("outside/note").unwrap(), b"not ours");
        fs::remove_file(&staging).unwrap();

        fs::write(&staging, b"a file").unwrap();
        assert!(fresh_staging(&staging).is_err());
        assert_eq!(fs::read(&staging).unwrap(), b"a file");
    }

    /// A backup that fails leaves nothing behind it. Only the file it made
    /// goes: a link or a directory at that name stays.
    #[test]
    fn a_failed_backup_leaves_only_what_was_there() {
        let scratch = Scratch::new("partial");
        let key = MasterKey::generate().unwrap();
        // A file that lists, and can't be read, as root or not.
        let unreadable = Place {
            name: String::from("mem"),
            path: PathBuf::from("/proc/self/mem"),
            part: Part::Mind,
        };
        let file = scratch.0.join("h.backup");
        assert!(write_backup(&[unreadable], &key, b"", &file).is_err());
        assert!(!file.with_extension("partial").exists());

        let partial = scratch.0.join("kept.partial");
        scratch.write("mine", b"mine");
        std::os::unix::fs::symlink(scratch.0.join("mine"), &partial).unwrap();
        drop_partial(&partial);
        assert!(fs::symlink_metadata(&partial).is_ok(), "the link went");
        assert_eq!(scratch.read("mine").unwrap(), b"mine");
        fs::remove_file(&partial).unwrap();
        fs::create_dir(&partial).unwrap();
        drop_partial(&partial);
        assert!(partial.is_dir(), "the directory went");
        fs::remove_dir(&partial).unwrap();
        fs::write(&partial, b"made").unwrap();
        drop_partial(&partial);
        assert!(!partial.exists());
    }

    /// The time of the last backup goes only as a file: a link at its name
    /// stops the marking, and neither it nor its target is touched.
    #[test]
    fn a_mark_replaces_only_a_file() {
        let scratch = Scratch::new("mark-link");
        let dir = scratch.0.join("backups");
        mark_backup(&dir).unwrap();
        scratch.write("mine", b"mine");
        let last = dir.join(LAST);
        fs::remove_file(&last).unwrap();
        std::os::unix::fs::symlink(scratch.0.join("mine"), &last).unwrap();
        assert!(mark_backup(&dir).is_err());
        assert!(fs::symlink_metadata(&last).unwrap().is_symlink());
        assert_eq!(scratch.read("mine").unwrap(), b"mine");
    }
}
