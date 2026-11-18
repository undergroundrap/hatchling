//! Files the owner hands the agent (WO-041): sent from the app in parts,
//! and put in the inbox of the agent's computer, the Room, by root.
//!
//! The owner's name for a file is untrusted text. It is shown in the app,
//! never used on disk, and never given to the model: the inbox names the
//! file `NNNN-<stem>`, from a stem the broker makes of it.

use serde::{Deserialize, Serialize};

/// The most bytes a file sent in takes: the inbox's bound for one file.
pub const FILE_IN_BYTES: u64 = 8 * 1024 * 1024;

/// The most bytes one part of a file carries, before base64: a part, as
/// JSON, fits one 64 KiB frame with room to spare.
pub const FILE_IN_PART_BYTES: u32 = 32 * 1024;

/// The longest owner's name for a file the app is shown back, in bytes:
/// a longer one is cut at a character's end.
pub const FILE_IN_NAME_BYTES: usize = 255;

/// Why a file wasn't taken in, or the inbox wasn't listed or changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum FileInRefusal {
    /// Larger than the inbox takes.
    TooLarge {
        /// The most it takes.
        limit: u64,
    },
    /// Another file is on its way in: one at a time.
    Busy,
    /// The agent's computer isn't on: no Room disk is open.
    NoRoom,
    /// No file of that id is on its way in on this connection.
    UnknownUpload,
    /// A part out of order, larger than a part, not base64, or past the
    /// size first said; or an end before the whole file. Nothing is put,
    /// and the file is sent again from its beginning.
    OutOfStep,
    /// The inbox refused it, in its own words.
    Refused {
        /// Why, as the owner is told.
        said: String,
    },
    /// It isn't known whether the file was put: the inbox's list says.
    /// It isn't sent again.
    Unknown,
}

/// A file in the Room's inbox, as the owner is shown it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InboxFile {
    /// Its name in the inbox, `NNNN-<stem>`: the agent reads it at
    /// `/workspace/inbox/<name>`.
    pub name: String,
    /// Its size.
    pub bytes: u64,
}

/// A file the owner put in the Room's inbox, as the chat shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileIn {
    /// The chat it was sent in.
    pub chat_id: String,
    /// Its place in the chat.
    pub message_id: String,
    /// The owner's name for it, as the app sent it, cut to
    /// [`FILE_IN_NAME_BYTES`]. Untrusted text: shown as text, never as a
    /// path or markup.
    pub name: String,
    /// Its name in the inbox.
    pub placed_as: String,
    /// Its size.
    pub bytes: u64,
    /// Whether it holds a secret the broker keeps: the owner is warned.
    /// It was put all the same: it is the owner's.
    pub holds_secret: bool,
}

/// A file the agent handed the owner (WO-041), as the Library keeps it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LibraryFile {
    /// The broker's id for it.
    pub id: String,
    /// The broker's name for it: plain ASCII, made from the file's own.
    pub name: String,
    /// Its length, in bytes.
    pub bytes: u64,
    /// Its SHA-256, in lowercase hex.
    pub sha256: String,
    /// When it was kept, as RFC 3339 (v7).
    pub kept_at: String,
    /// The conversation it came from, by the Library's number for it (v8).
    pub conversation: u64,
    /// The reply it came from: the chat run's cell id (v8); none for one
    /// kept before items were grouped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
}

/// A folder the agent handed the owner (v8, WO-043), as the Library keeps
/// it: its files are listed by [`crate::ui::UiAction::ListFolder`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LibraryFolder {
    /// The broker's id for it.
    pub id: String,
    /// The broker's name for it: plain ASCII, made from the folder's own.
    pub name: String,
    /// How many files it holds.
    pub files: u32,
    /// Its files' length together, in bytes.
    pub bytes: u64,
    /// The SHA-256, in lowercase hex, of its list as the Library keeps it:
    /// the JSON of its entries, each its path, length and SHA-256.
    pub sha256: String,
    /// When it was kept, as RFC 3339.
    pub kept_at: String,
    /// The conversation it came from, by the Library's number for it.
    pub conversation: u64,
    /// The reply it came from: the chat run's cell id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    /// What its walk left out, by kind.
    pub left_out: crate::LeftOut,
}

/// One file of a folder in the Library (v8), as its list says it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FolderEntry {
    /// Its path in the folder: steps of printable ASCII joined by `/`.
    pub path: String,
    /// Its length, in bytes.
    pub bytes: u64,
    /// Its SHA-256, in lowercase hex.
    pub sha256: String,
}

/// The most entries one page of a folder's list holds (v8): a page of the
/// longest fits one frame.
pub const FOLDER_PAGE: u32 = 32;

/// The Library's use against its bounds, and the open conversation's
/// standing (v7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LibraryStanding {
    /// The items it keeps, files and folders (v8: a folder is one).
    pub files: u32,
    /// The most items it keeps.
    pub file_limit: u32,
    /// Their bytes together.
    pub bytes: u64,
    /// The most bytes it keeps.
    pub byte_limit: u64,
    /// The files the agent has given in the open conversation.
    pub given: u32,
    /// The most it gives in one conversation.
    pub given_limit: u32,
    /// Whether giving files is off for the open conversation: one held a
    /// secret. A new conversation turns it back on.
    pub off: bool,
    /// The files it keeps in all, each of a folder's counted (v8).
    pub files_in_all: u32,
    /// The most files it keeps in all (v8).
    pub files_in_all_limit: u32,
    /// What the open conversation has read out of the agent's computer, in
    /// bytes (v8).
    pub read: u64,
    /// The most one conversation reads (v8).
    pub read_limit: u64,
    /// The open conversation, by the Library's number for it (v8).
    pub conversation: u64,
}

/// Why a file wasn't read out of the Library for the owner (v7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum FileOutRefusal {
    /// The Library has no file of that id: it was removed.
    Gone,
    /// The Library's copy doesn't read back to its hash: nothing of it is
    /// given. Removing it is the fix.
    Damaged,
    /// Other downloads are under way: one at a time a connection, and few
    /// at once. Try again shortly.
    Busy,
    /// No file of that id is being downloaded on this connection: it was
    /// never begun, its last part was read, it went idle, its connection
    /// began another, or its file was removed.
    UnknownDownload,
    /// A part asked out of order. The download is let go, and begun again.
    OutOfStep,
    /// A folder's list or file was asked of an item that is a file (v8).
    NotAFolder,
    /// An item that is a folder was asked as a file (v8): a file of it is
    /// asked by its place.
    AFolder,
    /// A place past the folder's files (v8).
    OutOfRange,
}

/// The agent handed the owner a file (v6): it is kept in the Library.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileOffered {
    /// The chat it was handed over in.
    pub chat_id: String,
    /// Its place in the chat: the broker's.
    pub message_id: String,
    /// The file.
    pub file: LibraryFile,
}

/// The agent tried to hand the owner a file, and it was withheld (v6): it
/// holds a secret the broker keeps, which is never named. Handing files
/// over is off for the conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileWithheld {
    /// The chat it was handed over in.
    pub chat_id: String,
    /// Its place in the chat: the broker's.
    pub message_id: String,
    /// The broker's name for it: none when the name itself held the
    /// secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
