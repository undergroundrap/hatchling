//! The Library (WO-041, WO-043): what the agent hands its owner, sealed in
//! a store of its own (`HATCHLING_LIBRARY_DIR`).
//!
//! - **An item is a file, or a folder of files.** Its bytes are kept back
//!   to back, in chunks of at most [`CHUNK_BYTES`], each an entry of its
//!   own; a folder's list of its files (each path, length and SHA-256, in
//!   the order of its bytes) is kept as chunks beside them. The index
//!   names each item: its id, kind, name, length, count of files, hash
//!   (a file's, or a folder's list's), how many chunks of each, when it was
//!   kept, and the conversation and reply it came from. The id is the
//!   broker's, random, so no two items share a chunk's name. A file is read
//!   back only whole, and only to its hash; a folder's list only to its
//!   hash, and each of its files only to its own: a store that was torn or
//!   tampered with reads as damaged, never as other bytes.
//! - **Grouped by the broker.** The conversation an item came from is the
//!   Library's own number for it, and the reply the chat run's cell id:
//!   both set by the broker, sealed in the index, never taken from what the
//!   Room handed over.
//! - **Bounded, and nothing is evicted.** At most [`LIBRARY_ITEMS`] items,
//!   [`LIBRARY_BYTES`] and [`LIBRARY_FILES`] files in all; a file at most
//!   [`FILE_BYTES`]; a folder at most [`FOLDER_BYTES`], [`FOLDER_FILES`]
//!   files and [`FOLDER_DEPTH`] steps deep, its paths the broker's rules'
//!   ([`check_paths`]). A full Library refuses an item; the owner removes
//!   items. A folder's list isn't counted in the bytes: at most
//!   [`LIST_CHUNKS`] chunks of it an item.
//! - **The open conversation's count, its bytes, and its off.** At most
//!   [`HANDED_PER_CONVERSATION`] hand-overs in one conversation, and
//!   [`READ_PER_CONVERSATION`] read out of the Room; a hand-over found to
//!   hold a secret turns handing over off for it. All kept here, sealed, so
//!   a restart keeps them, and only the owner's new conversation clears
//!   them: a conversation the chat doesn't resume after a restart is
//!   grouped apart ([`at_start`]), and gets nothing back.
//! - **Spent before it is checked** ([`Library::spend`]): each check is a
//!   guess at a secret, so every one is counted, sealed, whatever comes of
//!   it, and one the Library has no room for is refused first. A file is
//!   spent once it is read, and every byte read counts, refused or not, so
//!   a loop of reads the Library refuses ends at the conversation's bytes;
//!   a folder is spent before it is read, its files and bytes as its
//!   listing says them. A hand-over belongs to the conversation it began
//!   in, by a number the Library gives each: one begun before a new
//!   conversation is neither counted, kept nor turns giving off there. A
//!   keep takes the [`Spent`] its spend gave, and keeps only what was
//!   spent.
//! - **A crash leaves nothing half done.** Before an item is written, or
//!   removed, what is about to happen is written (`pending`): its two runs
//!   of chunks, known before anything is written, so undoing it never reads
//!   a list that may be torn. Every operation first finishes or undoes
//!   what a crash left: a keep whose item the index names is done, any
//!   other is undone; a removal is always finished.
//! - **Quiet.** The chunks are written and removed without a log line
//!   each ([`SealedVault::store_quietly`]); the Library logs each item kept,
//!   removed or undone once, by its id, kind, files and bytes, never its
//!   name or a path, which came from the agent.
//!
//! Every operation holds one lock for the process, so nothing here is
//! seen half done by another.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, Once, PoisonError};

use hatchling_core::room::TAKE_FILE_BYTES;
use hatchling_core::{LeftOut, MAX_SECRET_BYTES, SecretName};
use hatchling_vault::{OpensStores, SealedVault, Secret, Store};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::Config;

/// The most items the Library keeps.
pub const LIBRARY_ITEMS: usize = 64;

/// The most the Library keeps in all, in bytes.
pub const LIBRARY_BYTES: u64 = 256 * 1024 * 1024;

/// The most files the Library keeps in all, a folder's each counted.
pub const LIBRARY_FILES: u64 = 4096;

/// The most hand-overs in one conversation.
pub const HANDED_PER_CONVERSATION: u32 = 16;

/// The most read out of the Room in one conversation, in bytes.
pub const READ_PER_CONVERSATION: u64 = 128 * 1024 * 1024;

/// The most of an item one entry keeps: a sealed entry's most.
pub const CHUNK_BYTES: usize = MAX_SECRET_BYTES;

/// The longest file the Library keeps: the longest the Room hands back.
pub const FILE_BYTES: u64 = TAKE_FILE_BYTES;

/// The most a folder keeps in all, in bytes.
pub const FOLDER_BYTES: u64 = 32 * 1024 * 1024;

/// The most files a folder keeps.
pub const FOLDER_FILES: u64 = 512;

/// The most steps a path in a folder has.
pub const FOLDER_DEPTH: usize = 16;

/// The longest a path in a folder may be, in bytes.
pub const PATH_BYTES: usize = 1024;

/// The longest one step of a path in a folder may be, in bytes.
pub const STEP_BYTES: usize = 255;

/// The longest an item's name may be, in bytes.
pub const NAME_BYTES: usize = 64;

/// The most a folder's list is, as JSON: [`FOLDER_FILES`] of the longest
/// paths ([`PATH_BYTES`]), each with the most said of it (128 bytes).
const LIST_BYTES: u64 = FOLDER_FILES * (1024 + 128) + 2;

/// The most chunks a folder's list is kept in: [`LIST_BYTES`] in chunks of
/// [`CHUNK_BYTES`].
pub const LIST_CHUNKS: u64 = LIST_BYTES.div_ceil(32 * 1024);

/// The longest an item's time of keeping may be, in bytes.
const KEPT_AT_BYTES: usize = 40;

/// The most a conversation's number may be: what a JSON number holds
/// exactly, so the owner's app reads it as it is.
pub const CONVERSATIONS: u64 = (1 << 53) - 1;

/// A run's cell id: `run-` and 24 lowercase hex digits.
const RUN_ID_BYTES: usize = 4 + 24;

/// The index's entry name.
const INDEX: &str = "library-index";

/// The state's entry name.
const STATE: &str = "library-state";

/// Serializes every operation within a broker.
static LIBRARY: Mutex<()> = Mutex::new(());

/// Whether `name` is one the Library keeps an item by: 1 to [`NAME_BYTES`]
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

/// Whether `reply` is one an item may name: none (an item kept before
/// items were grouped), or a run's cell id.
pub(crate) fn is_reply(reply: &str) -> bool {
    reply.is_empty()
        || (reply.len() == RUN_ID_BYTES
            && reply.strip_prefix("run-").is_some_and(|hex| {
                hex.bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }))
}

