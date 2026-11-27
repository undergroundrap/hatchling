//! Whether what the agent asks to remember is kept at once or put to the
//! owner on a card (WO-057): the owner's choice, Automatic or Ask me.
//!
//! The choice is the owner's alone, kept in the broker-owned owner
//! directory ([`crate::config::SchedulerConfig::owner_dir`]) beside the
//! model's, written whole beside and put in place. No file means
//! Automatic, the default for a new install and an upgraded one. A file
//! that doesn't read means Ask me: when what the owner chose can't be
//! told, nothing is kept without them.
//!
//! The mode is only the owner's half of the rule. Even in Automatic, the
//! broker keeps a memory at once only from a chat run whose conversation
//! read nothing from outside ([`crate::conversation_taint`]), and only so
//! many a day ([`crate::memory_keep::MAX_AUTOMATIC_PER_DAY`]).

use std::fs::{DirBuilder, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use crate::config::SchedulerConfig;

/// The file in the owner directory that holds the choice.
pub const MODE_FILE: &str = "memory-mode.json";

/// The largest mode file read.
const MAX_FILE_BYTES: u64 = 256;

/// One write at a time.
static WRITES: Mutex<()> = Mutex::new(());

/// What the owner chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Kept at once when the broker's rule holds; a card otherwise.
    Automatic,
    /// Every proposal is a card.
    Ask,
}

/// The choice, as it's kept.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Kept {
    mode: Mode,
}

/// The mode kept in `owner_dir`: Automatic when nothing is, Ask me when
/// what's there doesn't read (logged).
#[must_use]
pub fn mode_in(owner_dir: &Path) -> Mode {
    let file = match std::fs::File::open(owner_dir.join(MODE_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Mode::Automatic,
        Err(error) => {
            tracing::error!(%error, "the memory mode can't be read: every memory is asked");
            return Mode::Ask;
        }
    };
    let mut text = Vec::new();
    let read = file.take(MAX_FILE_BYTES + 1).read_to_end(&mut text);
    let whole = u64::try_from(text.len()).is_ok_and(|len| len <= MAX_FILE_BYTES);
    match serde_json::from_slice::<Kept>(&text) {
        Ok(kept) if read.is_ok() && whole => kept.mode,
        _ => {
            tracing::error!("the memory mode doesn't read: every memory is asked");
            Mode::Ask
        }
    }
}

/// The mode the owner chose, where `scheduler` keeps it: Ask me when it
/// keeps nothing, since then there's no chat to keep from.
#[must_use]
pub fn mode(scheduler: &SchedulerConfig) -> Mode {
    if scheduler.enabled() {
        mode_in(&scheduler.owner_dir())
    } else {
        Mode::Ask
    }
}

/// Keeps `mode` as the owner's choice, written whole beside and put in
/// place.
///
/// # Errors
///
/// Why it couldn't be kept: scheduling keeps nothing
/// ([`std::io::ErrorKind::Unsupported`]), or the write failed.
pub fn set(scheduler: &SchedulerConfig, mode: Mode) -> std::io::Result<()> {
    if !scheduler.enabled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "scheduling is off: there's no place to keep the memory mode",
        ));
    }
    let dir = scheduler.owner_dir();
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let next = dir.join(format!("{MODE_FILE}.next"));
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&next)?
        .write_all(&serde_json::to_vec(&Kept { mode })?)?;
    std::fs::rename(&next, dir.join(MODE_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduler(name: &str) -> (SchedulerConfig, crate::api_auth::tests::Scratch) {
        let scratch = crate::api_auth::tests::scratch(&format!("memory-mode-{name}"));
        let mut scheduler = SchedulerConfig::disabled();
        scheduler.root = scratch.0.join("schedules");
        (scheduler, scratch)
    }

    #[test]
    fn with_nothing_chosen_memory_is_automatic() {
        let (scheduler, _scratch) = scheduler("default");
        assert!(scheduler.enabled());
        assert_eq!(mode(&scheduler), Mode::Automatic);
    }

    #[test]
    fn the_owners_choice_is_kept_and_read_back() {
        let (scheduler, _scratch) = scheduler("set");
        set(&scheduler, Mode::Ask).unwrap();
        assert_eq!(mode(&scheduler), Mode::Ask);
        set(&scheduler, Mode::Automatic).unwrap();
        assert_eq!(mode(&scheduler), Mode::Automatic);
        let written = std::fs::read_to_string(scheduler.owner_dir().join(MODE_FILE)).unwrap();
        assert_eq!(written, r#"{"mode":"automatic"}"#);
    }

    #[test]
    fn a_choice_that_doesnt_read_asks() {
        let (scheduler, _scratch) = scheduler("unreadable");
        let dir = scheduler.owner_dir();
        std::fs::create_dir_all(&dir).unwrap();
        for text in [
            String::from("{\"mode\":\"always\"}"),
            String::from("not json"),
            String::from("{\"mode\":\"automatic\",\"more\":1}"),
            format!("{{\"mode\":\"automatic\"}}{}", " ".repeat(300)),
        ] {
            std::fs::write(dir.join(MODE_FILE), &text).unwrap();
            assert_eq!(mode(&scheduler), Mode::Ask, "{text}");
        }
        // A directory where the file should be can't be read either.
        std::fs::remove_file(dir.join(MODE_FILE)).unwrap();
        std::fs::create_dir(dir.join(MODE_FILE)).unwrap();
        assert_eq!(mode(&scheduler), Mode::Ask);
    }

    /// A choice that can't be opened, for any reason but that there is
    /// none, asks.
    #[test]
    fn a_choice_that_cant_be_opened_asks() {
        let (scheduler, _scratch) = scheduler("unopened");
        // The owner directory is a file: nothing in it opens.
        std::fs::create_dir_all(scheduler.owner_dir().parent().unwrap()).unwrap();
        std::fs::write(scheduler.owner_dir(), b"").unwrap();
        assert_eq!(mode(&scheduler), Mode::Ask);
    }

    #[test]
    fn without_scheduling_every_memory_is_asked_and_none_is_kept() {
        let scheduler = SchedulerConfig::disabled();
        assert_eq!(mode(&scheduler), Mode::Ask);
        assert_eq!(
            set(&scheduler, Mode::Automatic).unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
    }
}
