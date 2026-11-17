//! The Library (WO-041): the files the agent hands its owner, sealed in a
//! store of its own (`HATCHLING_LIBRARY_DIR`).
//!
//! - **A file is kept whole, in pieces.** A sealed entry holds at most
//!   [`CHUNK_BYTES`], so a file is kept as its chunks, in order, each an
//!   entry of its own, and the index names it: its id, its name, its
//!   length, its SHA-256 and how many chunks it is. The id is the broker's,
//!   random, so no two files share a chunk's name. A file is read back only
//!   whole, and only if its chunks come to its hash: a store that was torn
//!   or tampered with reads as an error, never as other bytes.
//! - **Bounded, and nothing is evicted.** At most [`LIBRARY_FILES`] files
//!   and [`LIBRARY_BYTES`] in all, each at most [`FILE_BYTES`]. A full
//!   Library refuses a file; the owner removes files.
//! - **The open conversation's count, and its off.** At most
//!   [`HANDED_PER_CONVERSATION`] files are handed over in one conversation,
//!   and a file found to hold a secret turns handing files over off for
//!   it. Both are kept here, sealed, so a restart keeps them, and only the
//!   owner's new conversation clears them.
//! - **A crash leaves nothing half done.** Before a file is written, or
//!   removed, what is about to happen is written (`pending`), and the
//!   count with it, so a crash never gives a hand-over back. Every
//!   operation first finishes or undoes what a crash left: a keep whose
//!   file the index names is done, any other is undone; a removal is
//!   always finished.
//!
//! Every operation holds one lock for the process, so nothing here is
//! seen half done by another.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::room::TAKE_FILE_BYTES;
use hatchling_core::{MAX_SECRET_BYTES, SecretName};
use hatchling_vault::{OpensStores, SealedVault, Secret, Store};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::Config;

/// The most files the Library keeps.
pub const LIBRARY_FILES: usize = 64;

/// The most the Library keeps in all, in bytes.
pub const LIBRARY_BYTES: u64 = 256 * 1024 * 1024;

/// The most files handed over in one conversation.
pub const HANDED_PER_CONVERSATION: u32 = 16;

/// The most of a file one entry keeps: a sealed entry's most.
pub const CHUNK_BYTES: usize = MAX_SECRET_BYTES;

/// The longest file the Library keeps: the longest the Room hands back.
pub const FILE_BYTES: u64 = TAKE_FILE_BYTES;

/// The longest a file's name may be, in bytes.
pub const NAME_BYTES: usize = 64;

/// The index's entry name.
const INDEX: &str = "library-index";

/// The state's entry name.
const STATE: &str = "library-state";

/// Serializes every operation within a broker.
static LIBRARY: Mutex<()> = Mutex::new(());

/// Whether `name` is one the Library keeps a file by: 1 to [`NAME_BYTES`]
/// ASCII letters, digits, `.`, `_` or `-`, not starting with a dot.
#[must_use]
pub fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// A file the Library keeps, as its index says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptFile {
    /// The broker's id for it: random hex.
    pub id: String,
    /// Its name, as the broker made it.
    pub name: String,
    /// Its length, in bytes.
    pub bytes: u64,
    /// Its SHA-256, in lowercase hex.
    pub sha256: String,
    /// How many chunks it is kept in.
    pub chunks: u64,
    /// When it was kept.
    pub kept_at: String,
}

/// Why a file wasn't kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Handing files over is off for this conversation: a file was found
    /// to hold a secret.
    Off,
    /// This conversation has handed over as many files as it may.
    Spent,
    /// The Library holds as many files, or as many bytes, as it may.
    Full,
    /// The file is longer than the Library keeps.
    TooLarge,
}

impl Refused {
    /// Why, in words for the agent and the owner.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Off => {
                "handing files over is off for this conversation: a file was found to hold a secret Hatchling keeps. A new conversation turns it back on"
            }
            Self::Spent => {
                "this conversation has handed over 16 files, the most one may: a new conversation may hand over more"
            }
            Self::Full => {
                "the Library is full (64 files, or 256 MiB in all): the owner removes files from it"
            }
            Self::TooLarge => "the file is larger than 8 MiB, the most a file handed over may be",
        }
    }
}

/// What is about to be done to a file: written before it is done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    op: Op,
    id: String,
    chunks: u64,
}

