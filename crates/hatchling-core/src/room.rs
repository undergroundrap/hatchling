//! The Room's wire: what the spawner asks the supervisor confined in the
//! Room, and what the supervisor says back (0.9).
//!
//! A command in the Room is a job. Whoever asks for one names it
//! ([`JobId`]), so a caller that lost an answer can ask what became of the
//! job by its name, and never has to run it again to find out.
//!
//! The spawner asks one thing at a time ([`Ask`]) and the supervisor
//! answers it ([`Say`]), echoing the ask's nonce. One thing is said
//! unasked: that a job has ended ([`Say::Ended`]). What a job wrote is kept
//! as a head and a tail of each stream ([`PIECE_BYTES`] each), with the
//! count of what fell between them, and the whole of it, up to
//! [`SPILL_BYTES`], in a file in the Room's session directory
//! ([`spill_path`]). The pieces are asked for one at a time
//! ([`Ask::Piece`]), so that every frame stays small. The supervisor
//! remembers only so many ended jobs: the oldest goes, with its files,
//! when another ends.
//!
//! The spawner may also ask for a page of a file in the Room
//! ([`Ask::Page`]): some of its lines, from one it names, as many as fit
//! [`PAGE_BYTES`]. The supervisor reads the file itself, as the Room's
//! user, so a page starts no job and takes no job's place among those it
//! remembers.
//!
//! And it may ask for the lines under a path that match a pattern
//! ([`Ask::Search`]): the supervisor has the Room's own search program
//! look, for no longer than [`SEARCH_MILLIS`], and hands back the first
//! [`FOUND_BYTES`] of what it found, in whole lines. A search is no job
//! either: it has no name, nothing of it is kept, and it is over before
//! the supervisor answers.
//!
//! And it may ask for a file to be written ([`Ask::Write`]), or for one
//! exact piece of a file's text to be replaced ([`Ask::Edit`]). The
//! supervisor writes the file itself, as the Room's user, whole and in
//! one step: no job is started for it, and nothing that reads the file
//! sees half of it. A write or an edit that got no answer isn't known to
//! have happened, and is never asked again by itself.
//!
//! And it may ask for a piece of a file in the workspace, to hand the file
//! to the owner ([`Ask::Take`], WO-041): at most [`TAKE_BYTES`] of it from
//! a byte it names, with the file's length and which file it is. The
//! supervisor opens it itself, as the Room's user, beneath the workspace,
//! following no link, and reads only a file. Each piece is asked for on
//! its own, and nothing is kept between them: the spawner puts them
//! together, and holds the file to being the same one, the same length,
//! from its first piece to its last.
//!
//! And it may ask what one folder in the workspace holds, for a folder to
//! be handed to the owner ([`Ask::List`], WO-043): a page of at most
//! [`LIST_PAGE`] of its names, in the order of their bytes, after a name
//! it names, each with what it is. The supervisor opens the folder as it
//! opens a file to take, and keeps nothing between pages: the spawner puts
//! them together, and holds the folder to being the same one from its
//! first page to its last. What a listing says a thing is, is never
//! trusted on its own: a file in it is read by a take, which decides.
//!
//! The supervisor runs in the Room, as the Room's user: everything it says
//! is untrusted. [`Captured::check`], [`Ended::check`], [`Page::check`],
//! [`Found::check`], [`Wrote::check`], [`Edited::check`],
//! [`NoEdit::check`], [`Took::check`] and [`Listed::check`] are what the
//! spawner holds its words to; unknown fields never decode.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The longest a job's name may be, in bytes.
pub const JOB_ID_MAX_LEN: usize = 64;

/// How much of the start of a stream, and of its end, is kept in memory
/// and handed back with a job's result.
pub const PIECE_BYTES: usize = 16 * 1024;

/// How much of a stream is written to its file in the session directory.
pub const SPILL_BYTES: u64 = 8 * 1024 * 1024;

/// The most a job may be given on its standard input.
pub const STDIN_BYTES: usize = 32 * 1024;

/// The most arguments a command may have, its program among them.
pub const ARGV_MAX: usize = 64;

/// The most bytes a command's arguments may come to.
pub const ARGV_BYTES: usize = 8 * 1024;

/// The longest a job's working directory may be, in bytes.
pub const DIR_BYTES: usize = 1024;

/// How many jobs run in the Room at once.
pub const RUNNING_MAX: usize = 4;

/// The most of a file one page hands back, in bytes.
pub const PAGE_BYTES: usize = 8 * 1024;

/// The most lines one page may be asked for.
pub const PAGE_LINES: u32 = 2000;

/// How much of a file the supervisor reads, from its start, to find a
/// page. A page that isn't within it isn't handed back.
pub const PAGE_SCAN_BYTES: u64 = 32 * 1024 * 1024;

/// The longest a file's path may be, in bytes.
pub const PATH_BYTES: usize = 1024;

/// The most of what a search found one answer hands back, in bytes.
pub const FOUND_BYTES: usize = 8 * 1024;

/// The most of one matching line that is handed back, in bytes: a longer
/// line is cut there, and says so.
pub const FOUND_LINE_BYTES: usize = 400;

/// How long the supervisor lets a search look, in milliseconds. Short of
/// the time the spawner waits for any answer.
pub const SEARCH_MILLIS: u64 = 5000;

/// The longest a search's pattern may be, in bytes.
pub const PATTERN_BYTES: usize = 1024;

/// The longest a search's file-name glob may be, in bytes.
pub const GLOB_BYTES: usize = 256;

/// The most a file may be written with at once, in bytes.
pub const WRITE_BYTES: usize = 32 * 1024;

/// The longest text an edit may replace, and the longest it may put in
/// that text's place, in bytes.
pub const EDIT_BYTES: usize = 8 * 1024;

/// The longest a file may be for an edit to be made in it, before the
/// edit and after it, in bytes.
pub const EDIT_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// How many of the places an edit's text was found at are named, when
/// it was found at more than the one wanted.
pub const EDIT_PLACES: usize = 8;

/// The most of a file one take hands back, in bytes.
pub const TAKE_BYTES: usize = 32 * 1024;

/// The longest a file may be to be taken out of the Room, in bytes.
pub const TAKE_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The most names one page of a folder's listing holds.
pub const LIST_PAGE: usize = 64;

/// The most names a folder may hold to be listed.
pub const LIST_FOLDER_NAMES: u32 = 1024;

/// The longest a name in a folder may be, in bytes: the kernel's own
/// bound.
pub const NAME_BYTES: usize = 255;

/// Where a job starts when its caller names no directory.
pub const WORKSPACE: &str = "/workspace";

/// The Room's session directory: scratch that goes with the session.
pub const SESSION_DIR: &str = "/session";

/// Where the supervisor keeps what jobs wrote, in the Room.
pub const JOBS_DIR: &str = "/session/jobs";

/// A job's `PATH`: where its programs are looked for.
///
/// The image keeps what it adds to its distribution (Node) in
/// `/usr/local/bin`, and the distribution's own programs in the other
/// two. All three are in the read-only image: no job can put a program in
/// any of them.
pub const JOB_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The name of a job, chosen by whoever asks for it.
///
/// 1 to [`JOB_ID_MAX_LEN`] ASCII letters, digits, `-`, `_` or `.`, and not
/// starting with a `.`: it is a file name in the session directory, and
/// can't be `.`, `..`, hidden, or a path.
///
/// Serializes as the bare string and deserializes through [`FromStr`], so
/// a name that isn't one never decodes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JobId(String);

impl JobId {
    /// The name, as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a job's name isn't one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadJobId(&'static str);

impl fmt::Display for BadJobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for BadJobId {}

impl FromStr for JobId {
    type Err = BadJobId;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw.is_empty() {
            return Err(BadJobId("a job's name is empty"));
        }
        if raw.len() > JOB_ID_MAX_LEN {
            return Err(BadJobId("a job's name is longer than 64 bytes"));
        }
        if raw.starts_with('.') {
            return Err(BadJobId("a job's name starts with a dot"));
        }
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if !raw.chars().all(plain) {
            return Err(BadJobId(
                "a job's name has something other than letters, digits, '-', '_' and '.'",
            ));
        }
        Ok(Self(raw.to_owned()))
    }
}

impl TryFrom<String> for JobId {
    type Error = BadJobId;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        raw.parse()
    }
}

impl From<JobId> for String {
    fn from(job: JobId) -> Self {
        job.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One of a job's two output streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl Stream {
    /// Both, in order.
    pub const BOTH: [Self; 2] = [Self::Stdout, Self::Stderr];

    /// Its name, in the wire's words and in its file's name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// Which kept piece of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum End {
    /// Its first [`PIECE_BYTES`].
    Head,
    /// Its last [`PIECE_BYTES`], of what came after the head.
    Tail,
}

/// The file, in the Room, that holds what `job` wrote to `stream`. The
/// name is made here from the job's name alone: nothing the supervisor
/// says chooses it.
#[must_use]
pub fn spill_path(job: &JobId, stream: Stream) -> String {
    format!("{JOBS_DIR}/{job}.{}", stream.as_str())
}

/// Whether `argv` is a command the Room may be asked to run: a program
/// named plainly (found on the Room's fixed path) or by an absolute path,
/// and arguments within [`ARGV_MAX`] and [`ARGV_BYTES`].
///
/// # Errors
///
/// Why it isn't, in words for whoever asked.
pub fn check_argv(argv: &[String]) -> Result<(), &'static str> {
    let Some(program) = argv.first() else {
        return Err("a Room command needs a program");
    };
    if program.is_empty() {
        return Err("a Room command needs a program");
    }
    if program.contains('/') && !program.starts_with('/') {
        return Err(
            "a Room program is named plainly or by an absolute path: files in the workspace don't run, so a script is run by naming its interpreter",
        );
    }
    if argv.len() > ARGV_MAX
        || argv.iter().map(String::len).sum::<usize>() > ARGV_BYTES
        || argv.iter().any(|argument| argument.contains('\0'))
    {
        return Err("a Room command's arguments are invalid or too long");
    }
    Ok(())
}

/// Whether `dir` may be asked for as a job's working directory: an
/// absolute path of at most [`DIR_BYTES`]. The Room decides whether it
/// is there.
///
/// # Errors
///
/// Why it isn't.
pub fn check_dir(dir: &str) -> Result<(), &'static str> {
    if !dir.starts_with('/') {
        return Err("a Room job's directory is an absolute path");
    }
    if dir.len() > DIR_BYTES || dir.contains('\0') {
        return Err("a Room job's directory is invalid or too long");
    }
    Ok(())
}

/// Whether a page may be asked for: `path` absolute and at most
/// [`PATH_BYTES`], lines counted from 1, and 1 to [`PAGE_LINES`] of them.
/// The Room decides whether the file is there.
///
/// # Errors
///
/// Why it may not.
pub fn check_page(path: &str, from: u64, lines: u32) -> Result<(), &'static str> {
    if !path.starts_with('/') {
        return Err("a Room file is named by its absolute path");
    }
    if path.len() > PATH_BYTES || path.contains('\0') {
        return Err("a Room file's path is invalid or too long");
    }
    if from == 0 {
        return Err("a file's lines are counted from 1");
    }
    if lines == 0 || lines > PAGE_LINES {
        return Err("a page is 1 to 2000 lines");
    }
    Ok(())
}