/// Why one step of a path in a folder isn't one the Library keeps, if it
/// isn't: printable ASCII but `< > : " \ | ? *`, not empty, `.` or `..`,
/// no space at either end, no dot at its end, at most [`STEP_BYTES`], and
/// no name Windows keeps for a device or acts on when its folder is shown.
fn step_refused(step: &str) -> Option<&'static str> {
    if step.is_empty() || step == "." || step == ".." {
        return Some("a path in a folder has a step that is empty, . or ..");
    }
    if step.len() > STEP_BYTES {
        return Some("a name in a folder is longer than 255 bytes");
    }
    let plain = step.bytes().all(|byte| {
        (b' '..=b'~').contains(&byte)
            && !matches!(byte, b'<' | b'>' | b':' | b'"' | b'\\' | b'|' | b'?' | b'*')
    });
    if !plain {
        return Some("a name in a folder isn't plain ASCII, or holds a character Windows forbids");
    }
    if step.starts_with(' ') || step.ends_with(' ') || step.ends_with('.') {
        return Some("a name in a folder starts or ends with a space, or ends with a dot");
    }
    let before_dot = step.split('.').next().unwrap_or_default();
    if crate::hand_over::is_reserved(before_dot.trim_end()) {
        return Some("a name in a folder is one Windows keeps for a device");
    }
    if crate::hand_over::reaches_out(step) {
        return Some(
            "a name in a folder is a kind of file Windows acts on when its folder is shown",
        );
    }
    None
}

/// Whether `paths` can be a folder's, as the broker keeps one.
///
/// Each is a relative path of at most [`PATH_BYTES`] and [`FOLDER_DEPTH`]
/// steps, each step one the Library keeps; no two are the same but for
/// case; and none is a file where another has a folder.
///
/// # Errors
///
/// Which rule one broke: never the path, which came from the agent.
pub fn check_paths<'p>(paths: impl IntoIterator<Item = &'p str>) -> Result<(), &'static str> {
    let mut seen = HashSet::new();
    let mut lowered = Vec::new();
    for path in paths {
        if path.len() > PATH_BYTES {
            return Err("a path in a folder is longer than 1024 bytes");
        }
        let steps: Vec<&str> = path.split('/').collect();
        if steps.len() > FOLDER_DEPTH {
            return Err("a path in a folder is more than 16 steps deep");
        }
        if let Some(why) = steps.iter().find_map(|step| step_refused(step)) {
            return Err(why);
        }
        let lower = path.to_ascii_lowercase();
        if !seen.insert(lower.clone()) {
            return Err("two paths in a folder are the same but for case");
        }
        lowered.push(lower);
    }
    // A file named where another path has a folder: each folder a path
    // is in, up to each of its slashes.
    for path in &lowered {
        for (slash, _) in path.match_indices('/') {
            let folder = path.get(..slash).unwrap_or_default();
            if seen.contains(folder) {
                return Err("a path in a folder is a file where another has a folder");
            }
        }
    }
    Ok(())
}

/// What an item is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    /// A file.
    #[default]
    File,
    /// A folder of files.
    Folder,
}

/// An item read back for the owner.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadOut {
    /// The file, whole, to its hash: wiped when dropped.
    Whole(Zeroizing<Vec<u8>>),
    /// The Library keeps no such file.
    Gone,
    /// Its chunks don't come to its hash, or one can't be read: nothing of
    /// it is given.
    Damaged,
}

/// A folder's list read back.
#[derive(Debug, PartialEq, Eq)]
pub enum ListOut {
    /// Its files, in the order of its bytes, to the list's hash.
    Whole(Vec<FolderFile>),
    /// The Library keeps no such folder.
    Gone,
    /// The list doesn't come to its hash, or isn't one a folder can have.
    Damaged,
}

/// A file of a folder read back for the owner, with its entry.
#[derive(Debug, PartialEq, Eq)]
pub enum EntryOut {
    /// Its entry in the folder's list, and its bytes, to its own hash.
    Whole(FolderFile, Zeroizing<Vec<u8>>),
    /// The Library keeps no such folder or file.
    Gone,
    /// It, or the folder's list, doesn't come to its hash.
    Damaged,
}

/// An item the Library keeps, as its index says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptItem {
    /// The broker's id for it: random hex.
    pub id: String,
    /// What it is: a file, kept before items had kinds, unless it says.
    #[serde(default)]
    pub kind: ItemKind,
    /// Its name, as the broker made it.
    pub name: String,
    /// Its length, all its files' together, in bytes.
    pub bytes: u64,
    /// How many files it is.
    #[serde(default = "one")]
    pub files: u64,
    /// A file's SHA-256, or a folder's list's, in lowercase hex.
    pub sha256: String,
    /// How many chunks its bytes are kept in.
    pub chunks: u64,
    /// How many chunks a folder's list is kept in: none for a file.
    #[serde(default)]
    pub list_chunks: u64,
    /// When it was kept.
    pub kept_at: String,
    /// The conversation it came from, by the Library's number.
    #[serde(default)]
    pub conversation: u64,
    /// The reply it came from: the chat run's cell id; none for an item
    /// kept before items were grouped.
    #[serde(default)]
    pub reply: String,
    /// What a folder's walk left out, by kind: nothing for a file.
    #[serde(default)]
    pub left_out: LeftOut,
}

/// One file of a folder, as its list says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderFile {
    /// Its path in the folder, as the broker took it.
    pub path: String,
    /// Its length, in bytes.
    pub bytes: u64,
    /// Its SHA-256, in lowercase hex.
    pub sha256: String,
}

const fn one() -> u64 {
    1
}

/// What a hand-over asks of the Library: a file, or a folder, of `files`
/// files and `bytes` in all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    /// What it is.
    pub kind: ItemKind,
    /// How many files.
    pub files: u64,
    /// How many bytes, all its files' together.
    pub bytes: u64,
}

impl Claim {
    /// A file of `bytes`.
    #[must_use]
    pub const fn file(bytes: u64) -> Self {
        Self {
            kind: ItemKind::File,
            files: 1,
            bytes,
        }
    }

    /// A folder of `files` files and `bytes` in all.
    #[must_use]
    pub const fn folder(files: u64, bytes: u64) -> Self {
        Self {
            kind: ItemKind::Folder,
            files,
            bytes,
        }
    }
}

/// A hand-over spent: on what, and in which conversation. Only
/// [`Library::spend`] makes one, and a keep takes it, once: it can't be
/// copied, so no spend keeps twice.
#[derive(Debug, PartialEq, Eq)]
pub struct Spent {
    claim: Claim,
    conversation: u64,
}