/// Which change a pending one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Op {
    Keep,
    Remove,
}

/// The Library's state: what is about to be done, and the open
/// conversation's count and off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    pending: Option<Pending>,
    handed: u32,
    off: bool,
}

/// How many chunks a file of `bytes` is kept in.
fn chunks_of(bytes: u64) -> u64 {
    bytes.div_ceil(u64::try_from(CHUNK_BYTES).unwrap_or(u64::MAX))
}

/// The entry name of chunk `chunk` of file `id`.
fn chunk_name(id: &str, chunk: u64) -> String {
    format!("library-{id}-{chunk}")
}

/// `bytes` as lowercase hex.
fn hex(bytes: &[u8]) -> String {
    crate::approval::hex_token("", bytes)
}

/// Whether `id` is one the broker gives: 64 lowercase hex digits.
fn is_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The Library's sealed store.
pub struct Library<'k> {
    root: &'k Path,
    keys: &'k dyn OpensStores,
}

impl std::fmt::Debug for Library<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl<'k> Library<'k> {
    /// The Library `config` gives the broker, opened with `keys`; `None`
    /// when the operator gave it none, and nothing is handed over.
    #[must_use]
    pub fn of(config: &'k Config, keys: &'k dyn OpensStores) -> Option<Self> {
        let root = config.scheduler.library.as_path();
        (!root.as_os_str().is_empty()).then_some(Self { root, keys })
    }