/// Whether a search may be asked for.
///
/// It may with a `pattern` of 1 to [`PATTERN_BYTES`] on one line, an
/// absolute `path` of at most [`PATH_BYTES`], and a `glob`, when there
/// is one, of 1 to [`GLOB_BYTES`] on one line. The Room decides whether
/// the path is there, and its search program whether the pattern is one.
///
/// # Errors
///
/// Why it may not.
pub fn check_search(pattern: &str, path: &str, glob: Option<&str>) -> Result<(), &'static str> {
    let one_line = |text: &str| !text.contains(['\0', '\n', '\r']);
    if pattern.is_empty() || pattern.len() > PATTERN_BYTES || !one_line(pattern) {
        return Err("a search's pattern is one line of 1 to 1024 bytes");
    }
    if !path.starts_with('/') {
        return Err("a search is of an absolute path in the Room");
    }
    if path.len() > PATH_BYTES || path.contains('\0') {
        return Err("a search's path is invalid or too long");
    }
    if glob.is_some_and(|glob| glob.is_empty() || glob.len() > GLOB_BYTES || !one_line(glob)) {
        return Err("a search's glob is one line of 1 to 256 bytes");
    }
    Ok(())
}

/// Whether `path` can name a file to be written or edited: absolute, at
/// most [`PATH_BYTES`], and not ending in `/`.
fn check_file(path: &str) -> Result<(), &'static str> {
    if !path.starts_with('/') {
        return Err("a Room file is named by its absolute path");
    }
    if path.len() > PATH_BYTES || path.contains('\0') {
        return Err("a Room file's path is invalid or too long");
    }
    if path.ends_with('/') {
        return Err("a Room file's path names a file, and this one a directory");
    }
    Ok(())
}

/// Whether a file may be asked to be written: at an absolute `path` of at
/// most [`PATH_BYTES`] that names a file, with at most [`WRITE_BYTES`].
/// The Room decides whether it can be written there.
///
/// # Errors
///
/// Why it may not.
pub fn check_write(path: &str, bytes: usize) -> Result<(), &'static str> {
    check_file(path)?;
    if bytes > WRITE_BYTES {
        return Err("a file is written with at most 32768 bytes at once");
    }
    Ok(())
}

/// Whether an edit may be asked for.
///
/// It may of the file at an absolute `path`, replacing `old`, of 1 to
/// [`EDIT_BYTES`], with `new`, of at most [`EDIT_BYTES`] and not the
/// same. The Room decides whether the file is there and has the text.
///
/// # Errors
///
/// Why it may not.
pub fn check_edit(path: &str, old: &[u8], new: &[u8]) -> Result<(), &'static str> {
    check_file(path)?;
    if old.is_empty() || old.len() > EDIT_BYTES {
        return Err("an edit replaces 1 to 8192 bytes of text");
    }
    if new.len() > EDIT_BYTES {
        return Err("an edit puts at most 8192 bytes in the text's place");
    }
    if old == new {
        return Err("an edit's new text is the text it replaces");
    }
    Ok(())
}

/// Whether a piece of a file may be asked to be taken.
///
/// It may from byte `at`, short of [`TAKE_FILE_BYTES`], of a file named by
/// an absolute path of at most [`PATH_BYTES`] under [`WORKSPACE`]. The
/// supervisor resolves the path beneath the workspace itself, and decides
/// whether it is a file.
///
/// This is no boundary: `.`, `..` and an empty step pass it. What keeps a
/// read in the workspace is the supervisor's open beneath it, as the
/// Room's user, through no link and onto no other file system.
///
/// # Errors
///
/// Why it may not.
pub fn check_take(path: &str, at: u64) -> Result<(), &'static str> {
    check_file(path)?;
    let under = path
        .strip_prefix(WORKSPACE)
        .is_some_and(|rest| rest.starts_with('/'));
    if !under {
        return Err("a file is handed over from the workspace: its path starts with /workspace/");
    }
    if at >= TAKE_FILE_BYTES {
        return Err("a file is handed over only if it is at most 8 MiB");
    }
    Ok(())
}

/// Whether `name` can be a name in a folder: 1 to [`NAME_BYTES`] bytes,
/// neither `/` nor NUL among them, and not `.` or `..`.
#[must_use]
pub fn is_name(name: &[u8]) -> bool {
    (1..=NAME_BYTES).contains(&name.len())
        && !name.iter().any(|&byte| byte == b'/' || byte == 0)
        && name != b"."
        && name != b".."
}

/// Whether a page of a folder may be asked to be listed.
///
/// It may of the workspace itself, or of a folder named by an absolute
/// path of at most [`PATH_BYTES`] beneath [`WORKSPACE`], none of its steps
/// empty, `.` or `..`, after `after`, a name or none.
///
/// The supervisor opens the folder beneath the workspace itself, through
/// no link and onto no other file system: that is what keeps a listing in
/// the workspace. This only refuses what no folder handed over is named
/// by.
///
/// # Errors
///
/// Why it may not.
pub fn check_list(path: &str, after: &[u8]) -> Result<(), &'static str> {
    if !path.starts_with('/') {
        return Err("a Room folder is named by its absolute path");
    }
    if path.len() > PATH_BYTES || path.contains('\0') {
        return Err("a Room folder's path is invalid or too long");
    }
    if !after.is_empty() && !is_name(after) {
        return Err("a listing goes on after a name, and this can't be one");
    }
    if path == WORKSPACE {
        return Ok(());
    }
    let Some(inner) = path
        .strip_prefix(WORKSPACE)
        .and_then(|rest| rest.strip_prefix('/'))
    else {
        return Err("a folder is handed over from the workspace: its path starts with /workspace/");
    };
    if inner
        .split('/')
        .any(|step| step.is_empty() || step == "." || step == "..")
    {
        return Err("a folder's path has a step that is empty, . or ..");
    }
    Ok(())
}

/// What the spawner asks the supervisor. Each carries a nonce the answer
/// must echo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Ask {
    /// Start `job`: `argv`, in `dir`, with `stdin` on its standard input.
    Start {
        /// Echoed by the answer.
        nonce: String,
        /// The job's name.
        job: JobId,
        /// The program and its arguments.
        argv: Vec<String>,
        /// The directory it starts in.
        dir: String,
        /// What it reads on its standard input.
        #[serde(with = "b64")]
        stdin: Vec<u8>,
    },
    /// Stop `job`: every process in its group is killed. Its end is said
    /// as any job's is.
    Stop {
        /// Echoed by the answer.
        nonce: String,
        /// The job's name.
        job: JobId,
    },
    /// Hand back a kept piece of what an ended `job` wrote.
    Piece {
        /// Echoed by the answer.
        nonce: String,
        /// The job's name.
        job: JobId,
        /// Which stream.
        stream: Stream,
        /// Which piece of it.
        end: End,
    },
    /// Hand back a page of the file at `path`: its lines from line `from`,
    /// at most `lines` of them and at most [`PAGE_BYTES`].
    Page {
        /// Echoed by the answer.
        nonce: String,
        /// The file's absolute path in the Room.
        path: String,
        /// The first line wanted, counted from 1.
        from: u64,
        /// The most lines wanted.
        lines: u32,
    },
    /// Hand back the lines that match `pattern` in the file at `path`, or
    /// in the files under it: at most [`FOUND_BYTES`] of them, found in
    /// at most [`SEARCH_MILLIS`].
    Search {
        /// Echoed by the answer.
        nonce: String,
        /// What to look for: a regular expression, or the very words
        /// when `literal` is set.
        pattern: String,
        /// The absolute path, in the Room, of the file or directory to
        /// look in.
        path: String,
        /// Only files whose names match this glob are looked in.
        glob: Option<String>,
        /// Whether letters match whatever their case.
        ignore_case: bool,
        /// Whether `pattern` is the very words to find, and no expression.
        literal: bool,
    },
    /// Write the file at `path` with `bytes`, whole: a new file, or one
    /// in place of the file there when `replace` is set.
    Write {
        /// Echoed by the answer.
        nonce: String,
        /// The file's absolute path in the Room.
        path: String,
        /// What the file is to hold.
        #[serde(with = "b64")]
        bytes: Vec<u8>,
        /// Whether a file already there is to be replaced.
        replace: bool,
    },
    /// Replace `old` with `new` in the file at `path`: at the one place
    /// it is, or at every place when `all` is set.
    Edit {
        /// Echoed by the answer.
        nonce: String,
        /// The file's absolute path in the Room.
        path: String,
        /// The text to replace, exactly as it is in the file.
        #[serde(with = "b64")]
        old: Vec<u8>,
        /// What to put in its place.
        #[serde(with = "b64")]
        new: Vec<u8>,
        /// Whether every place the text is at is to be replaced.
        all: bool,
    },
    /// Hand back a piece of the file at `path` in the workspace: at most
    /// [`TAKE_BYTES`] of it from byte `at`, with its length and which
    /// file it is.
    Take {
        /// Echoed by the answer.
        nonce: String,
        /// The file's absolute path in the Room, under the workspace.
        path: String,
        /// The first byte wanted, counted from 0.
        at: u64,
    },
    /// Hand back a page of what the folder at `path` in the workspace
    /// holds: at most [`LIST_PAGE`] of its names after `after`, in the
    /// order of their bytes.
    List {
        /// Echoed by the answer.
        nonce: String,
        /// The folder's absolute path in the Room, under the workspace.
        path: String,
        /// The last name of the page before; empty for the first page.
        #[serde(with = "b64")]
        after: Vec<u8>,
    },
}

impl Ask {
    /// The nonce the answer must echo.
    #[must_use]
    pub fn nonce(&self) -> &str {
        match self {
            Self::Start { nonce, .. }
            | Self::Stop { nonce, .. }
            | Self::Piece { nonce, .. }
            | Self::Page { nonce, .. }
            | Self::Search { nonce, .. }
            | Self::Write { nonce, .. }
            | Self::Edit { nonce, .. }
            | Self::Take { nonce, .. }
            | Self::List { nonce, .. } => nonce,
        }
    }
}

/// Why the supervisor didn't start a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotStarted {
    /// It has a job by that name already.
    Known,
    /// It is running as many jobs as it may.
    Full,
    /// The directory isn't there, or isn't one the Room's user may enter.
    NoSuchDir,
    /// The program couldn't be started.
    NoSuchProgram,
    /// It couldn't prepare the job: what it was given to read couldn't
    /// be put in the session directory, or its first process couldn't
    /// be watched.
    NoSession,
}

/// Why the supervisor handed back no page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoPage {
    /// There is no file at that path.
    NoSuchFile,
    /// The Room's user may not read it.
    Denied,
    /// It is a directory, or something else that isn't a file.
    NotAFile,
    /// The lines asked for have a byte no text has.
    NotText,
    /// The lines asked for aren't within [`PAGE_SCAN_BYTES`] of its start.
    TooFar,
    /// It has only this many lines, and the first one asked for is past
    /// them.
    PastEnd(u64),
    /// It couldn't be read.
    Failed,
}

