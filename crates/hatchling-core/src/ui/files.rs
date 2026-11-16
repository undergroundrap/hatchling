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