    fn vault(&self) -> Result<SealedVault<'_>, String> {
        SealedVault::open_in(self.root, self.keys, Store::LIBRARY)
            .map_err(|error| format!("the Library can't be opened: {error}"))
    }

    fn name(raw: &str) -> Result<SecretName, String> {
        raw.parse()
            .map_err(|_| format!("the Library's entry name {raw:?} is invalid"))
    }

    fn read<T: serde::de::DeserializeOwned>(
        vault: &SealedVault<'_>,
        entry: &str,
    ) -> Result<Option<T>, String> {
        let secret = match vault.fetch(&Self::name(entry)?) {
            Ok(secret) => secret,
            Err(hatchling_vault::Error::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(format!("the Library can't be read: {error}")),
        };
        serde_json::from_slice(secret.as_bytes())
            .map(Some)
            .map_err(|error| format!("the Library's {entry} is corrupt: {error}"))
    }

    fn write<T: Serialize>(vault: &SealedVault<'_>, entry: &str, value: &T) -> Result<(), String> {
        let bytes = serde_json::to_vec(value)
            .map_err(|error| format!("the Library's {entry} doesn't encode: {error}"))?;
        Self::put(vault, entry, bytes)
    }

    fn put(vault: &SealedVault<'_>, entry: &str, bytes: Vec<u8>) -> Result<(), String> {
        let secret = Secret::new(bytes)
            .map_err(|error| format!("the Library's {entry} can't be kept: {error}"))?;
        vault
            .store(&Self::name(entry)?, &secret)
            .map_err(|error| format!("the Library's {entry} can't be written: {error}"))
    }

    /// The index, held to what one can be: at most [`LIBRARY_FILES`] files,
    /// each with an id the broker gives, a name the Library keeps, at most
    /// [`FILE_BYTES`] and the chunks that length is kept in.
    fn index(vault: &SealedVault<'_>) -> Result<Vec<KeptFile>, String> {
        let index: Vec<KeptFile> = Self::read(vault, INDEX)?.unwrap_or_default();
        let sound = index.len() <= LIBRARY_FILES
            && index.iter().all(|file| {
                is_id(&file.id)
                    && is_name(&file.name)
                    && file.bytes <= FILE_BYTES
                    && file.chunks == chunks_of(file.bytes)
            });
        if !sound {
            return Err(String::from(
                "the Library's index holds what an index can't",
            ));
        }
        Ok(index)
    }

    fn state(vault: &SealedVault<'_>) -> Result<State, String> {
        let state: State = Self::read(vault, STATE)?.unwrap_or_default();
        if state.handed > HANDED_PER_CONVERSATION
            || state.pending.as_ref().is_some_and(|pending| {
                !is_id(&pending.id) || pending.chunks > chunks_of(FILE_BYTES)
            })
        {
            return Err(String::from("the Library's state holds what a state can't"));
        }
        Ok(state)
    }

    /// Removes chunks `0..chunks` of file `id`: one already gone is gone.
    fn remove_chunks(vault: &SealedVault<'_>, id: &str, chunks: u64) -> Result<(), String> {
        for chunk in 0..chunks {
            match vault.remove(&Self::name(&chunk_name(id, chunk))?) {
                Ok(()) | Err(hatchling_vault::Error::NotFound { .. }) => {}
                Err(error) => {
                    return Err(format!("the Library's file can't be removed: {error}"));
                }
            }
        }
        Ok(())
    }

    /// Finishes or undoes what a crash, or a failed write, left half done:
    /// a keep whose file the index names is done; any other keep is
    /// undone, its chunks removed; a removal is finished. The state then
    /// has nothing pending.
    fn recover(vault: &SealedVault<'_>) -> Result<State, String> {
        let mut state = Self::state(vault)?;
        let Some(pending) = state.pending.take() else {
            return Ok(state);
        };
        let mut index = Self::index(vault)?;
        let named = index.iter().any(|file| file.id == pending.id);
        match pending.op {
            Op::Keep if named => {}
            Op::Keep => Self::remove_chunks(vault, &pending.id, pending.chunks)?,
            Op::Remove => {
                if named {
                    index.retain(|file| file.id != pending.id);
                    Self::write(vault, INDEX, &index)?;
                }
                Self::remove_chunks(vault, &pending.id, pending.chunks)?;
            }
        }
        Self::write(vault, STATE, &state)?;
        Ok(state)
    }

    /// Opens the store, with the lock held, after recovering it.
    fn opened<T>(
        &self,
        then: impl FnOnce(&SealedVault<'_>, State) -> Result<T, String>,
    ) -> Result<T, String> {
        let _held = LIBRARY.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let state = Self::recover(&vault)?;
        then(&vault, state)
    }

    /// Why a file of `bytes` wouldn't be kept now, if it wouldn't: what is
    /// asked before a file is read out of the Room. [`Self::keep`] asks
    /// again.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn refuses(&self, bytes: u64) -> Result<Option<Refused>, String> {
        self.opened(|vault, state| Ok(Self::refusal(&Self::index(vault)?, &state, bytes)))
    }

    fn refusal(index: &[KeptFile], state: &State, bytes: u64) -> Option<Refused> {
        let held = index
            .iter()
            .fold(0_u64, |held, file| held.saturating_add(file.bytes));
        if bytes > FILE_BYTES {
            Some(Refused::TooLarge)
        } else if state.off {
            Some(Refused::Off)
        } else if state.handed >= HANDED_PER_CONVERSATION {
            Some(Refused::Spent)
        } else if index.len() >= LIBRARY_FILES || held.saturating_add(bytes) > LIBRARY_BYTES {
            Some(Refused::Full)
        } else {
            None
        }
    }

    /// Keeps `bytes` as the file `name`, at `now`: the file as the index
    /// says it, or why it wasn't kept. Its hash is of these bytes.
    ///
    /// # Errors
    ///
    /// When `name` isn't one the Library keeps a file by, or the Library
    /// can't be read or written before the file is in its index. A keep
    /// that fails part way is undone by the next operation, and still
    /// counts as handed over; one whose file is in the index is kept,
    /// whatever fails after.
    pub fn keep(
        &self,
        name: &str,
        bytes: &[u8],
        now: &str,
    ) -> Result<Result<KeptFile, Refused>, String> {
        if !is_name(name) {
            return Err(format!(
                "the Library doesn't keep a file by the name {name:?}"
            ));
        }
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let id = crate::approval::random_hex("")
            .map_err(|error| format!("the Library can't name a file: {error}"))?;
        self.opened(|vault, mut state| {
            let mut index = Self::index(vault)?;
            if let Some(refused) = Self::refusal(&index, &state, length) {
                return Ok(Err(refused));
            }
            let chunks = chunks_of(length);
            state.pending = Some(Pending {
                op: Op::Keep,
                id: id.clone(),
                chunks,
            });
            state.handed = state.handed.saturating_add(1);
            Self::write(vault, STATE, &state)?;
            for (chunk, piece) in (0_u64..).zip(bytes.chunks(CHUNK_BYTES)) {
                Self::put(vault, &chunk_name(&id, chunk), piece.to_vec())?;
            }
            let kept = KeptFile {
                id,
                name: name.to_owned(),
                bytes: length,
                sha256: hex(&Sha256::digest(bytes)),
                chunks,
                kept_at: now.to_owned(),
            };
            index.push(kept.clone());
            if let Err(error) = Self::write(vault, INDEX, &index) {
                // A write can fail after its rename: what the index says
                // now is whether the file was kept.
                let named = Self::index(vault)
                    .is_ok_and(|index| index.iter().any(|file| file.id == kept.id));
                if !named {
                    return Err(error);
                }
            }
            // Kept. What is left pending, if this write fails, the next
            // operation finishes.
            state.pending = None;
            let _ = Self::write(vault, STATE, &state);
            Ok(Ok(kept))
        })
    }

    /// Turns handing files over off for the open conversation.
    ///
    /// # Errors
    ///
    /// When the Library can't be read or written.
    pub fn turn_off(&self) -> Result<(), String> {
        self.opened(|vault, mut state| {
            state.off = true;
            Self::write(vault, STATE, &state)
        })
    }

    /// The owner started a new conversation: its count starts again, and
    /// handing files over is on.
    ///
    /// # Errors
    ///
    /// When the Library can't be read or written.
    pub fn new_conversation(&self) -> Result<(), String> {
        self.opened(|vault, mut state| {
            state.handed = 0;
            state.off = false;
            Self::write(vault, STATE, &state)
        })
    }

    /// The open conversation's count of files handed over, and whether
    /// handing files over is off for it.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn standing(&self) -> Result<(u32, bool), String> {
        self.opened(|_, state| Ok((state.handed, state.off)))
    }

    /// The files the Library keeps, oldest first.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn files(&self) -> Result<Vec<KeptFile>, String> {
        self.opened(|vault, _| Self::index(vault))
    }

    /// The file `id`, whole, as it was kept; `None` when the Library keeps
    /// no such file.
    ///
    /// # Errors
    ///
    /// When the Library can't be read, or the file's chunks don't come to
    /// its hash.
    pub fn fetch(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
        self.opened(|vault, _| {
            let index = Self::index(vault)?;
            let Some(file) = index.iter().find(|file| file.id == id) else {
                return Ok(None);
            };
            let damaged = || format!("the Library's file {} is damaged", file.name);
            let mut bytes = Vec::with_capacity(usize::try_from(file.bytes).unwrap_or(0));
            for chunk in 0..file.chunks {
                let secret = vault
                    .fetch(&Self::name(&chunk_name(&file.id, chunk))?)
                    .map_err(|_| damaged())?;
                bytes.extend_from_slice(secret.as_bytes());
            }
            // The hash is the index's, sealed: chunks of another file, cut
            // short or out of order come to another hash. No more than the
            // index's chunks are read, each at most a sealed entry's most.
            if hex(&Sha256::digest(&bytes)) != file.sha256 {
                return Err(damaged());
            }
            Ok(Some(bytes))
        })
    }

    /// Removes the file `id`: whether the Library kept it.
    ///
    /// # Errors
    ///
    /// When the Library can't be read, or the removal can't be written
    /// as pending. Once it is, the file is removed: what fails after, the
    /// next operation finishes.
    pub fn remove(&self, id: &str) -> Result<bool, String> {
        self.opened(|vault, mut state| {
            let mut index = Self::index(vault)?;
            let Some(file) = index.iter().find(|file| file.id == id).cloned() else {
                return Ok(false);
            };
            let pending = Pending {
                op: Op::Remove,
                id: file.id.clone(),
                chunks: file.chunks,
            };
            state.pending = Some(pending.clone());
            if let Err(error) = Self::write(vault, STATE, &state) {
                // A write can fail after its rename: a removal written as
                // pending is one the next operation finishes.
                let written = Self::state(vault).is_ok_and(|now| now.pending == Some(pending));
                if !written {
                    return Err(error);
                }
            }
            // From here the removal is made: what this doesn't finish, the
            // next operation does.
            index.retain(|kept| kept.id != file.id);
            if Self::write(vault, INDEX, &index).is_ok()
                && Self::remove_chunks(vault, &file.id, file.chunks).is_ok()
            {
                state.pending = None;
                let _ = Self::write(vault, STATE, &state);
            }
            Ok(true)
        })
    }
}

#[cfg(test)]
#[path = "library/tests.rs"]
mod tests;
