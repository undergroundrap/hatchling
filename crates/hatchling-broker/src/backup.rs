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
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;

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
            let _ = fs::remove_file(&partial);
            Err(error)
        }
    }
}

/// Clears `place`. A sealed store's dotfiles, its wrapped key among them,
/// stay unless `all`: a store with no key can't take new secrets.
fn clear(place: &Place, all: bool) -> Result<(), Error> {
    let Ok(kind) = fs::symlink_metadata(&place.path) else {
        return Ok(());
    };
    if !kind.is_dir() {
        return fs::remove_file(&place.path).map_err(ring::io_error);
    }
    for entry in fs::read_dir(&place.path).map_err(ring::io_error)? {
        let entry = entry.map_err(ring::io_error)?;
        if !all && entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let inner = fs::symlink_metadata(&path).map_err(ring::io_error)?;
        if inner.is_dir() {
            fs::remove_dir_all(&path).map_err(ring::io_error)?;
        } else {
            fs::remove_file(&path).map_err(ring::io_error)?;
        }
    }
    Ok(())
}

/// Clears what `level` clears of `places`.
fn clear_level(places: &[Place], level: Level) -> Result<(), Error> {
    places
        .iter()
        .filter(|place| place.part.cleared_by(level))
        .try_for_each(|place| clear(place, level == Level::Everything))
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

/// Puts `staging`'s files in `places`, clearing each first.
fn swap_in(places: &[Place], staging: &Path) -> Result<(), Error> {
    for place in places {
        clear(place, true)?;
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
        .map(|entry| entry.path())
        .collect();
    // Their names start with the time, so they sort by it.
    kept.sort();
    let extra = kept.len().saturating_sub(KEEP);
    for old in kept.iter().take(extra) {
        fs::remove_file(old).map_err(ring::io_error)?;
    }
    Ok(())
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
    keyring: &KeyringConfig,
    to: Option<&Path>,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<PathBuf, Error> {
    let (key, ring_bytes) = opened(config, keyring, ask, devices)?;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let path = to.map_or_else(
        || Path::new(BACKUPS).join(backup_name(seconds)),
        Path::to_path_buf,
    );
    if to.is_none() {
        fs::create_dir_all(BACKUPS).map_err(ring::io_error)?;
        fs::set_permissions(BACKUPS, fs::Permissions::from_mode(0o700)).map_err(ring::io_error)?;
    }
    write_backup(&places(config)?, &key, &ring_bytes, &path)?;
    if to.is_none() {
        prune(Path::new(BACKUPS))?;
    }
    Ok(path)
}

/// `backup [file]`.
///
/// # Errors
///
/// Why not; the service is started again whatever happened.
pub fn run_backup(config: &Config, to: Option<&Path>) -> Result<(), Error> {
    must_be_root()?;
    let keyring = KeyringConfig::from_env()?;
    service("stop")?;
    let (mut ask, mut devices) = ring::terminal_operator();
    let made = make_backup(config, &keyring, to, &mut ask, &mut devices);
    service("start")?;
    let made = made?;
    let _ = writeln!(
        std::io::stdout().lock(),
        "Backed up to {}.\nIt opens with what opens your vault today. To bring it back: sudo hatchling-broker restore <file>",
        made.display()
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
    service("stop")?;
    let (mut ask, mut devices) = ring::terminal_operator();
    let restored = (|| {
        let (key, staged_ring) =
            open_for_restore(config, &keyring_config, &ring_bytes, &mut ask, &mut devices)?;
        let staging = config.vault.root.with_extension("restoring");
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(ring::io_error)?;
        let mut reader = ArchiveReader::new(file, &key, &ring_bytes).map_err(ring::vault_error)?;
        let count = stage(&mut reader, &by_name, &staging)?;
        swap_in(&places, &staging)?;
        if let (Some(path), false) = (keyring_config.keyring.as_deref(), ring_bytes.is_empty()) {
            fs::rename(&staged_ring, path).map_err(ring::io_error)?;
        }
        let _ = fs::remove_dir_all(&staging);
        Ok::<usize, Error>(count)
    })();
    service("start")?;
    let count = restored?;
    let _ = writeln!(
        std::io::stdout().lock(),
        "Restored {count} files from {}.",
        from.display()
    );
    Ok(())
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
    fs::write(&staged, ring_bytes).map_err(ring::io_error)?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o600)).map_err(ring::io_error)?;
    if let Ok(Some((uid, gid))) = crate::keeper::keeper_user() {
        std::os::unix::fs::chown(&staged, Some(uid), Some(gid)).map_err(ring::io_error)?;
    }
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
    service("stop")?;
    let done = (|| {
        if !has("--no-backup") {
            let made = make_backup(config, &keyring, None, &mut ask, &mut devices)?;
            let _ = writeln!(
                std::io::stdout().lock(),
                "Backed up first, to {}.",
                made.display()
            );
        }
        clear_level(&places(config)?, level)?;
        if level == Level::Everything {
            erase_keys(config, &keyring)?;
        }
        Ok::<(), Error>(())
    })();
    service("start")?;
    done?;
    let _ = writeln!(
        std::io::stdout().lock(),
        "Done. {}",
        match level {
            Level::Everything => "Your next step is the same as a new install: docs/install.md",
            Level::Conversation | Level::Agent => "Hatchling is starting.",
        }
    );
    Ok(())
}

/// What opens the vault goes too, for a start from nothing.
fn erase_keys(config: &Config, keyring: &KeyringConfig) -> Result<(), Error> {
    if let Some(path) = keyring.keyring.as_deref() {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(crate::key_setup::codes_marker(path));
    }
    let _ = fs::remove_file(keyring.kek_file(&config.vault));
    let _ = fs::remove_file(&config.vault.key_file);
    let key = FileKey::new(config.vault.key_file.clone())
        .load_or_create()
        .map_err(ring::vault_error)?;
    drop(key);
    crate::vault_ops::run_migrate_vault(config)?;
    if keyring.keyring.is_some() {
        ring::init(&config.vault, keyring)?;
    }
    Ok(())
}

/// How `reset` is used.
pub const USAGE: &str =
    "usage: hatchling-broker reset <conversation | agent | everything> [--yes] [--no-backup]";

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

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
        clear_level(&places, Level::Conversation).unwrap();
        assert_eq!(scratch.read("conversation/chat"), None);
        assert_eq!(scratch.read("conversation/.key").unwrap(), b"wrapped");
        assert_eq!(scratch.read("memory/deep/note").unwrap(), b"remember");
        let scratch = Scratch::new("levels-agent");
        let places = three(&scratch);
        clear_level(&places, Level::Agent).unwrap();
        assert_eq!(scratch.read("memory/deep/note"), None);
        assert_eq!(scratch.read("secrets/token").unwrap(), b"sealed");
        let scratch = Scratch::new("levels-all");
        let places = three(&scratch);
        clear_level(&places, Level::Everything).unwrap();
        assert_eq!(scratch.read("secrets/token"), None);
        assert_eq!(scratch.read("conversation/.key"), None);
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
        clear_level(&places, Level::Agent).unwrap();
        scratch.write("memory/newer", b"added since");

        let mut opened = fs::File::open(&file).unwrap();
        assert_eq!(archive_keyring(&mut opened).unwrap(), b"ring");
        let mut reader = ArchiveReader::new(opened, &key, b"ring").unwrap();
        let by_name: BTreeMap<&str, &Place> = places
            .iter()
            .map(|place| (place.name.as_str(), place))
            .collect();
        let staging = scratch.0.join("staging");
        fs::create_dir_all(&staging).unwrap();
        assert_eq!(stage(&mut reader, &by_name, &staging).unwrap(), 4);
        swap_in(&places, &staging).unwrap();
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
}
