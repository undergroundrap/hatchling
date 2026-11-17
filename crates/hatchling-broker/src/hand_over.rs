//! Handing the owner a file (WO-041): what the agent's `give_file` asks,
//! as the broker answers it.
//!
//! - **Only the chat's own run, in the open conversation.** Handing over
//!   is the broker's own when the operator gave it a Library; a session the
//!   broker didn't start as a run names no run; a run is the chat's only
//!   when the chat says so ([`crate::follow_ups`]); and one started before
//!   the owner's new conversation hands nothing over.
//! - **The broker's name, never the agent's.** The file's last component
//!   made plain ASCII, as a file in's stem is; refused when its part
//!   before the first dot is a name Windows keeps for itself, or it ends in
//!   a dot.
//! - **The Room's copy, read once.** The spawner reads the file out whole
//!   ([`crate::room_ops::Rooms::take`]); that one buffer is checked and
//!   kept, and nothing reads the Room again for it.
//! - **Checked, the file and its name.** Each for the secrets a gate scans
//!   for and for the keeper's whole set. A secret found withholds the file,
//!   says so to the agent and the owner without naming it, and turns
//!   handing files over off for the conversation. A check that couldn't
//!   be made keeps nothing and turns nothing off: it is never told as a
//!   secret found.
//! - **Kept, then offered.** The Library keeps it ([`crate::library`]);
//!   the owner's app is told of it, and Activity keeps how it ended, the
//!   broker's name and its size: never its path or what it holds.

use std::cell::Cell;

use hatchling_artifact::SecretSet;
use hatchling_core::ui::{FileOffered, FileWithheld, LibraryFile, UiEventBody};
use hatchling_core::{ActivityEvent, CellId, FileOutEnded, RoomResult, SubagentEvent};
use hatchling_scheduler::Timestamp;
use hatchling_vault::OpensStores;

use crate::Config;
use crate::api_ops::LOCAL_CHAT;
use crate::held_check::CheckFor;
use crate::library::{Library, Refused};
use crate::room_ops::Rooms;

/// What the scan of a file out is recorded as: a gate's scan.
pub(crate) const FILE_OUT_GATE: &str = "file-out-gate";

/// Said where the operator gave the broker no Library.
pub(crate) const OFF_HERE: &str = "giving the owner files is off on this install";

/// Said to a session that isn't the chat's run.
pub(crate) const NOT_THE_CHAT: &str = "only a chat's own reply gives the owner a file";

/// Said to a run started before the owner's new conversation.
pub(crate) const STALE: &str =
    "the owner started a new conversation since this reply began: nothing from it is given";

/// Said of a name Windows keeps for itself.
pub(crate) const RESERVED: &str =
    "that file's name is one Windows keeps for itself: rename the file, then give it";

/// Said of a name that ends in a dot.
pub(crate) const TRAILING_DOT: &str =
    "a file's name can't end in a dot: rename the file, then give it";

/// Said of a file, or its name, that holds a secret the broker keeps.
pub(crate) const WITHHELD: &str = "it was withheld: it holds a secret Hatchling keeps, which isn't named. Giving files is now off for this conversation; the owner can start a new one";

/// Said of a file that couldn't be checked.
pub(crate) const UNCHECKED: &str =
    "it couldn't be checked for the secrets Hatchling keeps, so it wasn't given: try again later";

/// Said of a file the Library couldn't keep.
pub(crate) const NOT_KEPT: &str = "the Library couldn't keep it";

/// The most `file_out` lines one session writes to Activity: a session
/// that asks over and over is the Room's, and fills no log.
pub(crate) const RECORDED_MAX: u32 = 32;

/// Whether a text holds a secret the broker keeps.
pub(crate) trait Checks {
    /// Whether `text` holds one: in the set a gate scans for, or in the
    /// keeper's whole set.
    ///
    /// # Errors
    ///
    /// When it couldn't be checked.
    fn holds(&self, text: &[u8]) -> Result<bool, String>;
}

/// The broker's own checks, for the run of `cell`.
struct Held<'k> {
    config: &'k Config,
    keys: &'k dyn OpensStores,
    cell: Option<&'k CellId>,
}

impl Held<'_> {
    /// The gate's scan: the secrets a gate scans for, as they are.
    fn scanned(&self, text: &[u8]) -> Result<bool, String> {
        crate::artifact_ops::held_secret_values(self.config, self.keys, FILE_OUT_GATE)
            .map(|values| SecretSet::new(values).exposed_in(text))
    }

    /// The keeper's check, of its whole set, as they are and encoded.
    fn kept(&self, text: &[u8]) -> Result<bool, String> {
        crate::held_check::check(
            crate::keeper::keeper(),
            self.config,
            self.keys,
            CheckFor::FileOut,
            self.cell,
            text,
        )
    }
}

