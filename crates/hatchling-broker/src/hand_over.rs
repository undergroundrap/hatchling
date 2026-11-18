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
//! - **Spent, then checked.** One of the conversation's hand-overs is spent
//!   on the file, sealed, before it is checked: every check is a guess at a
//!   secret, and is counted, whatever comes of it. A file the Library has
//!   no room for is refused, unchecked.
//! - **Checked, the file and its name.** Each for the secrets a gate scans
//!   for and for the keeper's whole set. A secret found withholds the file,
//!   says so to the agent and the owner without naming it, and turns
//!   handing files over off for the conversation. A check that couldn't
//!   be made keeps nothing and turns nothing off: it is never told as a
//!   secret found.
//! - **Kept, then offered.** The Library keeps it ([`crate::library`]);
//!   the owner's app is told of it, and Activity keeps how it ended, the
//!   broker's name and its size: never its path or what it holds.
//!
//! A folder (`give_folder`, WO-043) is handed over the same way, as one:
//!
//! - **Walked before anything is read** ([`crate::folder_walk`]): its
//!   names held to the owner's computer's rules and its size to the
//!   bounds, by the Room's listings, before anything is spent. What isn't
//!   a file or a folder is left out, and counted.
//! - **Spent as the walk found it**, files and bytes, before any file is
//!   read; then each file is read whole and held at once to the length
//!   its listing said: a folder that changed is refused whole, and what a
//!   file read past its listed length counts toward the conversation's
//!   bytes too. Its paths are checked with it, counted only as one of the
//!   conversation's hand-overs.
//! - **Checked as one**: its own name alone, then every path and every
//!   byte of its files, back to back, in one text, so a secret split
//!   across two files is found. That text is the only copy: what is kept
//!   is what was checked. A value shorter than a check looks for inside a
//!   text ([`crate::held_check::MIN_SCAN_BYTES`]) is found only as a whole
//!   text: a file that is exactly a short secret is found by `give_file`,
//!   not in a folder.
//! - **Kept as one item**, with what was left out. Nothing tells the
//!   owner's app of it yet: it reads it from the Library.

use std::cell::Cell;
use std::ops::Range;
use std::time::{Duration, Instant};

use hatchling_artifact::SecretSet;
use hatchling_core::ui::{FileOffered, FileWithheld, UiEventBody};
use hatchling_core::{ActivityEvent, CellId, FileOutEnded, RoomResult, SubagentEvent};
use hatchling_scheduler::Timestamp;
use hatchling_vault::OpensStores;

use crate::Config;
use crate::api_ops::LOCAL_CHAT;
use crate::folder_walk::{CHANGED, TOO_SLOW, Walked};
use crate::held_check::CheckFor;
use crate::library::{Claim, Library, Refused};
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

/// Said of a file whose kind makes Windows reach out to another machine
/// when the folder it is in is only shown.
pub(crate) const REACHES_OUT: &str = "that kind of file can make Windows connect to another computer when its folder is only opened: put it in an archive (a .zip), then give that";

/// The extensions of files Windows acts on when their folder is shown,
/// lowercase: a link or a shell command file can name a share on another
/// machine, which Windows then connects to with the owner's sign-in.
const REACHING: [&str; 11] = [
    "url",
    "website",
    "pif",
    "deskthemepack",
    "scf",
    "lnk",
    "library-ms",
    "search-ms",
    "searchconnector-ms",
    "theme",
    "themepack",
];

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

/// Said of the workspace's inbox, or a folder in it, asked to be given.
pub(crate) const THE_INBOX: &str =
    "the workspace's inbox holds what the owner gave you: it isn't given back";

/// How long a folder may take to give: its walk and its reads.
pub(crate) const FOLDER_TIME: Duration = Duration::from_secs(5 * 60);

/// What a folder is checked as, at most: its paths, each of at most
/// 1024 bytes with a newline, one more newline, and its files' bytes.
#[allow(
    dead_code,
    reason = "used by the assertion below and a test, which Rust 1.85 doesn't count"
)]
const FOLDER_CHECK_BYTES: u64 =
    crate::library::FOLDER_FILES * (1024 + 1) + 1 + crate::library::FOLDER_BYTES;