/// Why an item wasn't kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Handing over is off for this conversation: something handed over
    /// was found to hold a secret.
    Off,
    /// This conversation has handed over as many times as it may.
    Spent,
    /// This conversation has read as much out of the Room as it may.
    ReadOut,
    /// This is more than this conversation may still read out of the Room;
    /// something smaller may be.
    ReadPast,
    /// The Library holds as many items, bytes or files as it may.
    Full,
    /// This would take the Library past its bytes or files; something
    /// smaller may fit.
    WouldFill,
    /// The file is longer than the Library keeps.
    TooLarge,
    /// The folder is larger, or holds more files, than the Library keeps,
    /// or none.
    FolderTooLarge,
    /// The owner started a new conversation since the hand-over began.
    Stale,
}

impl Refused {
    /// Why, in words for the agent and the owner.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Off => {
                "handing over is off for this conversation: something was found to hold a secret Hatchling keeps. A new conversation turns it back on"
            }
            Self::Spent => {
                "this conversation has handed over 16 times, the most one may: a new conversation may hand over more"
            }
            Self::ReadOut => {
                "this conversation has read 128 MiB out of the agent's computer, the most one may: a new conversation may read more"
            }
            Self::ReadPast => {
                "this is more than this conversation may still read out of the agent's computer (128 MiB in all): something smaller may still be handed over, and a new conversation may read more"
            }
            Self::Full => {
                "the Library is full (64 items, 256 MiB or 4096 files in all): the owner removes items from it"
            }
            Self::WouldFill => {
                "this would take the Library past 256 MiB or 4096 files in all: something smaller may still fit, or the owner removes items from it"
            }
            Self::TooLarge => "the file is larger than 8 MiB, the most a file handed over may be",
            Self::FolderTooLarge => {
                "the folder is larger than 32 MiB, or holds more than 512 files, the most a folder handed over may, or holds none"
            }
            Self::Stale => {
                "the owner started a new conversation since this reply began: nothing from it is given"
            }
        }
    }
}

/// What is about to be done to an item: written before it is done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    op: Op,
    id: String,
    chunks: u64,
    #[serde(default)]
    list_chunks: u64,
}

/// Which change a pending one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Op {
    Keep,
    Remove,
}

/// The Library's state: what is about to be done, and the open
/// conversation's number, count, bytes read and off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    pending: Option<Pending>,
    handed: u32,
    off: bool,
    /// The open conversation, by a number the Library gives it: each new
    /// conversation is the next. A state kept before it had one reads as
    /// the first.
    #[serde(default)]
    conversation: u64,
    /// The bytes the open conversation has read out of the Room.
    #[serde(default)]
    read: u64,
    /// What was removed from the open conversation, oldest first: so its
    /// chat says so after a restart (WO-043).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed: Vec<Removed>,
}

/// An item removed from the open conversation, as its chat says it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Removed {
    /// The broker's id for it.
    pub id: String,
    /// What it was.
    pub kind: ItemKind,
    /// Its name, as the broker made it.
    pub name: String,
    /// How many files it was.
    pub files: u64,
    /// Its bytes, all its files' together.
    pub bytes: u64,
    /// The reply it came from: the chat run's cell id.
    pub reply: String,
}

/// Whether `removed` is what the Library can have removed from the open
/// conversation: of an item it could have kept, with a reply, each once.
fn is_removed(removed: &[Removed]) -> bool {
    let ids: HashSet<&str> = removed.iter().map(|gone| gone.id.as_str()).collect();
    removed.len() <= usize::try_from(HANDED_PER_CONVERSATION).unwrap_or(usize::MAX)
        && ids.len() == removed.len()
        && removed.iter().all(|gone| {
            is_id(&gone.id)
                && is_name(&gone.name)
                && !gone.reply.is_empty()
                && is_reply(&gone.reply)
                && match gone.kind {
                    ItemKind::File => gone.files == 1 && gone.bytes <= FILE_BYTES,
                    ItemKind::Folder => {
                        (1..=FOLDER_FILES).contains(&gone.files) && gone.bytes <= FOLDER_BYTES
                    }
                }
        })
}

/// How many chunks `bytes` are kept in.
fn chunks_of(bytes: u64) -> u64 {
    bytes.div_ceil(u64::try_from(CHUNK_BYTES).unwrap_or(u64::MAX))
}

/// The entry name of chunk `chunk` of item `id`'s bytes.
fn chunk_name(id: &str, chunk: u64) -> String {
    format!("library-{id}-{chunk}")
}

/// The entry name of chunk `chunk` of folder `id`'s list.
fn list_chunk_name(id: &str, chunk: u64) -> String {
    format!("library-{id}-l{chunk}")
}

/// `bytes` as lowercase hex.
fn hex(bytes: &[u8]) -> String {
    crate::approval::hex_token("", bytes)
}

/// Whether `kept_at` can be a time an item was kept: at most
/// [`KEPT_AT_BYTES`] of printable ASCII, so the index's length is known.
fn is_kept_at(kept_at: &str) -> bool {
    kept_at.len() <= KEPT_AT_BYTES && kept_at.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether `id` is one the broker gives: 64 lowercase hex digits.
fn is_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether `hash` is a SHA-256 in lowercase hex.
fn is_hash(hash: &str) -> bool {
    is_id(hash)
}

/// Whether `item` can be one an index names: an id the broker gives, a
/// name the Library keeps, a hash, a time and a reply of their shapes, and
/// within its kind's bounds, its chunks those its lengths are kept in.
fn is_item(item: &KeptItem) -> bool {
    let shaped = is_id(&item.id)
        && is_name(&item.name)
        && is_hash(&item.sha256)
        && is_kept_at(&item.kept_at)
        && is_reply(&item.reply)
        && item.conversation <= CONVERSATIONS
        && item.chunks == chunks_of(item.bytes);
    shaped
        && match item.kind {
            ItemKind::File => {
                item.files == 1
                    && item.list_chunks == 0
                    && item.bytes <= FILE_BYTES
                    && item.left_out == LeftOut::default()
            }
            ItemKind::Folder => {
                (1..=FOLDER_FILES).contains(&item.files)
                    && item.bytes <= FOLDER_BYTES
                    && (1..=LIST_CHUNKS).contains(&item.list_chunks)
            }
        }
}

/// The Library's items and the open conversation's standing, as they
/// were at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shelf {
    /// Its items, oldest first.
    pub items: Vec<KeptItem>,
    /// The open conversation's hand-overs.
    pub handed: u32,
    /// Whether handing over is off for it.
    pub off: bool,
    /// What it has read out of the Room, in bytes.
    pub read: u64,
    /// Its number.
    pub conversation: u64,
    /// What was removed from it, oldest first.
    pub removed: Vec<Removed>,
}

/// The conversation after `conversation`: refused past
/// [`CONVERSATIONS`], never wrapped, so no two are ever one.
fn next(conversation: u64) -> Result<u64, String> {
    conversation
        .checked_add(1)
        .filter(|next| *next <= CONVERSATIONS)
        .ok_or_else(|| String::from("the Library has numbered all the conversations it can"))
}