impl Checks for Held<'_> {
    fn holds(&self, text: &[u8]) -> Result<bool, String> {
        either(self.scanned(text), self.kept(text))
    }
}

/// Whether either check found a secret; that it couldn't be checked when
/// neither did and one couldn't be made.
fn either(first: Result<bool, String>, second: Result<bool, String>) -> Result<bool, String> {
    match (first, second) {
        (Ok(true), _) | (_, Ok(true)) => Ok(true),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(false), Ok(false)) => Ok(false),
    }
}

/// The broker's name for the file at `path`: its last component, as a
/// file in's stem is made. `Err` with why when it can't be one.
pub(crate) fn name_of(path: &str) -> Result<String, &'static str> {
    let name = crate::file_in::stem_of(path);
    let before_dot = name.split('.').next().unwrap_or_default();
    if is_reserved(before_dot) {
        return Err(RESERVED);
    }
    if name.ends_with('.') {
        return Err(TRAILING_DOT);
    }
    Ok(name)
}

/// Whether `stem` is a name Windows keeps for a device, whatever its case.
fn is_reserved(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    let numbered = |prefix: &str| {
        upper
            .strip_prefix(prefix)
            .is_some_and(|digit| matches!(digit.as_bytes(), [b'0'..=b'9']))
    };
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT")
}

/// A session's way to hand the owner a file.
pub(crate) struct Handing<'k> {
    config: &'k Config,
    keys: &'k dyn OpensStores,
    /// The run the broker started this session as: none for one it didn't.
    cell: Option<&'k CellId>,
    /// What Activity names the session by.
    run_id: String,
    /// Whether a run is the chat's, and in its open conversation.
    open: fn(&str) -> Option<bool>,
    checks: Box<dyn Checks + 'k>,
    /// How many lines it has written to Activity.
    recorded: Cell<u32>,
}

impl<'k> Handing<'k> {
    /// For the session under `config` whose hello named `cell`, a run of
    /// the broker's own only when it is `known` as one: a session the
    /// broker didn't start names any cell it likes. Named `run_id` in
    /// Activity.
    pub(crate) fn new(
        config: &'k Config,
        keys: &'k dyn OpensStores,
        known: bool,
        cell: &'k CellId,
        run_id: &str,
    ) -> Self {
        let cell = known.then_some(cell);
        Self {
            config,
            keys,
            cell,
            run_id: run_id.to_owned(),
            open: crate::follow_ups::open_for,
            checks: Box::new(Held { config, keys, cell }),
            recorded: Cell::new(0),
        }
    }

    /// Whether the run of `cell` is still the chat's, in the conversation
    /// open now: asked before anything is read, and again before anything
    /// is kept or turned off, as the owner may start a new one meanwhile.
    fn open_now(&self, cell: &CellId) -> Option<bool> {
        (self.open)(cell.as_str())
    }

    /// Hands over the file at `path` in the Room's workspace, read out
    /// through `rooms`.
    pub(crate) fn hand(&self, rooms: &dyn Rooms, path: &str) -> RoomResult {
        // What is refused before the file is read is the agent's to hear,
        // and no line of Activity's: nothing crossed.
        let Some(library) = Library::of(self.config, self.keys) else {
            return unread(OFF_HERE);
        };
        let Some(cell) = self.cell else {
            return unread(NOT_THE_CHAT);
        };
        match self.open_now(cell) {
            None => return unread(NOT_THE_CHAT),
            Some(false) => return unread(STALE),
            Some(true) => {}
        }
        match library.refuses(0) {
            Ok(None) => {}
            Ok(Some(refused)) => return unread(refused.words()),
            Err(error) => {
                tracing::warn!(%error, "the Library couldn't be read for a file out");
                return unread(NOT_KEPT);
            }
        }
        let name = match name_of(path) {
            Ok(name) => name,
            Err(why) => return unread(why),
        };
        let bytes = match rooms.take(&crate::room_ops::placed(path)) {
            Ok(bytes) => bytes,
            Err(reason) => return self.ended(None, 0, FileOutEnded::Refused, &reason),
        };
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        tracing::info!(bytes = size, "a file crossed out of the Room");
        let name_held = self.checks.holds(name.as_bytes());
        let file_held = self.checks.holds(&bytes);
        let held = either(name_held.clone(), file_held);
        // A name is shown only when it was checked and found clear.
        let shown = (name_held == Ok(false)).then_some(name.as_str());
        // The owner may have started a new conversation while it was
        // read: nothing from this run is kept, or turns the new one off.
        if self.open_now(cell) != Some(true) {
            return self.ended(shown, size, FileOutEnded::Refused, STALE);
        }
        match held {
            Ok(true) => return self.withheld(&library, shown, size),
            Err(error) => {
                tracing::warn!(%error, "a file out couldn't be checked");
                return self.ended(None, size, FileOutEnded::Unchecked, UNCHECKED);
            }
            Ok(false) => {}
        }
        match library.keep(&name, &bytes, &Timestamp::now().to_string()) {
            Ok(Ok(kept)) => self.offered(kept),
            Ok(Err(refused)) => self.refused_kept(&name, size, refused),
            Err(error) => {
                tracing::warn!(%error, "the Library couldn't keep a file out");
                self.ended(Some(&name), size, FileOutEnded::Refused, NOT_KEPT)
            }
        }
    }