/// Some of a file's lines, as the supervisor read them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    /// The lines, as they are in the file: each with its line end, but
    /// for a last line that has none.
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
    /// Whether the file goes on after them.
    pub more: bool,
    /// Whether they are the start of one line too long for a page, and
    /// no more of it.
    pub cut: bool,
    /// The file's length, in bytes.
    pub size: u64,
}

impl Page {
    /// Whether this can be a page: no longer than [`PAGE_BYTES`], and cut
    /// only when it is that long and the file goes on.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub fn check(&self) -> Result<(), &'static str> {
        if self.bytes.len() > PAGE_BYTES {
            return Err("a page of a file is too long");
        }
        if self.cut && (self.bytes.len() < PAGE_BYTES || !self.more) {
            return Err("a page is said to be cut that can't be");
        }
        Ok(())
    }
}

/// Why the supervisor handed back nothing of a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoSearch {
    /// There is nothing at that path.
    NoSuchPath,
    /// The search program wouldn't take the pattern or the glob, or
    /// couldn't look where it was sent.
    Refused,
    /// The search couldn't be run.
    Failed,
}

/// What a search found, as the supervisor read it from the search
/// program.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Found {
    /// The matching lines, each as `path:line:text` with its line end, in
    /// the order the search program found them. Nothing when nothing
    /// matched.
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
    /// Whether the search was stopped before it had looked everywhere:
    /// it had found more than fits, or its time ran out.
    pub more: bool,
}

impl Found {
    /// Whether this can be what a search found: no longer than
    /// [`FOUND_BYTES`], and whole lines.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub fn check(&self) -> Result<(), &'static str> {
        if self.bytes.len() > FOUND_BYTES {
            return Err("what a search found is too long");
        }
        if self.bytes.last().is_some_and(|last| *last != b'\n') {
            return Err("what a search found ends in the middle of a line");
        }
        Ok(())
    }
}

/// Why the supervisor wrote no file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoWrite {
    /// There is a file at that path, and it wasn't to be replaced.
    Exists,
    /// What is at that path is a directory, a link, or something else
    /// that isn't a file.
    NotAFile,
    /// The Room's user may not write there.
    Denied,
    /// It couldn't be written.
    Failed,
}

/// A file as the supervisor wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wrote {
    /// The file's length now, in bytes.
    pub size: u64,
    /// Whether there was a file there, which this one replaced.
    pub replaced: bool,
}

impl Wrote {
    /// Whether this can be what a write came to: a file no longer than
    /// one may be written with.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub fn check(&self) -> Result<(), &'static str> {
        if usize::try_from(self.size).map_or(true, |size| size > WRITE_BYTES) {
            return Err("a file is said to be written with more than one may be");
        }
        Ok(())
    }
}

/// Why the supervisor made no edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoEdit {
    /// There is no file at that path.
    NoSuchFile,
    /// The Room's user may not read it, or may not write there.
    Denied,
    /// It is a directory, a link, or something else that isn't a file.
    NotAFile,
    /// It isn't text: it has a byte no text has, or isn't UTF-8.
    NotText,
    /// It is longer than [`EDIT_FILE_BYTES`], or the edit would make it
    /// so.
    TooBig,
    /// The text to replace isn't in it.
    NotFound,
    /// The text to replace is in it at more than one place, and one was
    /// wanted.
    Ambiguous {
        /// How many places it is at.
        places: u32,
        /// The lines the first [`EDIT_PLACES`] of them start on, counted
        /// from 1, in order.
        lines: Vec<u64>,
    },
    /// It couldn't be read, or couldn't be written.
    Failed,
}

impl NoEdit {
    /// Whether this can be why no edit was made: text found at more than
    /// one place is at two or more, and names 1 to [`EDIT_PLACES`] of
    /// them, no more than there are, by lines counted from 1 that never
    /// go back.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub fn check(&self) -> Result<(), &'static str> {
        let Self::Ambiguous { places, lines } = self else {
            return Ok(());
        };
        let named = u32::try_from(lines.len()).unwrap_or(u32::MAX);
        if *places < 2 || lines.is_empty() || lines.len() > EDIT_PLACES || named > *places {
            return Err("an edit's text is said to be at places that can't be");
        }
        let in_order = lines
            .iter()
            .zip(lines.iter().skip(1))
            .all(|(line, next)| line <= next);
        if lines.first() == Some(&0) || !in_order {
            return Err("an edit's text is said to be on lines that can't be");
        }
        Ok(())
    }
}

/// An edit as the supervisor made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edited {
    /// How many places the text was replaced at.
    pub replaced: u32,
    /// The line the first of them starts on, counted from 1.
    pub line: u64,
    /// The file's length now, in bytes.
    pub size: u64,
}

impl Edited {
    /// Whether this can be what an edit came to: at least one place, on a
    /// line counted from 1, in a file no longer than one may be.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub const fn check(&self) -> Result<(), &'static str> {
        if self.replaced == 0 || self.line == 0 {
            return Err("an edit is said to be made at no place");
        }
        if self.size > EDIT_FILE_BYTES {
            return Err("an edit is said to leave a file longer than one may");
        }
        Ok(())
    }
}

/// Why the supervisor handed back no piece of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoTake {
    /// There is no file at that path.
    NoSuchFile,
    /// The Room's user may not read it.
    Denied,
    /// It is a directory, or something else that isn't a file.
    NotAFile,
    /// The path has a link in it: none is followed.
    Link,
    /// The path leads out of the workspace, or onto another file system.
    Outside,
    /// The file changed while the piece was read.
    Changed,
    /// It couldn't be read.
    Failed,
}

/// A piece of a file, as the supervisor read it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Took {
    /// The bytes, from the byte asked for.
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
    /// The file's length, in bytes.
    pub size: u64,
    /// The file's inode number.
    pub ino: u64,
    /// When the file last changed, its contents or what is said of it, in
    /// nanoseconds since the epoch.
    pub changed: u64,
}

impl Took {
    /// Whether this can be the piece of a file from byte `at`: no longer
    /// than [`TAKE_BYTES`], not past the file's end, and short of that
    /// only where the file ends.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub fn check(&self, at: u64) -> Result<(), &'static str> {
        if self.bytes.len() > TAKE_BYTES {
            return Err("a piece of a file is too long");
        }
        let end = at.saturating_add(u64::try_from(self.bytes.len()).unwrap_or(u64::MAX));
        if end > self.size {
            return Err("a piece of a file goes past the file's end");
        }
        if end < self.size && self.bytes.len() < TAKE_BYTES {
            return Err("a piece of a file is short where the file goes on");
        }
        Ok(())
    }
}

/// Why the supervisor handed back no page of a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoList {
    /// There is no folder at that path.
    NoSuchFolder,
    /// The Room's user may not read it, or what is in it.
    Denied,
    /// It is a file, or something else that isn't a folder.
    NotAFolder,
    /// The path has a link in it: none is followed.
    Link,
    /// The path leads out of the workspace, or onto another file system.
    Outside,
    /// It holds more than [`LIST_FOLDER_NAMES`] names.
    TooMany,
    /// It changed while the page was read.
    Changed,
    /// It couldn't be read.
    Failed,
}

/// What a name in a folder is, as the supervisor saw it, following no
/// link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A regular file.
    File,
    /// A folder.
    Folder,
    /// A link.
    Link,
    /// The root of another mount: what is there isn't the workspace's.
    Mount,
    /// A device, a pipe or a socket.
    Other,
}

/// A name in a folder, and what it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Its name, as the folder holds it.
    #[serde(with = "b64")]
    pub name: Vec<u8>,
    /// What it is.
    pub kind: Kind,
    /// Its inode number: of a file or a folder only, else 0.
    pub ino: u64,
    /// Its length in bytes: of a file only, else 0.
    pub size: u64,
}

/// A page of a folder's listing, as the supervisor read it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listed {
    /// The folder's inode number.
    pub ino: u64,
    /// When the folder last changed, in nanoseconds since the epoch.
    pub changed: u64,
    /// How many names the folder holds.
    pub total: u32,
    /// The names after the one asked from, in the order of their bytes.
    pub entries: Vec<Entry>,
}

impl Listed {
    /// Whether this can be a page of a folder after `after`: no more than
    /// [`LIST_PAGE`] names, of a folder of no more than
    /// [`LIST_FOLDER_NAMES`], each a name, each after the one before it
    /// and the first after `after`, and an inode or a length only where
    /// one is said.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub fn check(&self, after: &[u8]) -> Result<(), &'static str> {
        if self.total > LIST_FOLDER_NAMES {
            return Err("a folder is said to hold more names than one listed may");
        }
        let held = usize::try_from(self.total).unwrap_or(usize::MAX);
        if self.entries.len() > LIST_PAGE || self.entries.len() > held {
            return Err("a page of a folder holds too many names");
        }
        check_entries(&self.entries, after)
    }
}

/// Whether `entries` can be names of a folder after `after`, in order:
/// each a name, each after the one before it and the first after `after`,
/// and an inode or a length only where one is said.
///
/// # Errors
///
/// What is wrong with them.
pub fn check_entries(entries: &[Entry], after: &[u8]) -> Result<(), &'static str> {
    let mut before = after;
    for entry in entries {
        if !is_name(&entry.name) {
            return Err("a page of a folder holds what can't be a name");
        }
        if entry.name.as_slice() <= before {
            return Err("a page of a folder's names aren't in order, or go back");
        }
        before = &entry.name;
        let has_ino = matches!(entry.kind, Kind::File | Kind::Folder);
        if !has_ino && entry.ino != 0 {
            return Err("a page of a folder gives an inode to what has none here");
        }
        if entry.kind != Kind::File && entry.size != 0 {
            return Err("a page of a folder gives a length to what isn't a file");
        }
    }
    Ok(())
}