/// A folder is checked whole, in one check.
const _: () = assert!(FOLDER_CHECK_BYTES <= crate::held_check::MAX_CHECK_BYTES);

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
    if reaches_out(&name) {
        return Err(REACHES_OUT);
    }
    Ok(name)
}

/// Whether `name` is a file Windows acts on when its folder is shown,
/// whatever its case: `desktop.ini`, or one with an extension in
/// [`REACHING`].
pub(crate) fn reaches_out(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "desktop.ini"
        || lower
            .rsplit_once('.')
            .is_some_and(|(_, extension)| REACHING.contains(&extension))
}

/// Whether `stem` is a name Windows keeps for a device, whatever its case.
pub(crate) fn is_reserved(stem: &str) -> bool {
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
    /// How long a folder may take to give.
    folder_time: Duration,
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
            folder_time: FOLDER_TIME,
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
        // The conversation this hand-over belongs to, read before the run is
        // known to be in it: a new one started after can't be counted or
        // filled by this run.
        let conversation = match library.conversation() {
            Ok(conversation) => conversation,
            Err(error) => {
                tracing::warn!(%error, "the Library couldn't be read for a file out");
                return unread(NOT_KEPT);
            }
        };
        match self.open_now(cell) {
            None => return unread(NOT_THE_CHAT),
            Some(false) => return unread(STALE),
            Some(true) => {}
        }
        match library.refuses(&Claim::file(0)) {
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
        // Each check is a guess at a secret: one is spent before it is made,
        // and a file the Library couldn't keep is never checked.
        let spent = match library.spend(&Claim::file(size), conversation) {
            Ok(Ok(spent)) => spent,
            Ok(Err(refused)) => {
                return self.ended(None, size, FileOutEnded::Refused, refused.words());
            }
            Err(error) => {
                tracing::warn!(%error, "the Library couldn't count a file out");
                return self.ended(None, size, FileOutEnded::Refused, NOT_KEPT);
            }
        };
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
            Ok(true) => return self.withheld(&library, conversation, shown, size),
            Err(error) => {
                tracing::warn!(%error, "a file out couldn't be checked");
                return self.ended(None, size, FileOutEnded::Unchecked, UNCHECKED);
            }
            Ok(false) => {}
        }
        let now = Timestamp::now().to_string();
        match library.keep_spent(&name, &bytes, &now, spent, cell.as_str()) {
            Ok(Ok(kept)) => self.offered(kept),
            Ok(Err(refused)) => self.refused_kept(&name, size, refused),
            Err(error) => {
                tracing::warn!(%error, "the Library couldn't keep a file out");
                self.ended(Some(&name), size, FileOutEnded::Refused, NOT_KEPT)
            }
        }
    }

    /// Hands over the folder at `path` in the Room's workspace, walked and
    /// read out through `rooms`.
    pub(crate) fn hand_folder(&self, rooms: &dyn Rooms, path: &str) -> RoomResult {
        let Some(library) = Library::of(self.config, self.keys) else {
            return unread(OFF_HERE);
        };
        let Some(cell) = self.cell else {
            return unread(NOT_THE_CHAT);
        };
        let conversation = match library.conversation() {
            Ok(conversation) => conversation,
            Err(error) => {
                tracing::warn!(%error, "the Library couldn't be read for a folder out");
                return unread(NOT_KEPT);
            }
        };
        match self.open_now(cell) {
            None => return unread(NOT_THE_CHAT),
            Some(false) => return unread(STALE),
            Some(true) => {}
        }
        match library.refuses(&Claim::folder(1, 0)) {
            Ok(None) => {}
            Ok(Some(refused)) => return unread(refused.words()),
            Err(error) => {
                tracing::warn!(%error, "the Library couldn't be read for a folder out");
                return unread(NOT_KEPT);
            }
        }
        let root = crate::room_ops::placed(path);
        if let Err(why) = hatchling_core::room::check_list(&root, b"") {
            return unread(why);
        }
        // The owner's, whatever its listings say: never walked.
        let inbox = format!("{}/inbox", hatchling_core::room::WORKSPACE);
        if root == inbox || root.starts_with(&format!("{inbox}/")) {
            return unread(THE_INBOX);
        }
        let name = match name_of(&root) {
            Ok(name) => name,
            Err(why) => return unread(why),
        };
        let until = Instant::now() + self.folder_time;
        let walked = match crate::folder_walk::walk(rooms, &root, until) {
            Ok(walked) => walked,
            Err(why) => return unread(&why),
        };
        // A walk that ran out of time spends nothing.
        if Instant::now() >= until {
            return unread(TOO_SLOW);
        }
        let files = u64::try_from(walked.files.len()).unwrap_or(u64::MAX);
        // Each check is a guess at a secret: one is spent before anything
        // is read, on what the walk found.
        let spent = match library.spend(&Claim::folder(files, walked.bytes), conversation) {
            Ok(Ok(spent)) => spent,
            Ok(Err(refused)) => return self.ended(None, 0, FileOutEnded::Refused, refused.words()),
            Err(error) => {
                tracing::warn!(%error, "the Library couldn't count a folder out");
                return self.ended(None, 0, FileOutEnded::Refused, NOT_KEPT);
            }
        };
        let (text, spans) = match read_folder(rooms, &root, &walked, until) {
            Ok(read) => read,
            Err(stopped) => {
                read_past(&library, stopped.past, conversation);
                return self.ended(None, stopped.read, FileOutEnded::Refused, &stopped.why);
            }
        };
        tracing::info!(
            files,
            bytes = walked.bytes,
            "a folder crossed out of the Room"
        );
        let name_held = self.checks.holds(name.as_bytes());
        let folder_held = self.checks.holds(&text);
        let held = either(name_held.clone(), folder_held);
        // A name is shown only when it was checked and found clear.
        let shown = (name_held == Ok(false)).then_some(name.as_str());
        if self.open_now(cell) != Some(true) {
            return self.ended(shown, walked.bytes, FileOutEnded::Refused, STALE);
        }
        match held {
            Ok(true) => return self.withheld(&library, conversation, shown, walked.bytes),
            Err(error) => {
                tracing::warn!(%error, "a folder out couldn't be checked");
                return self.ended(None, walked.bytes, FileOutEnded::Unchecked, UNCHECKED);
            }
            Ok(false) => {}
        }
        let parts: Vec<(&str, &[u8])> = walked
            .files
            .iter()
            .zip(spans)
            .map(|((path, _), span)| (path.as_str(), text.get(span).unwrap_or_default()))
            .collect();
        let now = Timestamp::now().to_string();
        match library.keep_folder_spent(&name, &parts, walked.left_out, &now, spent, cell.as_str())
        {
            Ok(Ok(kept)) => self.folder_offered(kept),
            Ok(Err(refused)) => self.refused_kept(&name, walked.bytes, refused),
            Err(error) => {
                tracing::warn!(%error, "the Library couldn't keep a folder out");
                self.ended(Some(&name), walked.bytes, FileOutEnded::Refused, NOT_KEPT)
            }
        }
    }

    /// The folder was kept: Activity keeps it with its count of files,
    /// every app is told to read the Library again, and the agent is told
    /// of it.
    fn folder_offered(&self, kept: crate::library::KeptItem) -> RoomResult {
        let files = u32::try_from(kept.files).unwrap_or(u32::MAX);
        self.record(
            Some(&kept.name),
            kept.bytes,
            Some(files),
            FileOutEnded::Offered,
        );
        crate::app_stream::stream(self.config).event(UiEventBody::LibraryChanged {});
        RoomResult::FolderHandedOver {
            name: kept.name,
            files,
            bytes: kept.bytes,
            left_out: kept.left_out,
        }
    }

    /// The file held a secret: nothing is kept, handing files over is off
    /// for the conversation, and the owner is told, by `shown` unless the
    /// name held it.
    fn withheld(
        &self,
        library: &Library<'_>,
        conversation: u64,
        shown: Option<&str>,
        size: u64,
    ) -> RoomResult {
        if let Err(error) = library.turn_off_in(conversation) {
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
    fn offered(&self, kept: crate::library::KeptItem) -> RoomResult {
        let name = kept.name.clone();
        let bytes = kept.bytes;
        let file = crate::api_ops::shown(kept);
        self.tell(|message_id| {
            UiEventBody::FileOffered(FileOffered {
                chat_id: String::from(LOCAL_CHAT),
                message_id,
                file: file.clone(),
            })
        });
        self.record(Some(&name), bytes, None, FileOutEnded::Offered);
        RoomResult::HandedOver { name, bytes }
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
    fn record(&self, name: Option<&str>, bytes: u64, files: Option<u32>, ended: FileOutEnded) {
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
                files,
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
        self.record(name, bytes, None, ended);
        RoomResult::NotHandedOver {
            reason: reason.to_owned(),
        }
    }
}

/// A folder read: the text it is checked as and where each file is in
/// it; or why it isn't handed over.
type FolderRead = Result<(Vec<u8>, Vec<Range<usize>>), Unread>;

/// A folder whose reads ended before it was whole.
struct Unread {
    /// The bytes read out of the Room.
    read: u64,
    /// Those read past what the folder was spent on.
    past: u64,
    /// Why, for the agent.
    why: String,
}

impl Unread {
    fn new(read: u64, past: u64, why: impl Into<String>) -> Self {
        Self {
            read,
            past,
            why: why.into(),
        }
    }
}

/// Reads each file `walked` found in the folder at `root` through
/// `rooms`, before `until`, into the one text a folder is checked as: its
/// paths, each ended by a newline, a newline, then every file's bytes,
/// back to back. Each read is held at once to the length the walk listed.
/// What it reads, and where each file's bytes are in it.
///
/// # Errors
///
/// How many bytes were read, and why the folder isn't handed over.
fn read_folder(rooms: &dyn Rooms, root: &str, walked: &Walked, until: Instant) -> FolderRead {
    let paths: usize = walked.files.iter().map(|(path, _)| path.len() + 1).sum();
    let mut text = Vec::new();
    // Bounded: the walk held the listed lengths to a folder's, and every
    // read to its listed length.
    text.try_reserve_exact(paths + 1 + usize::try_from(walked.bytes).unwrap_or(usize::MAX))
        .map_err(|_| Unread::new(0, 0, NOT_KEPT))?;
    for (path, _) in &walked.files {
        text.extend_from_slice(path.as_bytes());
        text.push(b'\n');
    }
    text.push(b'\n');
    let mut spans = Vec::with_capacity(walked.files.len());
    let mut read = 0_u64;
    for (path, listed) in &walked.files {
        if Instant::now() >= until {
            return Err(Unread::new(read, 0, TOO_SLOW));
        }
        let bytes = rooms
            .take(&crate::folder_walk::whole(root, path))
            .map_err(|reason| Unread::new(read, 0, reason))?;
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        read = read.saturating_add(length);
        if length != *listed {
            return Err(Unread::new(read, length.saturating_sub(*listed), CHANGED));
        }
        let start = text.len();
        text.extend_from_slice(&bytes);
        spans.push(start..text.len());
    }
    Ok((text, spans))
}

/// Counts `past`, what a folder's file read past its listed length,
/// toward `conversation`'s bytes in `library`.
fn read_past(library: &Library<'_>, past: u64, conversation: u64) {
    if past == 0 {
        return;
    }
    if let Err(error) = library.read_more(past, conversation) {
        tracing::warn!(%error, "the Library couldn't count a folder's read");
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

#[cfg(test)]
#[path = "hand_over/folder_tests.rs"]
mod folder_tests;