    /// The file held a secret: nothing is kept, handing files over is off
    /// for the conversation, and the owner is told, by `shown` unless the
    /// name held it.
    fn withheld(&self, library: &Library<'_>, shown: Option<&str>, size: u64) -> RoomResult {
        if let Err(error) = library.turn_off() {
            tracing::error!(%error, "handing files over couldn't be turned off after a secret was found");
        }
        self.tell(|message_id| {
            UiEventBody::FileWithheld(FileWithheld {
                chat_id: String::from(LOCAL_CHAT),
                message_id,
                name: shown.map(str::to_owned),
            })
        });
        self.ended(shown, size, FileOutEnded::Withheld, WITHHELD)
    }

    /// The file was kept: the owner is told of it, and the agent.
    fn offered(&self, kept: crate::library::KeptFile) -> RoomResult {
        let file = LibraryFile {
            id: kept.id,
            name: kept.name.clone(),
            bytes: kept.bytes,
            sha256: kept.sha256,
        };
        self.tell(|message_id| {
            UiEventBody::FileOffered(FileOffered {
                chat_id: String::from(LOCAL_CHAT),
                message_id,
                file: file.clone(),
            })
        });
        self.record(Some(&kept.name), kept.bytes, FileOutEnded::Offered);
        RoomResult::HandedOver {
            name: kept.name,
            bytes: kept.bytes,
        }
    }

    fn refused_kept(&self, name: &str, size: u64, refused: Refused) -> RoomResult {
        self.ended(Some(name), size, FileOutEnded::Refused, refused.words())
    }

    /// Tells the owner's app `body`, under a message id the broker mints.
    fn tell(&self, body: impl FnOnce(String) -> UiEventBody) {
        match crate::approval::random_hex("file-") {
            Ok(message_id) => {
                crate::app_stream::stream(self.config).event(body(message_id));
            }
            Err(error) => tracing::warn!(%error, "the app couldn't be told of a file out"),
        }
    }

    /// Keeps how a file out ended in Activity: the broker's name, its
    /// size, and how. Past [`RECORDED_MAX`] lines a session, nothing more.
    fn record(&self, name: Option<&str>, bytes: u64, ended: FileOutEnded) {
        if self.recorded.get() >= RECORDED_MAX {
            return;
        }
        self.recorded.set(self.recorded.get().saturating_add(1));
        let Ok(id) = crate::approval::random_hex("") else {
            return;
        };
        let event = ActivityEvent {
            event_id: format!("{}:file-out-{id}", self.run_id),
            ts: Timestamp::now().to_string(),
            run_id: self.run_id.clone(),
            kind: SubagentEvent::FileOut {
                name: name.map(str::to_owned),
                bytes,
                ended,
            },
        };
        crate::subagent_ops::record(
            crate::run_trail::activity_log(self.config).as_deref(),
            &event,
            "file out",
        );
    }

    /// A file out that ended other than kept: Activity keeps it, and the
    /// agent is told `reason`.
    fn ended(
        &self,
        name: Option<&str>,
        bytes: u64,
        ended: FileOutEnded,
        reason: &str,
    ) -> RoomResult {
        self.record(name, bytes, ended);
        RoomResult::NotHandedOver {
            reason: reason.to_owned(),
        }
    }
}

/// Nothing was given, and nothing was read: `reason` is the agent's.
fn unread(reason: &str) -> RoomResult {
    RoomResult::NotHandedOver {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
#[path = "hand_over/tests.rs"]
mod tests;