/// What the supervisor says. All but [`Say::Ended`] answer an [`Ask`],
/// and echo its nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "say", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Say {
    /// The job was started.
    Started {
        /// The ask's nonce.
        nonce: String,
    },
    /// The job wasn't started.
    NotStarted {
        /// The ask's nonce.
        nonce: String,
        /// Why not.
        why: NotStarted,
    },
    /// The job's processes were sent the kill; its end follows.
    Stopping {
        /// The ask's nonce.
        nonce: String,
    },
    /// There's no such job, or it isn't in the state the ask needs: a
    /// stop of one that has ended, a piece of one still running.
    NoJob {
        /// The ask's nonce.
        nonce: String,
    },
    /// A kept piece of what a job wrote.
    Piece {
        /// The ask's nonce.
        nonce: String,
        /// The piece.
        #[serde(with = "b64")]
        bytes: Vec<u8>,
    },
    /// A page of a file.
    Page {
        /// The ask's nonce.
        nonce: String,
        /// The page.
        page: Page,
    },
    /// No page of the file was handed back.
    NoPage {
        /// The ask's nonce.
        nonce: String,
        /// Why not.
        why: NoPage,
    },
    /// What a search found.
    Found {
        /// The ask's nonce.
        nonce: String,
        /// What it found.
        found: Found,
    },
    /// Nothing of the search was handed back.
    NoSearch {
        /// The ask's nonce.
        nonce: String,
        /// Why not.
        why: NoSearch,
    },
    /// The file was written.
    Wrote {
        /// The ask's nonce.
        nonce: String,
        /// What was written.
        wrote: Wrote,
    },
    /// No file was written.
    NoWrite {
        /// The ask's nonce.
        nonce: String,
        /// Why not.
        why: NoWrite,
    },
    /// The edit was made.
    Edited {
        /// The ask's nonce.
        nonce: String,
        /// What was made.
        edited: Edited,
    },
    /// No edit was made: the file is as it was.
    NoEdit {
        /// The ask's nonce.
        nonce: String,
        /// Why not.
        why: NoEdit,
    },
    /// A piece of a file.
    Took {
        /// The ask's nonce.
        nonce: String,
        /// The piece.
        took: Took,
    },
    /// No piece of the file was handed back.
    NoTake {
        /// The ask's nonce.
        nonce: String,
        /// Why not.
        why: NoTake,
    },
    /// A page of a folder's listing.
    Listed {
        /// The ask's nonce.
        nonce: String,
        /// The page.
        listed: Listed,
    },
    /// No page of the folder was handed back.
    NoList {
        /// The ask's nonce.
        nonce: String,
        /// Why not.
        why: NoList,
    },
    /// A job has ended. Said once for each job that was started, unasked.
    Ended {
        /// The job's name.
        job: JobId,
        /// How its first process ended.
        how: Ended,
        /// What it wrote to standard output.
        stdout: Captured,
        /// What it wrote to standard error.
        stderr: Captured,
    },
}

impl Say {
    /// The nonce this echoes, when it answers an ask.
    #[must_use]
    pub fn nonce(&self) -> Option<&str> {
        match self {
            Self::Started { nonce }
            | Self::NotStarted { nonce, .. }
            | Self::Stopping { nonce }
            | Self::NoJob { nonce }
            | Self::Piece { nonce, .. }
            | Self::Page { nonce, .. }
            | Self::NoPage { nonce, .. }
            | Self::Found { nonce, .. }
            | Self::NoSearch { nonce, .. }
            | Self::Wrote { nonce, .. }
            | Self::NoWrite { nonce, .. }
            | Self::Edited { nonce, .. }
            | Self::NoEdit { nonce, .. }
            | Self::Took { nonce, .. }
            | Self::NoTake { nonce, .. }
            | Self::Listed { nonce, .. }
            | Self::NoList { nonce, .. } => Some(nonce),
            Self::Ended { .. } => None,
        }
    }
}

/// How a job's first process ended, as the supervisor says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ended {
    /// It returned this exit code.
    Exited(u8),
    /// It was ended by this signal.
    Signaled(u8),
}

impl Ended {
    /// Whether this is an end a process can have: a signal is 1 to 64.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub const fn check(self) -> Result<(), &'static str> {
        match self {
            Self::Exited(_) | Self::Signaled(1..=64) => Ok(()),
            Self::Signaled(_) => Err("a job ended by a signal that isn't one"),
        }
    }
}

/// How much of a stream was kept, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Captured {
    /// Bytes kept from its start.
    pub head: u32,
    /// Bytes kept from its end.
    pub tail: u32,
    /// Bytes between the two that weren't kept in memory.
    pub dropped: u64,
    /// Bytes of it written to its file.
    pub spilled: u64,
    /// Whether its file holds less than all of it.
    pub spill_cut: bool,
}

impl Captured {
    /// All the stream came to, in bytes.
    #[must_use]
    pub fn total(&self) -> u64 {
        u64::from(self.head)
            .saturating_add(self.dropped)
            .saturating_add(u64::from(self.tail))
    }

    /// Whether these counts can be true together: neither piece is over
    /// [`PIECE_BYTES`]; there's a tail only past a full head, and bytes
    /// dropped only between two full pieces; the file holds no more than
    /// [`SPILL_BYTES`] nor than the stream, and all of it unless it's
    /// said to be cut.
    ///
    /// # Errors
    ///
    /// What is wrong with them.
    pub fn check(&self) -> Result<(), &'static str> {
        let piece = u32::try_from(PIECE_BYTES).unwrap_or(u32::MAX);
        if self.head > piece || self.tail > piece {
            return Err("a kept piece of a job's output is too long");
        }
        if self.tail > 0 && self.head < piece {
            return Err("a job's output has a tail without a full head");
        }
        if self.dropped > 0 && self.tail < piece {
            return Err("a job's output dropped bytes without a full tail");
        }
        let total = self.total();
        if self.spilled > SPILL_BYTES || self.spilled > total {
            return Err("a job's output file holds more than it can");
        }
        if self.spill_cut == (self.spilled == total) {
            return Err("a job's output file isn't cut as it's said to be");
        }
        Ok(())
    }
}

/// Bytes on the wire as base64 text, for `#[serde(with = ...)]`: a frame
/// is JSON, and a list of numbers is four times the bytes it stands for.
pub mod b64 {
    use serde::{Deserialize as _, Deserializer, Serializer, de};

    /// Writes `bytes` as base64 text.
    ///
    /// # Errors
    ///
    /// The serializer's own.
    pub fn serialize<S: Serializer>(bytes: &[u8], to: S) -> Result<S::Ok, S::Error> {
        to.serialize_str(&crate::secret::encode(bytes))
    }