/// What `index` holds in all: its bytes and its files.
fn held(index: &[KeptItem]) -> (u64, u64) {
    index.iter().fold((0_u64, 0_u64), |(bytes, files), item| {
        (
            bytes.saturating_add(item.bytes),
            files.saturating_add(item.files),
        )
    })
}

/// Writes `parts`, back to back, as the chunks of item `id`'s bytes,
/// each named by `name`: every chunk whole but the last.
fn put_run(
    vault: &SealedVault<'_>,
    id: &str,
    parts: &[&[u8]],
    name: fn(&str, u64) -> String,
) -> Result<(), String> {
    // As many chunks as the bytes are kept in, each taken in turn from
    // the parts as one run: nothing here can go round for ever.
    let total = parts.iter().fold(0_u64, |all, part| {
        all.saturating_add(u64::try_from(part.len()).unwrap_or(u64::MAX))
    });
    let mut bytes = parts.iter().flat_map(|part| part.iter().copied());
    for chunk in 0..chunks_of(total) {
        let piece: Vec<u8> = bytes.by_ref().take(CHUNK_BYTES).collect();
        Library::put(vault, &name(id, chunk), piece)?;
    }
    Ok(())
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
        let secret = Secret::new(bytes)
            .map_err(|error| format!("the Library's {entry} can't be kept: {error}"))?;
        vault
            .store(&Self::name(entry)?, &secret)
            .map_err(|error| format!("the Library's {entry} can't be written: {error}"))
    }

    /// Writes one chunk, quietly: the item is logged once, not each chunk.
    fn put(vault: &SealedVault<'_>, entry: &str, bytes: Vec<u8>) -> Result<(), String> {
        let secret = Secret::new(bytes)
            .map_err(|error| format!("the Library's chunk can't be kept: {error}"))?;
        vault
            .store_quietly(&Self::name(entry)?, &secret)
            .map_err(|error| format!("the Library's chunk can't be written: {error}"))
    }

    /// The index, held to what one can be: at most [`LIBRARY_ITEMS`] items,
    /// each one an index can name ([`is_item`]), within the Library's
    /// bytes and files in all.
    fn index(vault: &SealedVault<'_>) -> Result<Vec<KeptItem>, String> {
        let index: Vec<KeptItem> = Self::read(vault, INDEX)?.unwrap_or_default();
        let (bytes, files) = held(&index);
        let sound = index.len() <= LIBRARY_ITEMS
            && index.iter().all(is_item)
            && bytes <= LIBRARY_BYTES
            && files <= LIBRARY_FILES;
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
            || state.read > READ_PER_CONVERSATION
            || state.conversation > CONVERSATIONS
            || state.pending.as_ref().is_some_and(|pending| {
                !is_id(&pending.id)
                    || pending.chunks > chunks_of(FOLDER_BYTES)
                    || pending.list_chunks > LIST_CHUNKS
            })
        {
            return Err(String::from("the Library's state holds what a state can't"));
        }
        // What was removed only says so in the chat: one that can't be is
        // dropped, never the Library's undoing.
        let mut state = state;
        if !is_removed(&state.removed) {
            tracing::warn!("the Library's removed items can't be what was removed: dropped");
            state.removed.clear();
        }
        Ok(state)
    }

    /// Removes chunks `0..chunks` of item `id`'s bytes and `0..list_chunks`
    /// of its list, quietly: one already gone is gone.
    fn remove_chunks(
        vault: &SealedVault<'_>,
        id: &str,
        chunks: u64,
        list_chunks: u64,
    ) -> Result<(), String> {
        let names = (0..chunks)
            .map(|chunk| chunk_name(id, chunk))
            .chain((0..list_chunks).map(|chunk| list_chunk_name(id, chunk)));
        for name in names {
            match vault.remove_quietly(&Self::name(&name)?) {
                Ok(()) | Err(hatchling_vault::Error::NotFound { .. }) => {}
                Err(error) => {
                    return Err(format!("the Library's item can't be removed: {error}"));
                }
            }
        }
        Ok(())
    }

    /// Finishes or undoes what a crash, or a failed write, left half done:
    /// a keep whose item the index names is done; any other keep is
    /// undone, its chunks removed; a removal is finished. The state then
    /// has nothing pending.
    fn recover(vault: &SealedVault<'_>) -> Result<State, String> {
        let mut state = Self::state(vault)?;
        let Some(pending) = state.pending.take() else {
            return Ok(state);
        };
        let mut index = Self::index(vault)?;
        let named = index.iter().any(|item| item.id == pending.id);
        match pending.op {
            Op::Keep if named => {}
            Op::Keep => {
                Self::remove_chunks(vault, &pending.id, pending.chunks, pending.list_chunks)?;
                tracing::info!(item = %pending.id, "the Library undid an item a keep left half done");
            }
            Op::Remove => {
                if named {
                    index.retain(|item| item.id != pending.id);
                    Self::write(vault, INDEX, &index)?;
                }
                Self::remove_chunks(vault, &pending.id, pending.chunks, pending.list_chunks)?;
                tracing::info!(item = %pending.id, "the Library finished removing an item");
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

    /// Why `claim` wouldn't be kept now, if it wouldn't: what is asked
    /// before anything is read out of the Room (a file's length isn't
    /// known yet, so it is asked as of none). [`Self::spend`] asks again.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn refuses(&self, claim: &Claim) -> Result<Option<Refused>, String> {
        self.opened(|vault, state| Ok(Self::refusal(&Self::index(vault)?, &state, claim, false)))
    }

    /// Why `claim` wouldn't be kept, by `index` and `state`. Once `spent`,
    /// its hand-over and its bytes are counted already: it is held to every
    /// other bound.
    fn refusal(index: &[KeptItem], state: &State, claim: &Claim, spent: bool) -> Option<Refused> {
        let (bytes, files) = held(index);
        let too_large = match claim.kind {
            ItemKind::File => (claim.bytes > FILE_BYTES).then_some(Refused::TooLarge),
            ItemKind::Folder => {
                (claim.bytes > FOLDER_BYTES || claim.files > FOLDER_FILES || claim.files == 0)
                    .then_some(Refused::FolderTooLarge)
            }
        };
        if too_large.is_some() {
            too_large
        } else if state.off {
            Some(Refused::Off)
        } else if !spent && state.handed >= HANDED_PER_CONVERSATION {
            Some(Refused::Spent)
        } else if !spent && state.read >= READ_PER_CONVERSATION {
            Some(Refused::ReadOut)
        } else if !spent && state.read.saturating_add(claim.bytes) > READ_PER_CONVERSATION {
            Some(Refused::ReadPast)
        } else if index.len() >= LIBRARY_ITEMS || bytes >= LIBRARY_BYTES || files >= LIBRARY_FILES {
            Some(Refused::Full)
        } else if bytes.saturating_add(claim.bytes) > LIBRARY_BYTES
            || files.saturating_add(claim.files) > LIBRARY_FILES
        {
            Some(Refused::WouldFill)
        } else {
            None
        }
    }

    /// The open conversation's number: a hand-over is held to it from
    /// before anything is read until it is kept.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn conversation(&self) -> Result<u64, String> {
        self.opened(|_, state| Ok(state.conversation))
    }

    /// Spends one of `conversation`'s hand-overs on `claim`, sealed, before
    /// it is checked for secrets: each check is a guess at a secret, and
    /// every one is counted, whatever comes of it. Refused, and no
    /// hand-over spent, when it wouldn't be kept, or the conversation is no
    /// longer the open one.
    ///
    /// A file is spent once it is read: its bytes count toward the
    /// conversation's, refused or not (all but a stale one's), up to the
    /// most. A folder is spent before it is read, its bytes as its listing
    /// says them, and counted only when it is spent.
    ///
    /// What it gives is what the keep takes: it keeps only what was spent.
    ///
    /// # Errors
    ///
    /// When the Library can't be read or written.
    pub fn spend(
        &self,
        claim: &Claim,
        conversation: u64,
    ) -> Result<Result<Spent, Refused>, String> {
        self.opened(|vault, mut state| {
            if state.conversation != conversation {
                return Ok(Err(Refused::Stale));
            }
            let refused = Self::refusal(&Self::index(vault)?, &state, claim, false);
            let read = state
                .read
                .saturating_add(claim.bytes)
                .min(READ_PER_CONVERSATION);
            match (refused, claim.kind) {
                (Some(refused), ItemKind::File) => {
                    state.read = read;
                    Self::write(vault, STATE, &state)?;
                    // Its bytes were read all the same: once they take the
                    // conversation to the most, nothing smaller fits either.
                    let spent_out = read >= READ_PER_CONVERSATION
                        && matches!(refused, Refused::ReadPast | Refused::WouldFill);
                    Ok(Err(if spent_out { Refused::ReadOut } else { refused }))
                }
                (Some(refused), ItemKind::Folder) => Ok(Err(refused)),
                (None, _) => {
                    state.read = read;
                    state.handed = state.handed.saturating_add(1);
                    Self::write(vault, STATE, &state)?;
                    Ok(Ok(Spent {
                        claim: *claim,
                        conversation,
                    }))
                }
            }
        })
    }

    /// Counts `bytes` more read out of the Room toward `conversation`'s, up
    /// to the most: what a folder's file read past the length it was spent
    /// on. Nothing for a conversation no longer the open one.
    ///
    /// # Errors
    ///
    /// When the Library can't be read or written.
    pub fn read_more(&self, bytes: u64, conversation: u64) -> Result<(), String> {
        self.opened(|vault, mut state| {
            if state.conversation != conversation {
                return Ok(());
            }
            state.read = state.read.saturating_add(bytes).min(READ_PER_CONVERSATION);
            Self::write(vault, STATE, &state)
        })
    }

    /// Keeps `bytes` as the file `name`, at `now`, spending one of the open
    /// conversation's hand-overs: the item as the index says it, or why it
    /// wasn't kept. For tests: a hand-over is spent before its file is
    /// checked, and kept with [`Self::keep_spent`].
    ///
    /// # Errors
    ///
    /// As [`Self::keep_spent`].
    #[cfg(test)]
    pub(crate) fn keep(
        &self,
        name: &str,
        bytes: &[u8],
        now: &str,
    ) -> Result<Result<KeptItem, Refused>, String> {
        if !is_name(name) {
            return Err(String::from(
                "the Library doesn't keep an item by that name",
            ));
        }
        let conversation = self.conversation()?;
        let claim = Claim::file(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let spent = match self.spend(&claim, conversation)? {
            Ok(spent) => spent,
            Err(refused) => return Ok(Err(refused)),
        };
        self.keep_spent(name, bytes, now, spent, tests::REPLY)
    }

    /// Keeps `bytes` as the file `name`, at `now`, the hand-over `spent` in
    /// the reply `reply`: refused when the conversation it was spent in is
    /// no longer the open one, handing over was turned off since, or the
    /// Library has no room.
    ///
    /// # Errors
    ///
    /// When `name` isn't one the Library keeps an item by, `reply` isn't a
    /// run's cell id, what is kept isn't what was spent, or the Library
    /// can't be read or written before the item is in its index. A keep that fails part way is undone by the
    /// next operation, and its hand-over stays spent; one whose item is in
    /// the index is kept, whatever fails after.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "a spend's receipt is taken, so one spend keeps once"
    )]
    pub fn keep_spent(
        &self,
        name: &str,
        bytes: &[u8],
        now: &str,
        spent: Spent,
        reply: &str,
    ) -> Result<Result<KeptItem, Refused>, String> {
        let claim = Claim::file(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let sha256 = hex(&Sha256::digest(bytes));
        self.keeping(
            name,
            &claim,
            &[bytes],
            None,
            LeftOut::default(),
            sha256,
            now,
            &spent,
            reply,
        )
    }

    /// Keeps `files`, each a path and its bytes, as the folder `name`,
    /// with what its walk `left_out`, at `now`, the hand-over `spent` in the reply `reply`: refused as
    /// [`Self::keep_spent`] is, and when its paths aren't a folder's
    /// ([`check_paths`]). Its paths are to be checked before it is spent,
    /// too, as its listing gives them: here they are checked again.
    ///
    /// # Errors
    ///
    /// As [`Self::keep_spent`], and which rule a path broke: never the
    /// path.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "a spend's receipt is taken, so one spend keeps once"
    )]
    pub fn keep_folder_spent(
        &self,
        name: &str,
        files: &[(&str, &[u8])],
        left_out: LeftOut,
        now: &str,
        spent: Spent,
        reply: &str,
    ) -> Result<Result<KeptItem, Refused>, String> {
        check_paths(files.iter().map(|(path, _)| *path)).map_err(String::from)?;
        if files
            .iter()
            .any(|(_, bytes)| u64::try_from(bytes.len()).unwrap_or(u64::MAX) > FILE_BYTES)
        {
            return Err(String::from("a file in a folder is larger than 8 MiB"));
        }
        let list: Vec<FolderFile> = files
            .iter()
            .map(|(path, bytes)| FolderFile {
                path: (*path).to_owned(),
                bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                sha256: hex(&Sha256::digest(bytes)),
            })
            .collect();
        let claim = Claim::folder(
            u64::try_from(list.len()).unwrap_or(u64::MAX),
            list.iter()
                .fold(0_u64, |all, file| all.saturating_add(file.bytes)),
        );
        let list = serde_json::to_vec(&list)
            .map_err(|error| format!("the Library's list doesn't encode: {error}"))?;
        let sha256 = hex(&Sha256::digest(&list));
        let parts: Vec<&[u8]> = files.iter().map(|(_, bytes)| *bytes).collect();
        self.keeping(
            name,
            &claim,
            &parts,
            Some(&list),
            left_out,
            sha256,
            now,
            &spent,
            reply,
        )
    }

    /// [`Self::keep_spent`] and [`Self::keep_folder_spent`]: `parts`, back
    /// to back, kept as the item `claim` says, with a folder's `list`.
    #[allow(
        clippy::too_many_arguments,
        reason = "what an item is kept as, each the broker's"
    )]
    fn keeping(
        &self,
        name: &str,
        claim: &Claim,
        parts: &[&[u8]],
        list: Option<&[u8]>,
        left_out: LeftOut,
        sha256: String,
        now: &str,
        spent: &Spent,
        reply: &str,
    ) -> Result<Result<KeptItem, Refused>, String> {
        let Spent {
            claim: spent_on,
            conversation,
        } = *spent;
        if spent_on != *claim {
            return Err(String::from("what is kept isn't what was spent"));
        }
        if !is_name(name) {
            return Err(String::from(
                "the Library doesn't keep an item by that name",
            ));
        }
        // An item kept now names its reply: none is only an old one's.
        if reply.is_empty() || !is_reply(reply) || !is_kept_at(now) {
            return Err(String::from(
                "the Library is told of a reply, or a time, that can't be one",
            ));
        }
        let list_bytes = list.map_or(0, <[u8]>::len);
        if u64::try_from(list_bytes).unwrap_or(u64::MAX) > LIST_BYTES {
            return Err(String::from("the folder's list is longer than one may be"));
        }
        let id = crate::approval::random_hex("")
            .map_err(|error| format!("the Library can't name an item: {error}"))?;
        self.opened(|vault, mut state| {
            let mut index = Self::index(vault)?;
            if conversation != state.conversation {
                return Ok(Err(Refused::Stale));
            }
            if let Some(refused) = Self::refusal(&index, &state, claim, true) {
                return Ok(Err(refused));
            }
            let chunks = chunks_of(claim.bytes);
            let list_chunks = chunks_of(u64::try_from(list_bytes).unwrap_or(u64::MAX));
            state.pending = Some(Pending {
                op: Op::Keep,
                id: id.clone(),
                chunks,
                list_chunks,
            });
            Self::write(vault, STATE, &state)?;
            put_run(vault, &id, parts, chunk_name)?;
            if let Some(list) = list {
                put_run(vault, &id, &[list], list_chunk_name)?;
            }
            let kept = KeptItem {
                id,
                kind: claim.kind,
                name: name.to_owned(),
                bytes: claim.bytes,
                files: claim.files,
                sha256,
                chunks,
                list_chunks,
                kept_at: now.to_owned(),
                conversation,
                reply: reply.to_owned(),
                left_out,
            };
            index.push(kept.clone());
            if let Err(error) = Self::write(vault, INDEX, &index) {
                // A write can fail after its rename: what the index says
                // now is whether the item was kept.
                let named = Self::index(vault)
                    .is_ok_and(|index| index.iter().any(|item| item.id == kept.id));
                if !named {
                    return Err(error);
                }
            }
            tracing::info!(
                item = %kept.id,
                kind = ?kept.kind,
                files = kept.files,
                bytes = kept.bytes,
                "the Library kept an item"
            );
            // Kept. What is left pending, if this write fails, the next
            // operation finishes.
            state.pending = None;
            let _ = Self::write(vault, STATE, &state);
            Ok(Ok(kept))
        })
    }

    /// Turns handing over off for `conversation`: nothing, when the owner
    /// has started a new one since, which an old run's hand-over doesn't
    /// turn off.
    ///
    /// # Errors
    ///
    /// When the Library can't be read or written.
    pub fn turn_off_in(&self, conversation: u64) -> Result<(), String> {
        self.opened(|vault, mut state| {
            if state.conversation != conversation {
                return Ok(());
            }
            state.off = true;
            Self::write(vault, STATE, &state)
        })
    }

    /// Turns handing over off for the open conversation.
    #[cfg(test)]
    pub(crate) fn turn_off(&self) -> Result<(), String> {
        self.turn_off_in(self.conversation()?)
    }

    /// The owner's new conversation, begun: the next number, with handing
    /// over **off** until [`Self::finish_new_conversation`], so no run of
    /// the last conversation gives anything while the chat moves on. Its
    /// number.
    ///
    /// # Errors
    ///
    /// When the Library can't be read or written.
    pub fn begin_new_conversation(&self) -> Result<u64, String> {
        self.opened(|vault, mut state| {
            state.off = true;
            state.conversation = next(state.conversation)?;
            state.removed.clear();
            Self::write(vault, STATE, &state)?;
            Ok(state.conversation)
        })
    }

    /// The owner's new conversation `conversation`, once the chat moved on
    /// to it: its count and bytes start again, and handing over is on.
    /// Nothing for any other conversation.
    ///
    /// # Errors
    ///
    /// When the Library can't be read or written: handing over stays off.
    pub fn finish_new_conversation(&self, conversation: u64) -> Result<(), String> {
        self.opened(|vault, mut state| {
            if state.conversation != conversation {
                return Ok(());
            }
            state.handed = 0;
            state.read = 0;
            state.off = false;
            Self::write(vault, STATE, &state)
        })
    }

    /// A new conversation started: its count and bytes start again, and
    /// handing over is on. Both of the owner's steps at once.
    ///
    /// # Errors
    ///
    /// When the Library can't be read or written.
    pub fn new_conversation(&self) -> Result<(), String> {
        self.opened(|vault, mut state| {
            state.handed = 0;
            state.read = 0;
            state.off = false;
            state.conversation = next(state.conversation)?;
            state.removed.clear();
            Self::write(vault, STATE, &state)
        })
    }

    /// The conversation the chat started without the owner (a restart's):
    /// its items are grouped apart, by the next number, and nothing else
    /// changes. A hand-over begun before it is stale.
    ///
    /// # Errors
    ///
    /// When the Library can't be read or written.
    pub fn regroup(&self) -> Result<(), String> {
        self.opened(|vault, mut state| {
            state.conversation = next(state.conversation)?;
            state.removed.clear();
            Self::write(vault, STATE, &state)
        })
    }

    /// The open conversation's count of hand-overs, and whether handing
    /// over is off for it.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn standing(&self) -> Result<(u32, bool), String> {
        self.opened(|_, state| Ok((state.handed, state.off)))
    }

    /// What the open conversation has read out of the Room, in bytes.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn read_so_far(&self) -> Result<u64, String> {
        self.opened(|_, state| Ok(state.read))
    }

    /// The items the Library keeps, oldest first, and the open
    /// conversation's standing, read at once.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn shelf(&self) -> Result<Shelf, String> {
        self.opened(|vault, state| {
            let items = Self::index(vault)?;
            // What was removed, unless the index names it still: an item
            // is never both.
            let removed = state
                .removed
                .into_iter()
                .filter(|gone| !items.iter().any(|item| item.id == gone.id))
                .collect();
            Ok(Shelf {
                items,
                handed: state.handed,
                off: state.off,
                read: state.read,
                conversation: state.conversation,
                removed,
            })
        })
    }

    /// The items the Library keeps, oldest first.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn items(&self) -> Result<Vec<KeptItem>, String> {
        self.opened(|vault, _| Self::index(vault))
    }

    /// The files the Library keeps, oldest first: its items that are files.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn files(&self) -> Result<Vec<KeptItem>, String> {
        let mut items = self.items()?;
        items.retain(|item| item.kind == ItemKind::File);
        Ok(items)
    }

    /// The file `id`, whole, as it was kept; `None` when the Library keeps
    /// no such file.
    ///
    /// # Errors
    ///
    /// When the Library can't be read, or the file's chunks don't come to
    /// its hash.
    pub fn fetch(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
        match self.read_out(id)? {
            ReadOut::Whole(mut bytes) => Ok(Some(std::mem::take(&mut *bytes))),
            ReadOut::Gone => Ok(None),
            ReadOut::Damaged => Err(format!("the Library's file {id} is damaged")),
        }
    }

    /// Reads `bytes` of item `id`'s bytes from byte `from`, named by
    /// `name`, chunk by chunk, never grown past `bytes`: `None` when a
    /// chunk can't be read, or holds less than its place. A chunk holding
    /// more is read only to its place; the hashes say whether it was
    /// whole.
    fn read_run(
        vault: &SealedVault<'_>,
        id: &str,
        from: u64,
        bytes: u64,
        name: fn(&str, u64) -> String,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        let chunk_bytes = u64::try_from(CHUNK_BYTES).unwrap_or(u64::MAX);
        let mut out = Zeroizing::new(Vec::with_capacity(usize::try_from(bytes).unwrap_or(0)));
        let mut at = from;
        let end = from.saturating_add(bytes);
        while at < end {
            let chunk = at / chunk_bytes;
            let Ok(secret) = vault.fetch(&Self::name(&name(id, chunk))?) else {
                return Ok(None);
            };
            let start = usize::try_from(at - chunk * chunk_bytes).unwrap_or(usize::MAX);
            let want = usize::try_from((end - at).min(chunk_bytes)).unwrap_or(usize::MAX);
            let Some(piece) = secret
                .as_bytes()
                .get(start..)
                .map(|rest| rest.get(..want.min(rest.len())).unwrap_or(rest))
            else {
                return Ok(None);
            };
            if piece.is_empty() {
                return Ok(None);
            }
            out.extend_from_slice(piece);
            at = at.saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
        }
        Ok(Some(out))
    }

    /// The file `id`, whole, as it was kept; or that the Library keeps no
    /// such file (a folder is no file), or that its copy is damaged.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn read_out(&self, id: &str) -> Result<ReadOut, String> {
        self.opened(|vault, _| {
            let index = Self::index(vault)?;
            let Some(file) = index
                .iter()
                .find(|item| item.id == id && item.kind == ItemKind::File)
            else {
                return Ok(ReadOut::Gone);
            };
            // The hash is the index's, sealed: chunks of another item, cut
            // short or out of order come to another hash.
            Ok(
                match Self::read_run(vault, &file.id, 0, file.bytes, chunk_name)? {
                    Some(bytes) if hex(&Sha256::digest(&*bytes)) == file.sha256 => {
                        ReadOut::Whole(bytes)
                    }
                    _ => ReadOut::Damaged,
                },
            )
        })
    }

    /// Folder `item`'s list, to the index's hash of it, as its files can
    /// be: their lengths come to the item's, their count is its, and their
    /// paths a folder's.
    fn list_in(vault: &SealedVault<'_>, item: &KeptItem) -> Result<ListOut, String> {
        let most = item
            .list_chunks
            .saturating_mul(u64::try_from(CHUNK_BYTES).unwrap_or(u64::MAX))
            .min(LIST_BYTES);
        let Some(bytes) = Self::read_run_whole(vault, &item.id, item.list_chunks, most)? else {
            return Ok(ListOut::Damaged);
        };
        if hex(&Sha256::digest(&*bytes)) != item.sha256 {
            return Ok(ListOut::Damaged);
        }
        let Ok(files) = serde_json::from_slice::<Vec<FolderFile>>(&bytes) else {
            return Ok(ListOut::Damaged);
        };
        let total = files
            .iter()
            .try_fold(0_u64, |all, file| all.checked_add(file.bytes));
        let sound = total == Some(item.bytes)
            && u64::try_from(files.len()).ok() == Some(item.files)
            && files
                .iter()
                .all(|file| is_hash(&file.sha256) && file.bytes <= FILE_BYTES)
            && check_paths(files.iter().map(|file| file.path.as_str())).is_ok();
        Ok(if sound {
            ListOut::Whole(files)
        } else {
            ListOut::Damaged
        })
    }

    /// The `chunks` chunks of item `id`'s list, whole, at most `most`
    /// bytes: `None` when one can't be read or they come to more.
    fn read_run_whole(
        vault: &SealedVault<'_>,
        id: &str,
        chunks: u64,
        most: u64,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        let mut out = Zeroizing::new(Vec::new());
        for chunk in 0..chunks {
            let Ok(secret) = vault.fetch(&Self::name(&list_chunk_name(id, chunk))?) else {
                return Ok(None);
            };
            if u64::try_from(out.len().saturating_add(secret.as_bytes().len())).unwrap_or(u64::MAX)
                > most
            {
                return Ok(None);
            }
            out.extend_from_slice(secret.as_bytes());
        }
        Ok(Some(out))
    }

    /// Folder `id`'s list of its files; or that the Library keeps no such
    /// folder, or that its list is damaged.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn list_of(&self, id: &str) -> Result<ListOut, String> {
        self.opened(|vault, _| {
            let index = Self::index(vault)?;
            index
                .iter()
                .find(|item| item.id == id && item.kind == ItemKind::Folder)
                .map_or(Ok(ListOut::Gone), |folder| Self::list_in(vault, folder))
        })
    }

    /// File `n` of folder `id`, counted from 0, whole, to its own hash; or
    /// that the Library keeps no such folder or file, or that it, or the
    /// folder's list, is damaged.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn read_out_in(&self, id: &str, n: usize) -> Result<ReadOut, String> {
        Ok(match self.entry_out(id, n)? {
            EntryOut::Whole(_, bytes) => ReadOut::Whole(bytes),
            EntryOut::Gone => ReadOut::Gone,
            EntryOut::Damaged => ReadOut::Damaged,
        })
    }

    /// File `n` of folder `id`, counted from 0: its entry in the folder's
    /// list and its bytes, whole and to its own hash, read at one moment;
    /// or that the Library keeps no such folder or file, or that it, or
    /// the folder's list, is damaged.
    ///
    /// # Errors
    ///
    /// When the Library can't be read.
    pub fn entry_out(&self, id: &str, n: usize) -> Result<EntryOut, String> {
        self.opened(|vault, _| {
            let index = Self::index(vault)?;
            let Some(folder) = index
                .iter()
                .find(|item| item.id == id && item.kind == ItemKind::Folder)
            else {
                return Ok(EntryOut::Gone);
            };
            let files = match Self::list_in(vault, folder)? {
                ListOut::Whole(files) => files,
                ListOut::Gone => return Ok(EntryOut::Gone),
                ListOut::Damaged => return Ok(EntryOut::Damaged),
            };
            let Some(file) = files.get(n) else {
                return Ok(EntryOut::Gone);
            };
            let from = files
                .iter()
                .take(n)
                .fold(0_u64, |at, before| at.saturating_add(before.bytes));
            Ok(
                match Self::read_run(vault, &folder.id, from, file.bytes, chunk_name)? {
                    Some(bytes) if hex(&Sha256::digest(&*bytes)) == file.sha256 => {
                        EntryOut::Whole(file.clone(), bytes)
                    }
                    _ => EntryOut::Damaged,
                },
            )
        })
    }

    /// Writes an index of file items of `lengths`, by its own word: none of
    /// their bytes are kept. For a Library near its bounds.
    #[cfg(test)]
    pub(crate) fn pretend_held(&self, lengths: &[u64]) {
        let index: Vec<KeptItem> = lengths
            .iter()
            .enumerate()
            .map(|(at, &bytes)| KeptItem {
                id: format!("{at:064x}"),
                kind: ItemKind::File,
                name: format!("held-{at}.bin"),
                bytes,
                files: 1,
                sha256: format!("{:064x}", 0),
                chunks: chunks_of(bytes),
                list_chunks: 0,
                kept_at: String::from("2026-11-17T00:00:00Z"),
                conversation: 0,
                reply: String::new(),
                left_out: LeftOut::default(),
            })
            .collect();
        self.pretend_index(&index);
    }

    /// Writes `index` as the Library's, by its own word.
    #[cfg(test)]
    pub(crate) fn pretend_index<T: Serialize>(&self, index: &T) {
        let vault = self.vault().unwrap_or_else(|error| unreachable!("{error}"));
        Self::write(&vault, INDEX, index).unwrap_or_else(|error| unreachable!("{error}"));
    }

    /// Puts junk of the same length in place of chunk `chunk` of item
    /// `id`'s bytes, or of its list when `list`: it reads back damaged.
    #[cfg(test)]
    pub(crate) fn damage_chunk(&self, id: &str, chunk: u64, list: bool) {
        let vault = self.vault().unwrap_or_else(|error| unreachable!("{error}"));
        let raw = if list {
            list_chunk_name(id, chunk)
        } else {
            chunk_name(id, chunk)
        };
        let name = Self::name(&raw).unwrap_or_else(|error| unreachable!("{error}"));
        let kept = vault
            .fetch(&name)
            .unwrap_or_else(|error| unreachable!("{error}"));
        let junk = vec![b'?'; kept.as_bytes().len()];
        vault
            .store(
                &name,
                &Secret::new(junk).unwrap_or_else(|error| unreachable!("{error}")),
            )
            .unwrap_or_else(|error| unreachable!("{error}"));
    }

    /// Puts junk in place of the first chunk of the file `id`.
    #[cfg(test)]
    pub(crate) fn damage(&self, id: &str) {
        self.damage_chunk(id, 0, false);
    }

    /// Removes the item `id`: whether the Library kept it.
    ///
    /// # Errors
    ///
    /// When the Library can't be read, or the removal can't be written
    /// as pending. Once it is, the item is removed: what fails after, the
    /// next operation finishes.
    pub fn remove(&self, id: &str) -> Result<bool, String> {
        self.opened(|vault, mut state| {
            let mut index = Self::index(vault)?;
            let Some(item) = index.iter().find(|item| item.id == id).cloned() else {
                return Ok(false);
            };
            let pending = Pending {
                op: Op::Remove,
                id: item.id.clone(),
                chunks: item.chunks,
                list_chunks: item.list_chunks,
            };
            state.pending = Some(pending.clone());
            // What the open conversation gave, its chat says was removed:
            // written with the removal, so it is said if the removal is
            // made. Never for an item kept before items had replies; past
            // the most, the oldest goes.
            if item.conversation == state.conversation && !item.reply.is_empty() {
                let most = usize::try_from(HANDED_PER_CONVERSATION).unwrap_or(usize::MAX);
                if state.removed.len() >= most {
                    state.removed.remove(0);
                }
                state.removed.push(Removed {
                    id: item.id.clone(),
                    kind: item.kind,
                    name: item.name.clone(),
                    files: item.files,
                    bytes: item.bytes,
                    reply: item.reply.clone(),
                });
            }
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
            index.retain(|kept| kept.id != item.id);
            if Self::write(vault, INDEX, &index).is_ok()
                && Self::remove_chunks(vault, &item.id, item.chunks, item.list_chunks).is_ok()
            {
                state.pending = None;
                let _ = Self::write(vault, STATE, &state);
            }
            tracing::info!(
                item = %item.id,
                kind = ?item.kind,
                files = item.files,
                bytes = item.bytes,
                "the Library removed an item"
            );
            Ok(true)
        })
    }
}

/// Groups what is handed over from now apart, at the broker's start: once
/// for each `once`, which the broker keeps for its life.
///
/// Unless the conversation kept from before is `resumed`: one the chat
/// doesn't resume is a new one to the owner, and its items aren't grouped
/// with the last. Its count, its bytes and its off are carried: only the
/// owner's new conversation clears them, never a restart.
pub fn at_start(once: &Once, config: &Config, keys: &dyn OpensStores, resumed: bool) {
    once.call_once(|| start_unless(config, keys, resumed));
}

/// [`at_start`]'s work, each time it is asked.
fn start_unless(config: &Config, keys: &dyn OpensStores, resumed: bool) {
    if resumed {
        return;
    }
    if let Some(library) = Library::of(config, keys) {
        if let Err(error) = library.regroup() {
            tracing::error!(%error, "the Library couldn't group a new conversation apart");
        }
    }
}

#[cfg(test)]
#[path = "library/tests.rs"]
pub(crate) mod tests;