    /// Reads bytes written as base64 text.
    ///
    /// # Errors
    ///
    /// When what is there isn't text, or isn't base64.
    pub fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(from)?;
        crate::secret::decode(&text).map_err(|_| de::Error::custom("bytes that aren't base64"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(name: &str) -> JobId {
        name.parse().unwrap()
    }

    #[test]
    fn the_bounds_are_as_they_are_written() {
        assert_eq!(JOB_ID_MAX_LEN, 64);
        assert_eq!(PIECE_BYTES, 16_384);
        assert_eq!(SPILL_BYTES, 8_388_608);
        assert_eq!(STDIN_BYTES, 32_768);
        assert_eq!(ARGV_MAX, 64);
        assert_eq!(ARGV_BYTES, 8_192);
        assert_eq!(DIR_BYTES, 1_024);
        assert_eq!(RUNNING_MAX, 4);
        assert_eq!(PAGE_BYTES, 8_192);
        assert_eq!(PAGE_LINES, 2_000);
        assert_eq!(PAGE_SCAN_BYTES, 33_554_432);
        assert_eq!(PATH_BYTES, 1_024);
    }

    #[test]
    fn a_jobs_name_is_a_plain_file_name() {
        for good in ["a", "build-1", "run_2.log", "A.b-c_d", &"x".repeat(64)] {
            assert_eq!(job(good).as_str(), good);
            assert_eq!(job(good).to_string(), good);
        }
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "a/b",
            "a b",
            "a\0",
            "caf\u{e9}",
            &"x".repeat(65),
        ] {
            assert!(bad.parse::<JobId>().is_err(), "{bad:?} was taken");
        }
        // Each refusal says which rule: a caller is told what to change.
        assert_eq!(
            "".parse::<JobId>().unwrap_err().to_string(),
            "a job's name is empty"
        );
        assert!(
            "x".repeat(65)
                .parse::<JobId>()
                .unwrap_err()
                .to_string()
                .contains("longer")
        );
        assert!(
            ".x".parse::<JobId>()
                .unwrap_err()
                .to_string()
                .contains("dot")
        );
        assert!(
            "a/b"
                .parse::<JobId>()
                .unwrap_err()
                .to_string()
                .contains("letters")
        );
    }

    #[test]
    fn a_name_that_is_not_one_never_decodes() {
        assert_eq!(serde_json::to_string(&job("j-1")).unwrap(), r#""j-1""#);
        assert_eq!(
            serde_json::from_str::<JobId>(r#""j-1""#).unwrap(),
            job("j-1")
        );
        assert!(serde_json::from_str::<JobId>(r#""../x""#).is_err());
        assert_eq!(String::from(job("j-1")), "j-1");
    }

    #[test]
    fn a_streams_file_is_named_from_the_jobs_name_alone() {
        assert_eq!(
            spill_path(&job("build-1"), Stream::Stdout),
            "/session/jobs/build-1.stdout"
        );
        assert_eq!(
            spill_path(&job("build-1"), Stream::Stderr),
            "/session/jobs/build-1.stderr"
        );
        assert_eq!(Stream::BOTH, [Stream::Stdout, Stream::Stderr]);
        assert_eq!(JOBS_DIR, [SESSION_DIR, "jobs"].join("/"));
        assert_eq!(JOB_PATH, "/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn a_command_is_a_plain_or_absolute_program_and_bounded_arguments() {
        let argv = |parts: &[&str]| {
            parts
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>()
        };
        assert!(check_argv(&argv(&["python3", "-c", "print(1)"])).is_ok());
        assert!(check_argv(&argv(&["/usr/bin/python3"])).is_ok());
        assert!(check_argv(&[]).is_err());
        assert!(check_argv(&argv(&[""])).is_err());
        for relative in ["./run.sh", "bin/tool", "../x"] {
            assert!(
                check_argv(&argv(&[relative]))
                    .unwrap_err()
                    .contains("interpreter"),
                "{relative} was taken"
            );
        }
        let mut many = vec![String::from("/bin/true"); ARGV_MAX];
        assert!(check_argv(&many).is_ok());
        many.push(String::new());
        assert!(check_argv(&many).is_err());
        let long = format!("/{}", "x".repeat(ARGV_BYTES - 1));
        assert!(check_argv(std::slice::from_ref(&long)).is_ok());
        assert!(check_argv(&[long, String::from("x")]).is_err());
        assert!(check_argv(&argv(&["/bin/true", "\0"])).is_err());
    }

    #[test]
    fn a_directory_is_absolute_and_bounded() {
        assert!(check_dir("/workspace").is_ok());
        assert!(check_dir(WORKSPACE).is_ok());
        assert!(check_dir("workspace").is_err());
        assert!(check_dir("").is_err());
        assert!(check_dir("/a\0").is_err());
        let longest = format!("/{}", "d".repeat(DIR_BYTES - 1));
        assert!(check_dir(&longest).is_ok());
        let over = format!("{longest}d");
        assert!(check_dir(&over).is_err());
    }

    #[test]
    fn a_page_is_of_an_absolute_path_from_a_line_that_is_one() {
        assert!(check_page("/workspace/notes.txt", 1, 1).is_ok());
        assert!(check_page("/workspace/notes.txt", u64::MAX, PAGE_LINES).is_ok());
        assert!(
            check_page("notes.txt", 1, 1)
                .unwrap_err()
                .contains("absolute")
        );
        assert!(check_page("", 1, 1).unwrap_err().contains("absolute"));
        assert!(check_page("/a\0", 1, 1).unwrap_err().contains("path"));
        let longest = format!("/{}", "f".repeat(PATH_BYTES - 1));
        assert!(check_page(&longest, 1, 1).is_ok());
        let over = format!("{longest}f");
        assert!(check_page(&over, 1, 1).unwrap_err().contains("path"));
        assert!(
            check_page("/a", 0, 1)
                .unwrap_err()
                .contains("counted from 1")
        );
        assert!(check_page("/a", 1, 0).unwrap_err().contains("2000 lines"));
        assert!(
            check_page("/a", 1, PAGE_LINES + 1)
                .unwrap_err()
                .contains("2000 lines")
        );
    }

    #[test]
    fn a_page_is_no_longer_than_one_and_cut_only_when_full() {
        let page = |length: usize, more: bool, cut: bool| Page {
            bytes: vec![b'x'; length],
            more,
            cut,
            size: 0,
        };
        assert!(Page::default().check().is_ok());
        assert!(page(PAGE_BYTES, false, false).check().is_ok());
        assert!(page(PAGE_BYTES, true, false).check().is_ok());
        assert!(page(PAGE_BYTES, true, true).check().is_ok());
        assert!(
            page(PAGE_BYTES + 1, true, false)
                .check()
                .unwrap_err()
                .contains("too long")
        );
        // A line is cut only where it filled the page, and went on.
        assert!(
            page(PAGE_BYTES - 1, true, true)
                .check()
                .unwrap_err()
                .contains("cut")
        );
        assert!(
            page(PAGE_BYTES, false, true)
                .check()
                .unwrap_err()
                .contains("cut")
        );
    }

    #[test]
    fn the_wire_is_as_written_and_takes_nothing_unknown() {
        let start = Ask::Start {
            nonce: String::from("n1"),
            job: job("j"),
            argv: vec![String::from("/bin/true")],
            dir: String::from("/workspace"),
            stdin: b"hi\xff".to_vec(),
        };
        let wire = r#"{"ask":"start","nonce":"n1","job":"j","argv":["/bin/true"],"dir":"/workspace","stdin":"aGn/"}"#;
        assert_eq!(serde_json::to_string(&start).unwrap(), wire);
        assert_eq!(serde_json::from_str::<Ask>(wire).unwrap(), start);
        assert_eq!(start.nonce(), "n1");
        let asks = [
            (
                Ask::Stop {
                    nonce: String::from("n2"),
                    job: job("j"),
                },
                r#"{"ask":"stop","nonce":"n2","job":"j"}"#,
                "n2",
            ),
            (
                Ask::Piece {
                    nonce: String::from("n3"),
                    job: job("j"),
                    stream: Stream::Stderr,
                    end: End::Tail,
                },
                r#"{"ask":"piece","nonce":"n3","job":"j","stream":"stderr","end":"tail"}"#,
                "n3",
            ),
            (
                Ask::Page {
                    nonce: String::from("n4"),
                    path: String::from("/workspace/a.txt"),
                    from: 3,
                    lines: 200,
                },
                r#"{"ask":"page","nonce":"n4","path":"/workspace/a.txt","from":3,"lines":200}"#,
                "n4",
            ),
        ];
        for (ask, wire, nonce) in asks {
            assert_eq!(serde_json::to_string(&ask).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Ask>(wire).unwrap(), ask);
            assert_eq!(ask.nonce(), nonce);
        }
        for refused in [
            r#"{"ask":"stop","nonce":"n","job":"j","extra":1}"#,
            r#"{"ask":"stop","nonce":"n","job":"../j"}"#,
            r#"{"ask":"run","nonce":"n","job":"j"}"#,
            r#"{"ask":"start","nonce":"n","job":"j","argv":[],"dir":"/","stdin":"not base64!"}"#,
            r#"{"ask":"piece","nonce":"n","job":"j","stream":"stdin","end":"head"}"#,
            r#"{"ask":"page","nonce":"n","path":"/a","from":1,"lines":1,"bytes":9}"#,
            r#"{"ask":"page","nonce":"n","path":"/a","from":-1,"lines":1}"#,
        ] {
            assert!(
                serde_json::from_str::<Ask>(refused).is_err(),
                "taken: {refused}"
            );
        }
    }

    #[test]
    fn what_the_supervisor_says_is_as_written_and_takes_nothing_unknown() {
        let nonce = || String::from("n");
        let said = [
            (
                Say::Started { nonce: nonce() },
                r#"{"say":"started","nonce":"n"}"#,
            ),
            (
                Say::NotStarted {
                    nonce: nonce(),
                    why: NotStarted::NoSuchProgram,
                },
                r#"{"say":"not-started","nonce":"n","why":"no-such-program"}"#,
            ),
            (
                Say::Stopping { nonce: nonce() },
                r#"{"say":"stopping","nonce":"n"}"#,
            ),
            (
                Say::NoJob { nonce: nonce() },
                r#"{"say":"no-job","nonce":"n"}"#,
            ),
            (
                Say::Piece {
                    nonce: nonce(),
                    bytes: vec![0, 255],
                },
                r#"{"say":"piece","nonce":"n","bytes":"AP8="}"#,
            ),
        ];
        for (say, wire) in said {
            assert_eq!(serde_json::to_string(&say).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Say>(wire).unwrap(), say);
            assert_eq!(say.nonce(), Some("n"));
        }
        let ended = Say::Ended {
            job: job("j"),
            how: Ended::Exited(3),
            stdout: Captured {
                head: 5,
                ..Captured::default()
            },
            stderr: Captured::default(),
        };
        let wire = concat!(
            r#"{"say":"ended","job":"j","how":{"exited":3},"#,
            r#""stdout":{"head":5,"tail":0,"dropped":0,"spilled":0,"spill_cut":false},"#,
            r#""stderr":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false}}"#
        );
        assert_eq!(serde_json::to_string(&ended).unwrap(), wire);
        assert_eq!(serde_json::from_str::<Say>(wire).unwrap(), ended);
        assert_eq!(ended.nonce(), None);
        for why in [
            (NotStarted::Known, r#""known""#),
            (NotStarted::Full, r#""full""#),
            (NotStarted::NoSuchDir, r#""no-such-dir""#),
            (NotStarted::NoSession, r#""no-session""#),
        ] {
            assert_eq!(serde_json::to_string(&why.0).unwrap(), why.1);
        }
        for refused in [
            r#"{"say":"started","nonce":"n","job":"j"}"#,
            r#"{"say":"ended","job":"j","how":{"exited":256},"stdout":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false},"stderr":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false}}"#,
            r#"{"say":"ended","job":"j","how":{"signaled":9},"stdout":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false,"more":1},"stderr":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false}}"#,
            r#"{"say":"piece","nonce":"n","bytes":[1,2]}"#,
            r#"{"say":"done","nonce":"n"}"#,
        ] {
            assert!(
                serde_json::from_str::<Say>(refused).is_err(),
                "taken: {refused}"
            );
        }
    }

    #[test]
    fn a_page_is_said_as_written_and_takes_nothing_unknown() {
        let nonce = || String::from("n");
        let said = [
            (
                Say::Page {
                    nonce: nonce(),
                    page: Page {
                        bytes: b"a\nb".to_vec(),
                        more: true,
                        cut: false,
                        size: 9,
                    },
                },
                r#"{"say":"page","nonce":"n","page":{"bytes":"YQpi","more":true,"cut":false,"size":9}}"#,
            ),
            (
                Say::NoPage {
                    nonce: nonce(),
                    why: NoPage::PastEnd(3),
                },
                r#"{"say":"no-page","nonce":"n","why":{"past-end":3}}"#,
            ),
            (
                Say::NoPage {
                    nonce: nonce(),
                    why: NoPage::NoSuchFile,
                },
                r#"{"say":"no-page","nonce":"n","why":"no-such-file"}"#,
            ),
        ];
        for (say, wire) in said {
            assert_eq!(serde_json::to_string(&say).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Say>(wire).unwrap(), say);
            assert_eq!(say.nonce(), Some("n"));
        }
        for why in [
            (NoPage::Denied, r#""denied""#),
            (NoPage::NotAFile, r#""not-a-file""#),
            (NoPage::NotText, r#""not-text""#),
            (NoPage::TooFar, r#""too-far""#),
            (NoPage::Failed, r#""failed""#),
        ] {
            assert_eq!(serde_json::to_string(&why.0).unwrap(), why.1);
        }
        for refused in [
            r#"{"say":"page","nonce":"n","page":{"bytes":"","more":false,"cut":false,"size":0,"lines":1}}"#,
            r#"{"say":"page","nonce":"n","page":{"bytes":"","more":false,"cut":false}}"#,
            r#"{"say":"no-page","nonce":"n","why":"gone"}"#,
        ] {
            assert!(
                serde_json::from_str::<Say>(refused).is_err(),
                "taken: {refused}"
            );
        }
    }

    #[test]
    fn an_end_is_an_exit_code_or_a_real_signal() {
        assert!(Ended::Exited(0).check().is_ok());
        assert!(Ended::Exited(255).check().is_ok());
        assert!(Ended::Signaled(1).check().is_ok());
        assert!(Ended::Signaled(64).check().is_ok());
        assert!(Ended::Signaled(0).check().is_err());
        assert!(Ended::Signaled(65).check().is_err());
    }

    #[test]
    fn kept_counts_must_be_true_together() {
        let piece = u32::try_from(PIECE_BYTES).unwrap();
        let whole = |head: u32, tail: u32, dropped: u64| {
            let total = u64::from(head) + u64::from(tail) + dropped;
            Captured {
                head,
                tail,
                dropped,
                spilled: total,
                spill_cut: false,
            }
        };
        assert!(Captured::default().check().is_ok());
        assert!(whole(1, 0, 0).check().is_ok());
        assert!(whole(piece, 0, 0).check().is_ok());
        assert!(whole(piece, 1, 0).check().is_ok());
        assert!(whole(piece, piece, 0).check().is_ok());
        assert!(whole(piece, piece, 1).check().is_ok());
        assert_eq!(whole(piece, piece, 7).total(), u64::from(piece) * 2 + 7);
        // A piece over its length.
        assert!(whole(piece + 1, 0, 0).check().is_err());
        assert!(whole(piece, piece + 1, 0).check().is_err());
        // A tail without a full head, and a gap without a full tail.
        assert!(whole(piece - 1, 1, 0).check().is_err());
        assert!(whole(piece, piece - 1, 1).check().is_err());
        // The file: no more than its cap, no more than the stream, and
        // cut exactly when it's short.
        let big = whole(piece, piece, SPILL_BYTES);
        assert!(big.check().is_err(), "a file over its cap");
        let cut = Captured {
            spilled: SPILL_BYTES,
            spill_cut: true,
            ..big
        };
        assert!(cut.check().is_ok());
        assert!(
            Captured {
                spilled: SPILL_BYTES + 1,
                ..cut
            }
            .check()
            .is_err()
        );
        let small = whole(3, 0, 0);
        assert!(
            Captured {
                spilled: 4,
                ..small
            }
            .check()
            .is_err(),
            "a file longer than its stream"
        );
        assert!(
            Captured {
                spilled: 2,
                ..small
            }
            .check()
            .is_err(),
            "short, and not said to be cut"
        );
        assert!(
            Captured {
                spilled: 2,
                spill_cut: true,
                ..small
            }
            .check()
            .is_ok()
        );
        assert!(
            Captured {
                spill_cut: true,
                ..small
            }
            .check()
            .is_err(),
            "whole, and said to be cut"
        );
    }

    #[test]
    fn a_start_with_the_most_it_may_carry_fits_one_frame() {
        // The longest plain command, directory and input: what a caller
        // may ask for at most always fits.
        let start = Ask::Start {
            nonce: "f".repeat(64),
            job: job(&"j".repeat(JOB_ID_MAX_LEN)),
            argv: vec![format!("/{}", "a".repeat(ARGV_BYTES - 1))],
            dir: format!("/{}", "d".repeat(DIR_BYTES - 1)),
            stdin: vec![0xff; STDIN_BYTES],
        };
        assert!(crate::fits_one_frame(&start));
        let piece = Say::Piece {
            nonce: "f".repeat(64),
            bytes: vec![0xff; PIECE_BYTES],
        };
        assert!(crate::fits_one_frame(&piece));
    }

    /// A search is of one line of pattern, an absolute path, and at most
    /// one line of glob; what may be asked for at most fits one frame.
    #[test]
    fn a_search_is_of_a_pattern_an_absolute_path_and_perhaps_a_glob() {
        assert_eq!(
            (
                FOUND_BYTES,
                FOUND_LINE_BYTES,
                SEARCH_MILLIS,
                PATTERN_BYTES,
                GLOB_BYTES
            ),
            (8_192, 400, 5_000, 1_024, 256)
        );
        assert!(check_search("TODO", "/workspace", None).is_ok());
        assert!(check_search("a|b", "/", Some("*.rs")).is_ok());
        let longest = "p".repeat(PATTERN_BYTES);
        let deepest = format!("/{}", "d".repeat(PATH_BYTES - 1));
        let widest = "g".repeat(GLOB_BYTES);
        assert!(check_search(&longest, &deepest, Some(&widest)).is_ok());
        for pattern in ["", "a\nb", "a\rb", "a\0b", &format!("{longest}p")] {
            assert!(
                check_search(pattern, "/workspace", None)
                    .unwrap_err()
                    .contains("pattern"),
                "{pattern:?}"
            );
        }
        assert!(
            check_search("x", "workspace", None)
                .unwrap_err()
                .contains("absolute")
        );
        assert!(
            check_search("x", "", None)
                .unwrap_err()
                .contains("absolute")
        );
        for path in ["/a\0", &format!("{deepest}d")] {
            assert!(
                check_search("x", path, None).unwrap_err().contains("path"),
                "{path:?}"
            );
        }
        for glob in ["", "a\nb", "a\0", &format!("{widest}g")] {
            assert!(
                check_search("x", "/workspace", Some(glob))
                    .unwrap_err()
                    .contains("glob"),
                "{glob:?}"
            );
        }
        let most = Ask::Search {
            nonce: "f".repeat(64),
            pattern: "\"".repeat(PATTERN_BYTES),
            path: deepest,
            glob: Some(widest),
            ignore_case: true,
            literal: true,
        };
        assert!(crate::fits_one_frame(&most));
    }

    /// A search and its answers go on the wire as they are written here,
    /// each echoes its nonce, and a field that isn't named never decodes.
    #[test]
    fn a_search_and_its_answers_have_their_wire_form() {
        let ask = Ask::Search {
            nonce: String::from("n1"),
            pattern: String::from("TODO"),
            path: String::from("/workspace"),
            glob: None,
            ignore_case: false,
            literal: true,
        };
        let json = serde_json::to_string(&ask).unwrap();
        assert_eq!(
            json,
            r#"{"ask":"search","nonce":"n1","pattern":"TODO","path":"/workspace","glob":null,"ignore_case":false,"literal":true}"#
        );
        assert_eq!(serde_json::from_str::<Ask>(&json).unwrap(), ask);
        assert_eq!(ask.nonce(), "n1");
        assert!(
            serde_json::from_str::<Ask>(&json.replace("\"literal\"", "\"shell\":true,\"literal\""))
                .is_err()
        );
        let found = Say::Found {
            nonce: String::from("n1"),
            found: Found {
                bytes: b"/workspace/a:1:TODO\n".to_vec(),
                more: true,
            },
        };
        let json = serde_json::to_string(&found).unwrap();
        assert_eq!(
            json,
            r#"{"say":"found","nonce":"n1","found":{"bytes":"L3dvcmtzcGFjZS9hOjE6VE9ETwo=","more":true}}"#
        );
        assert_eq!(serde_json::from_str::<Say>(&json).unwrap(), found);
        assert_eq!(found.nonce(), Some("n1"));
        assert!(
            serde_json::from_str::<Say>(&json.replace("\"more\"", "\"job\":1,\"more\"")).is_err()
        );
        for (why, word) in [
            (NoSearch::NoSuchPath, "no-such-path"),
            (NoSearch::Refused, "refused"),
            (NoSearch::Failed, "failed"),
        ] {
            let none = Say::NoSearch {
                nonce: String::from("n2"),
                why,
            };
            let json = serde_json::to_string(&none).unwrap();
            assert_eq!(
                json,
                format!(r#"{{"say":"no-search","nonce":"n2","why":"{word}"}}"#)
            );
            assert_eq!(serde_json::from_str::<Say>(&json).unwrap(), none);
            assert_eq!(none.nonce(), Some("n2"));
        }
    }

    /// What a search found is no longer than it may be, and whole lines;
    /// the most it may be fits one frame.
    #[test]
    fn what_a_search_found_is_bounded_and_in_whole_lines() {
        let found = |bytes: &[u8]| Found {
            bytes: bytes.to_vec(),
            more: false,
        };
        assert!(Found::default().check().is_ok(), "nothing matched");
        assert!(found(b"/a:1:x\n").check().is_ok());
        assert!(
            found(b"/a:1:x\n/b:2:y")
                .check()
                .unwrap_err()
                .contains("middle of a line")
        );
        let mut most = vec![b'x'; FOUND_BYTES - 1];
        most.push(b'\n');
        assert!(found(&most).check().is_ok());
        most.insert(0, b'x');
        assert!(found(&most).check().unwrap_err().contains("too long"));
        let largest = Say::Found {
            nonce: "f".repeat(64),
            found: Found {
                bytes: vec![0xff; FOUND_BYTES],
                more: true,
            },
        };
        assert!(crate::fits_one_frame(&largest));
    }

    /// A write and an edit are each of a file named by its whole path;
    /// a write carries no more than it may, and an edit two texts that
    /// differ, the one to find not empty. The most of each fits one
    /// frame.
    #[test]
    fn a_write_and_an_edit_are_of_a_file_named_whole_and_bounded() {
        assert_eq!(
            (WRITE_BYTES, EDIT_BYTES, EDIT_FILE_BYTES, EDIT_PLACES),
            (32_768, 8_192, 4_194_304, 8)
        );
        let deepest = format!("/{}", "\u{1}".repeat(PATH_BYTES - 1));
        assert!(check_write("/workspace/notes.txt", 0).is_ok());
        assert!(check_write(&deepest, WRITE_BYTES).is_ok());
        assert!(
            check_write("/a", WRITE_BYTES + 1)
                .unwrap_err()
                .contains("32768 bytes")
        );
        assert!(check_edit("/a", b"x", b"y").is_ok());
        assert!(check_edit("/a", b"x", b"").is_ok(), "text taken out");
        let longest = vec![b'x'; EDIT_BYTES];
        let other = vec![b'y'; EDIT_BYTES];
        assert!(check_edit(&deepest, &longest, &other).is_ok());
        let over = vec![b'x'; EDIT_BYTES + 1];
        for old in [&b""[..], &over[..]] {
            assert!(
                check_edit("/a", old, b"y")
                    .unwrap_err()
                    .contains("replaces 1 to 8192"),
                "{} bytes",
                old.len()
            );
        }
        assert!(
            check_edit("/a", b"x", &over)
                .unwrap_err()
                .contains("at most 8192")
        );
        assert!(
            check_edit("/a", b"same", b"same")
                .unwrap_err()
                .contains("the text it replaces")
        );
        let too_deep = format!("{deepest}d");
        for (path, why) in [
            ("notes.txt", "absolute"),
            ("", "absolute"),
            ("/a\0", "invalid or too long"),
            (too_deep.as_str(), "invalid or too long"),
            ("/workspace/", "a directory"),
            ("/", "a directory"),
        ] {
            assert!(check_write(path, 1).unwrap_err().contains(why), "{path:?}");
            assert!(
                check_edit(path, b"x", b"y").unwrap_err().contains(why),
                "{path:?}"
            );
        }
        let most = Ask::Write {
            nonce: "f".repeat(64),
            path: deepest.clone(),
            bytes: vec![0xff; WRITE_BYTES],
            replace: true,
        };
        assert!(crate::fits_one_frame(&most));
        let most = Ask::Edit {
            nonce: "f".repeat(64),
            path: deepest,
            old: vec![0xff; EDIT_BYTES],
            new: vec![0xfe; EDIT_BYTES],
            all: true,
        };
        assert!(crate::fits_one_frame(&most));
    }

    /// A write and an edit go on the wire as they are written here, each
    /// with its nonce, and a field that isn't named never decodes.
    #[test]
    fn a_write_and_an_edit_have_their_wire_form() {
        let write = Ask::Write {
            nonce: String::from("n1"),
            path: String::from("/workspace/a.txt"),
            bytes: b"hi\n".to_vec(),
            replace: false,
        };
        let json = serde_json::to_string(&write).unwrap();
        assert_eq!(
            json,
            r#"{"ask":"write","nonce":"n1","path":"/workspace/a.txt","bytes":"aGkK","replace":false}"#
        );
        assert_eq!(serde_json::from_str::<Ask>(&json).unwrap(), write);
        assert_eq!(write.nonce(), "n1");
        assert!(
            serde_json::from_str::<Ask>(&json.replace("\"replace\"", "\"mode\":511,\"replace\""))
                .is_err()
        );
        let edit = Ask::Edit {
            nonce: String::from("n2"),
            path: String::from("/workspace/a.txt"),
            old: b"hi".to_vec(),
            new: b"yo".to_vec(),
            all: true,
        };
        let json = serde_json::to_string(&edit).unwrap();
        assert_eq!(
            json,
            r#"{"ask":"edit","nonce":"n2","path":"/workspace/a.txt","old":"aGk=","new":"eW8=","all":true}"#
        );
        assert_eq!(serde_json::from_str::<Ask>(&json).unwrap(), edit);
        assert_eq!(edit.nonce(), "n2");
        assert!(
            serde_json::from_str::<Ask>(&json.replace("\"all\"", "\"regex\":true,\"all\""))
                .is_err()
        );
    }

    /// What the supervisor says of a write and of an edit goes on the
    /// wire as it is written here, each echoing its nonce, and a field or
    /// a reason that isn't named never decodes.
    #[test]
    fn what_is_said_of_a_write_and_an_edit_has_its_wire_form() {
        let said = |say: &Say, written: &str| {
            let json = serde_json::to_string(say).unwrap();
            assert_eq!(json, written);
            assert_eq!(serde_json::from_str::<Say>(&json).unwrap(), *say);
            assert_eq!(say.nonce(), Some("n3"));
        };
        let nonce = || String::from("n3");
        said(
            &Say::Wrote {
                nonce: nonce(),
                wrote: Wrote {
                    size: 3,
                    replaced: true,
                },
            },
            r#"{"say":"wrote","nonce":"n3","wrote":{"size":3,"replaced":true}}"#,
        );
        said(
            &Say::Edited {
                nonce: nonce(),
                edited: Edited {
                    replaced: 2,
                    line: 7,
                    size: 40,
                },
            },
            r#"{"say":"edited","nonce":"n3","edited":{"replaced":2,"line":7,"size":40}}"#,
        );
        for (why, word) in [
            (NoWrite::Exists, "exists"),
            (NoWrite::NotAFile, "not-a-file"),
            (NoWrite::Denied, "denied"),
            (NoWrite::Failed, "failed"),
        ] {
            said(
                &Say::NoWrite {
                    nonce: nonce(),
                    why,
                },
                &format!(r#"{{"say":"no-write","nonce":"n3","why":"{word}"}}"#),
            );
        }
        for (why, word) in [
            (NoEdit::NoSuchFile, "no-such-file"),
            (NoEdit::Denied, "denied"),
            (NoEdit::NotAFile, "not-a-file"),
            (NoEdit::NotText, "not-text"),
            (NoEdit::TooBig, "too-big"),
            (NoEdit::NotFound, "not-found"),
            (NoEdit::Failed, "failed"),
        ] {
            said(
                &Say::NoEdit {
                    nonce: nonce(),
                    why,
                },
                &format!(r#"{{"say":"no-edit","nonce":"n3","why":"{word}"}}"#),
            );
        }
        said(
            &Say::NoEdit {
                nonce: nonce(),
                why: NoEdit::Ambiguous {
                    places: 3,
                    lines: vec![1, 3, 3],
                },
            },
            r#"{"say":"no-edit","nonce":"n3","why":{"ambiguous":{"places":3,"lines":[1,3,3]}}}"#,
        );
        for unknown in [
            r#"{"say":"wrote","nonce":"n3","wrote":{"size":3,"replaced":true,"mode":420}}"#,
            r#"{"say":"edited","nonce":"n3","edited":{"replaced":2,"line":7,"size":40,"old":"x"}}"#,
            r#"{"say":"no-write","nonce":"n3","why":"gone"}"#,
            r#"{"say":"no-edit","nonce":"n3","why":{"ambiguous":{"places":3}}}"#,
        ] {
            assert!(serde_json::from_str::<Say>(unknown).is_err(), "{unknown}");
        }
    }

    /// What a write and an edit are said to have come to is held to what
    /// either can come to: a file no longer than may be, an edit at a
    /// place there is, and text at more places than one named by lines
    /// that can be lines.
    #[test]
    fn what_was_written_and_edited_is_held_to_its_bounds() {
        let most = u64::try_from(WRITE_BYTES).unwrap();
        let wrote = |size: u64| Wrote {
            size,
            replaced: false,
        };
        assert!(Wrote::default().check().is_ok());
        assert!(wrote(most).check().is_ok());
        assert!(wrote(most + 1).check().unwrap_err().contains("more than"));
        assert!(wrote(u64::MAX).check().is_err());
        let edited = |replaced: u32, line: u64, size: u64| Edited {
            replaced,
            line,
            size,
        };
        assert!(edited(1, 1, 0).check().is_ok());
        assert!(edited(u32::MAX, u64::MAX, EDIT_FILE_BYTES).check().is_ok());
        assert!(edited(0, 1, 9).check().unwrap_err().contains("no place"));
        assert!(edited(1, 0, 9).check().unwrap_err().contains("no place"));
        assert!(
            edited(1, 1, EDIT_FILE_BYTES + 1)
                .check()
                .unwrap_err()
                .contains("longer")
        );
        for plain in [
            NoEdit::NoSuchFile,
            NoEdit::Denied,
            NoEdit::NotAFile,
            NoEdit::NotText,
            NoEdit::TooBig,
            NoEdit::NotFound,
            NoEdit::Failed,
        ] {
            assert!(plain.check().is_ok(), "{plain:?}");
        }
        let at = |places: u32, lines: &[u64]| NoEdit::Ambiguous {
            places,
            lines: lines.to_vec(),
        };
        assert!(at(2, &[1, 1]).check().is_ok());
        assert!(at(2, &[4]).check().is_ok(), "fewer named than there are");
        assert!(at(u32::MAX, &[1, 2, 3, 4, 5, 6, 7, 8]).check().is_ok());
        for (bad, why) in [
            (at(1, &[1]), "places"),
            (at(0, &[]), "places"),
            (at(2, &[]), "places"),
            (at(2, &[1, 2, 3]), "places"),
            (at(9, &[1, 2, 3, 4, 5, 6, 7, 8, 9]), "places"),
            (at(2, &[0, 1]), "lines"),
            (at(3, &[5, 4]), "lines"),
            (at(3, &[1, 3, 2]), "lines"),
        ] {
            assert!(bad.check().unwrap_err().contains(why), "{bad:?}");
        }
        let longest = Say::NoEdit {
            nonce: "f".repeat(64),
            why: at(u32::MAX, &[u64::MAX; 8]),
        };
        assert!(crate::fits_one_frame(&longest));
    }

    /// A take is of a file named whole under the workspace, from a byte
    /// short of the bound, and the most it may carry, asked or answered,
    /// fits one frame.
    #[test]
    fn a_take_is_of_a_file_in_the_workspace_and_bounded() {
        assert_eq!((TAKE_BYTES, TAKE_FILE_BYTES), (32_768, 8_388_608));
        assert_eq!(TAKE_FILE_BYTES, SPILL_BYTES);
        let deepest = format!("/workspace/{}", "\u{1}".repeat(PATH_BYTES - 11));
        assert_eq!(deepest.len(), PATH_BYTES);
        for good in ["/workspace/a", "/workspace/out/report.pdf", &deepest] {
            assert!(check_take(good, 0).is_ok(), "{good:?}");
        }
        assert!(check_take("/workspace/a", TAKE_FILE_BYTES - 1).is_ok());
        for at in [TAKE_FILE_BYTES, u64::MAX] {
            let why = check_take("/workspace/a", at).unwrap_err();
            assert!(why.contains("at most 8 MiB"), "{at}: {why}");
        }
        let too_deep = format!("{deepest}d");
        for (path, why) in [
            ("workspace/a", "absolute"),
            ("", "absolute"),
            ("/workspace/a\0", "invalid or too long"),
            (too_deep.as_str(), "invalid or too long"),
            ("/workspace/", "a directory"),
            ("/workspace/out/", "a directory"),
            ("/workspace", "from the workspace"),
            ("/workspaces/a", "from the workspace"),
            ("/session/jobs/a.stdout", "from the workspace"),
            ("/etc/passwd", "from the workspace"),
        ] {
            let said = check_take(path, 0).unwrap_err();
            assert!(said.contains(why), "{path:?}: {said}");
        }
        let most = Ask::Take {
            nonce: "f".repeat(64),
            path: deepest,
            at: u64::MAX,
        };
        assert!(crate::fits_one_frame(&most));
        let most = Say::Took {
            nonce: "f".repeat(64),
            took: Took {
                bytes: vec![0xff; TAKE_BYTES],
                size: u64::MAX,
                ino: u64::MAX,
                changed: u64::MAX,
            },
        };
        assert!(crate::fits_one_frame(&most));
    }

    /// A take, and what is said of it, go on the wire as they are written
    /// here, each with its nonce, and a field or a reason that isn't named
    /// never decodes.
    #[test]
    fn a_take_and_its_answers_have_their_wire_form() {
        let take = Ask::Take {
            nonce: String::from("n4"),
            path: String::from("/workspace/a.txt"),
            at: 32_768,
        };
        let json = serde_json::to_string(&take).unwrap();
        assert_eq!(
            json,
            r#"{"ask":"take","nonce":"n4","path":"/workspace/a.txt","at":32768}"#
        );
        assert_eq!(serde_json::from_str::<Ask>(&json).unwrap(), take);
        assert_eq!(take.nonce(), "n4");
        assert!(
            serde_json::from_str::<Ask>(&json.replace("\"at\"", "\"follow\":true,\"at\"")).is_err()
        );
        let said = |say: &Say, written: &str| {
            let json = serde_json::to_string(say).unwrap();
            assert_eq!(json, written);
            assert_eq!(serde_json::from_str::<Say>(&json).unwrap(), *say);
            assert_eq!(say.nonce(), Some("n4"));
        };
        said(
            &Say::Took {
                nonce: String::from("n4"),
                took: Took {
                    bytes: b"hi\n".to_vec(),
                    size: 3,
                    ino: 12,
                    changed: 1_700_000_000_000_000_001,
                },
            },
            r#"{"say":"took","nonce":"n4","took":{"bytes":"aGkK","size":3,"ino":12,"changed":1700000000000000001}}"#,
        );
        for (why, word) in [
            (NoTake::NoSuchFile, "no-such-file"),
            (NoTake::Denied, "denied"),
            (NoTake::NotAFile, "not-a-file"),
            (NoTake::Link, "link"),
            (NoTake::Outside, "outside"),
            (NoTake::Changed, "changed"),
            (NoTake::Failed, "failed"),
        ] {
            said(
                &Say::NoTake {
                    nonce: String::from("n4"),
                    why,
                },
                &format!(r#"{{"say":"no-take","nonce":"n4","why":"{word}"}}"#),
            );
        }
        for unknown in [
            r#"{"say":"took","nonce":"n4","took":{"bytes":"","size":0,"ino":1,"changed":0,"mode":420}}"#,
            r#"{"say":"took","nonce":"n4","took":{"bytes":"","size":0,"ino":1}}"#,
            r#"{"say":"no-take","nonce":"n4","why":"gone"}"#,
        ] {
            assert!(serde_json::from_str::<Say>(unknown).is_err(), "{unknown}");
        }
    }

    /// A piece of a file is held to what a piece from where it was asked
    /// can be: no longer than one, not past the file's end, and short of
    /// one only at the file's end.
    #[test]
    fn a_piece_taken_is_held_to_where_it_was_asked() {
        let piece = |len: usize, size: u64| Took {
            bytes: vec![7; len],
            size,
            ino: 1,
            changed: 0,
        };
        let full = TAKE_BYTES;
        let whole = u64::try_from(full).unwrap();
        for (took, at) in [
            (piece(0, 0), 0),
            (piece(3, 3), 0),
            (piece(full, whole), 0),
            (piece(full, whole + 1), 0),
            (piece(1, whole + 1), whole),
            (piece(full, 3 * whole), whole),
            (piece(0, whole), whole),
        ] {
            assert!(
                took.check(at).is_ok(),
                "{} at {at} of {}",
                took.bytes.len(),
                took.size
            );
        }
        for (took, at, why) in [
            (piece(full + 1, u64::MAX), 0, "too long"),
            (piece(4, 3), 0, "past the file's end"),
            (piece(1, whole), whole, "past the file's end"),
            (piece(0, 3), 4, "past the file's end"),
            (piece(1, 1), u64::MAX, "past the file's end"),
            (piece(2, 3), 0, "short where the file goes on"),
            (
                piece(full - 1, whole + 1),
                0,
                "short where the file goes on",
            ),
            (piece(0, 1), 0, "short where the file goes on"),
        ] {
            let said = took.check(at).unwrap_err();
            assert!(
                said.contains(why),
                "{} at {at} of {}: {said}",
                took.bytes.len(),
                took.size
            );
        }
    }

    fn entry(name: &[u8], kind: Kind) -> Entry {
        let (ino, size) = match kind {
            Kind::File => (7, 3),
            Kind::Folder => (8, 0),
            Kind::Link | Kind::Mount | Kind::Other => (0, 0),
        };
        Entry {
            name: name.to_vec(),
            kind,
            ino,
            size,
        }
    }

    /// A name is what a folder can hold: 1 to 255 bytes, any but `/` and
    /// NUL, and neither `.` nor `..`.
    #[test]
    fn a_name_is_what_a_folder_can_hold() {
        assert_eq!((LIST_PAGE, LIST_FOLDER_NAMES, NAME_BYTES), (64, 1024, 255));
        let longest = vec![b'a'; NAME_BYTES];
        for good in [
            &b"a"[..],
            b"...",
            b".a",
            b"a b",
            b"\xff\xfe",
            b"line\nbreak",
            b"\\",
            &longest,
        ] {
            assert!(is_name(good), "{good:?}");
        }
        let longer = vec![b'a'; NAME_BYTES + 1];
        for bad in [&b""[..], b".", b"..", b"a/b", b"/", b"a\0", &longer] {
            assert!(!is_name(bad), "{bad:?}");
        }
    }

    /// A folder is listed from beneath the workspace, by a path with no
    /// empty, `.` or `..` step, after a name or from its start.
    #[test]
    fn a_listing_is_of_a_folder_in_the_workspace() {
        let deepest = format!("/workspace/{}", "\u{1}".repeat(PATH_BYTES - 11));
        assert_eq!(deepest.len(), PATH_BYTES);
        for good in [
            WORKSPACE,
            "/workspace/a",
            "/workspace/out/site",
            "/workspace/.git",
            &deepest,
        ] {
            assert!(check_list(good, b"").is_ok(), "{good:?}");
            assert!(check_list(good, b"z").is_ok(), "{good:?}");
        }
        let too_deep = format!("{deepest}d");
        for (path, why) in [
            ("workspace/a", "absolute"),
            ("", "absolute"),
            ("/workspace/a\0", "invalid or too long"),
            (too_deep.as_str(), "invalid or too long"),
            ("/workspaces/a", "from the workspace"),
            ("/workspaces", "from the workspace"),
            ("/etc", "from the workspace"),
            ("/workspace/", "empty, . or .."),
            ("/workspace/out/", "empty, . or .."),
            ("/workspace//out", "empty, . or .."),
            ("/workspace/.", "empty, . or .."),
            ("/workspace/./out", "empty, . or .."),
            ("/workspace/..", "empty, . or .."),
            ("/workspace/out/../..", "empty, . or .."),
        ] {
            let said = check_list(path, b"").unwrap_err();
            assert!(said.contains(why), "{path:?}: {said}");
        }
        for after in [&b"."[..], b"..", b"a/b", b"a\0"] {
            for path in [WORKSPACE, "/workspace/a"] {
                let said = check_list(path, after).unwrap_err();
                assert!(said.contains("can't be one"), "{path} {after:?}: {said}");
            }
        }
    }

    /// The most a listing may carry, asked or answered, fits one frame: a
    /// full page of the longest names, each with the most it can say.
    #[test]
    fn the_most_a_listing_carries_fits_one_frame() {
        let deepest = format!("/workspace/{}", "\u{1}".repeat(PATH_BYTES - 11));
        let most = Ask::List {
            nonce: "f".repeat(64),
            path: deepest,
            after: vec![0xff; NAME_BYTES],
        };
        assert!(crate::fits_one_frame(&most));
        let most = Say::Listed {
            nonce: "f".repeat(64),
            listed: Listed {
                ino: u64::MAX,
                changed: u64::MAX,
                total: u32::MAX,
                entries: (0..LIST_PAGE)
                    .map(|_| Entry {
                        name: vec![0xff; NAME_BYTES],
                        kind: Kind::Folder,
                        ino: u64::MAX,
                        size: u64::MAX,
                    })
                    .collect(),
            },
        };
        assert!(crate::fits_one_frame(&most));
    }

    /// A listing, and what is said of it, go on the wire as they are
    /// written here, each with its nonce, and a field, a kind or a reason
    /// that isn't named never decodes.
    #[test]
    fn a_listing_and_its_answers_have_their_wire_form() {
        let list = Ask::List {
            nonce: String::from("n5"),
            path: String::from("/workspace/site"),
            after: b"a\xff".to_vec(),
        };
        let json = serde_json::to_string(&list).unwrap();
        assert_eq!(
            json,
            r#"{"ask":"list","nonce":"n5","path":"/workspace/site","after":"Yf8="}"#
        );
        assert_eq!(serde_json::from_str::<Ask>(&json).unwrap(), list);
        assert_eq!(list.nonce(), "n5");
        assert!(
            serde_json::from_str::<Ask>(&json.replace("\"after\"", "\"deep\":true,\"after\""))
                .is_err()
        );
        let said = |say: &Say, written: &str| {
            let json = serde_json::to_string(say).unwrap();
            assert_eq!(json, written);
            assert_eq!(serde_json::from_str::<Say>(&json).unwrap(), *say);
            assert_eq!(say.nonce(), Some("n5"));
        };
        said(
            &Say::Listed {
                nonce: String::from("n5"),
                listed: Listed {
                    ino: 4,
                    changed: 1_700_000_000_000_000_001,
                    total: 5,
                    entries: vec![
                        entry(b"a", Kind::File),
                        entry(b"b", Kind::Folder),
                        entry(b"c", Kind::Link),
                        entry(b"d", Kind::Mount),
                        entry(b"e", Kind::Other),
                    ],
                },
            },
            concat!(
                r#"{"say":"listed","nonce":"n5","listed":{"ino":4,"changed":1700000000000000001,"total":5,"entries":["#,
                r#"{"name":"YQ==","kind":"file","ino":7,"size":3},"#,
                r#"{"name":"Yg==","kind":"folder","ino":8,"size":0},"#,
                r#"{"name":"Yw==","kind":"link","ino":0,"size":0},"#,
                r#"{"name":"ZA==","kind":"mount","ino":0,"size":0},"#,
                r#"{"name":"ZQ==","kind":"other","ino":0,"size":0}]}}"#,
            ),
        );
        for (why, word) in [
            (NoList::NoSuchFolder, "no-such-folder"),
            (NoList::Denied, "denied"),
            (NoList::NotAFolder, "not-a-folder"),
            (NoList::Link, "link"),
            (NoList::Outside, "outside"),
            (NoList::TooMany, "too-many"),
            (NoList::Changed, "changed"),
            (NoList::Failed, "failed"),
        ] {
            said(
                &Say::NoList {
                    nonce: String::from("n5"),
                    why,
                },
                &format!(r#"{{"say":"no-list","nonce":"n5","why":"{word}"}}"#),
            );
        }
        for unknown in [
            r#"{"say":"listed","nonce":"n5","listed":{"ino":1,"changed":0,"total":0,"entries":[],"more":true}}"#,
            r#"{"say":"listed","nonce":"n5","listed":{"ino":1,"changed":0,"entries":[]}}"#,
            r#"{"say":"listed","nonce":"n5","listed":{"ino":1,"changed":0,"total":1,"entries":[{"name":"YQ==","kind":"file","ino":1,"size":1,"mode":420}]}}"#,
            r#"{"say":"listed","nonce":"n5","listed":{"ino":1,"changed":0,"total":1,"entries":[{"name":"YQ==","kind":"socket","ino":0,"size":0}]}}"#,
            r#"{"say":"listed","nonce":"n5","listed":{"ino":1,"changed":0,"total":1,"entries":[{"name":"YQ==","kind":"file","ino":1,"size":-1}]}}"#,
            r#"{"say":"no-list","nonce":"n5","why":"gone"}"#,
        ] {
            assert!(serde_json::from_str::<Say>(unknown).is_err(), "{unknown}");
        }
    }

    /// A page of a folder is held to what a page after the name asked
    /// from can be.
    #[test]
    fn a_page_of_a_folder_is_held_to_where_it_was_asked() {
        let page = |total: u32, entries: Vec<Entry>| Listed {
            ino: 1,
            changed: 2,
            total,
            entries,
        };
        let names = |count: usize| -> Vec<Entry> {
            (0..count)
                .map(|at| entry(format!("{at:04}").as_bytes(), Kind::File))
                .collect()
        };
        // Whole pages, the last short, an empty folder, and the most.
        assert_eq!(page(0, Vec::new()).check(b""), Ok(()));
        assert_eq!(page(LIST_FOLDER_NAMES, names(LIST_PAGE)).check(b""), Ok(()));
        assert_eq!(page(3, names(3)).check(b""), Ok(()));
        let rest = names(3).get(1..).unwrap().to_vec();
        assert_eq!(page(3, rest).check(b"0000"), Ok(()));
        // Every kind, as it may be said.
        let kinds = vec![
            entry(b"A", Kind::Mount),
            entry(b"B", Kind::Other),
            entry(b"a", Kind::Folder),
            entry(b"b", Kind::Link),
            entry(b"c", Kind::File),
        ];
        assert_eq!(page(5, kinds).check(b""), Ok(()));
        let refused = |listed: Listed, after: &[u8], why: &str| {
            let said = listed.check(after).unwrap_err();
            assert!(said.contains(why), "{said}");
        };
        refused(page(LIST_FOLDER_NAMES + 1, Vec::new()), b"", "more names");
        refused(page(u32::MAX, Vec::new()), b"", "more names");
        refused(
            page(LIST_FOLDER_NAMES, names(LIST_PAGE + 1)),
            b"",
            "too many",
        );
        refused(page(2, names(3)), b"", "too many");
        refused(
            page(1, vec![entry(b"a/b", Kind::File)]),
            b"",
            "can't be a name",
        );
        refused(
            page(1, vec![entry(b"..", Kind::Folder)]),
            b"",
            "can't be a name",
        );
        refused(
            page(1, vec![entry(b"", Kind::File)]),
            b"",
            "can't be a name",
        );
        // Out of order, twice the same, or not after the name asked from.
        let mut backwards = names(2);
        backwards.reverse();
        refused(page(2, backwards), b"", "in order");
        refused(
            page(2, vec![entry(b"a", Kind::File), entry(b"a", Kind::File)]),
            b"",
            "in order",
        );
        refused(page(2, names(2)), b"0000", "in order");
        refused(page(2, names(2)), b"0001", "in order");
        refused(page(2, names(2)), b"9", "in order");
        // An inode or a length only where one is said.
        for kind in [Kind::Link, Kind::Mount, Kind::Other] {
            let mut with = entry(b"a", kind);
            with.ino = 9;
            refused(page(1, vec![with]), b"", "an inode");
        }
        for kind in [Kind::Folder, Kind::Link, Kind::Mount, Kind::Other] {
            let mut with = entry(b"a", kind);
            with.size = 1;
            refused(page(1, vec![with]), b"", "a length");
        }
    }
}
