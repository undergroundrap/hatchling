//! The Room kept between commands, and the jobs run in it.
//!
//! The install has one Room session at a time. One thread of the spawner
//! holds it from its launch until it is down: the launch, every job, every
//! reason it ends and the cleanup after are that thread's alone.
//! Connections hand it messages and wait for its answers, so nothing about
//! a session is decided in two places.
//!
//! A command in the Room is a job, named by whoever asks for it. Only so
//! many run at once. A job has a limit: still running then, it is stopped,
//! which kills every process in its group. Its caller waits for it only so
//! long, and is then told it is running; whoever asks by its name later is
//! told how it is, or how it came out. What is known of a job that is over
//! is kept in a ledger that outlives the session, so a name the spawner
//! knows is never run twice.
//!
//! The supervisor in the Room is asked one thing at a time and must answer
//! it; the one thing it says unasked is that a job has ended. Anything
//! else it says, or doesn't say in time, ends the session.
//!
//! A page of a file in the Room is asked of the supervisor the same way,
//! in its turn. It is no job: nothing is kept of it, and asking again
//! reads the file again. Like a job, it starts a session when there is
//! none. So is a search of the Room's files: asked in its turn, kept
//! nowhere, and looked for again when asked again. And so is a piece of a
//! file to be handed over: each piece is asked in its turn, and the
//! spawner's connection puts them together.
//!
//! A session ends at its time limit, when it has been idle, when a job in
//! it doesn't end after it was stopped, when its supervisor says what it
//! may not or goes quiet, when the disk under it is closed or wiped, and
//! when the broker goes. It always ends the same way: its first process is
//! stopped and confirmed gone, its cgroup is drained and its image mapping
//! closed. Until that has worked the thread keeps the Room's disk claimed
//! and refuses new work. A job still running when its session ends is
//! lost with it, and is said to be.

use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hatchling_core::room::{
    Ask, Captured, Edited, End, Ended, Found, JobId, NoEdit, NoPage, NoSearch, NoTake, NoWrite,
    Page, RUNNING_MAX, Say, Stream, Took, Wrote, check_edit, check_page, check_search, check_take,
    check_write,
};

use crate::room_disk::{InboxAnswer, InboxOp, RoomDiskOp, RoomDiskStatus};
use crate::room_job::{
    Checked, RoomJob, RoomJobAnswer, RoomJobState, RoomJobStopped, RoomRun, not_started,
};
use crate::room_launch::Session;

const FULL: &str = "the Room is running as many jobs as it may";
pub(crate) const NO_JOB: &str = "there is no Room job by that name";
const NOT_OVER: &str =
    "the job is still running: what it has written so far is in its file in the Room";
const LOST: &str = "the job was lost with its Room, and what it wrote went with it";
const EXPIRED: &str = "the job's limit passed before the Room could start it";
const UNREADY: &str = "the Room supervisor did not become ready in time";
const CAPPED: &str = "the Room session reached its time limit";
const IDLE: &str = "the Room session was idle";
const REQUESTER_GONE: &str = "the broker ended the Room request";
const LAUNCHER_ENDED: &str = "the Room's first process ended";
const OUT_OF_TURN: &str = "the Room supervisor spoke out of turn";
const CLOSED: &str = "the Room supervisor closed";
const CLOSED_EARLY: &str = "the Room supervisor closed before readiness";
const BAD_READY: &str = "the Room supervisor sent invalid readiness";
const BAD_SAY: &str = "the Room supervisor said something that can't be read";
const BAD_END: &str = "the Room supervisor said a job ended in a way it can't have";
const BAD_PIECE: &str = "the Room supervisor sent a piece of output of the wrong length";
const BAD_PAGE: &str = "the Room supervisor sent a page of a file that can't be one";
const BAD_FOUND: &str = "the Room supervisor sent what a search found that can't be it";
const BAD_WROTE: &str = "the Room supervisor said it wrote a file in a way it can't have";
const BAD_EDITED: &str = "the Room supervisor said it made an edit in a way it can't have";
const BAD_TOOK: &str = "the Room supervisor sent a piece of a file that can't be the one asked for";
/// Said after why a session ended, to whoever asked for a change the
/// supervisor had been given and hadn't answered.
const UNHEARD: &str =
    ". The file may or may not have been changed: read it before changing it again";
const UNANSWERED: &str = "the Room supervisor didn't answer in time";
const UNSTOPPED: &str = "a Room job didn't end when it was stopped";
const UNSENT: &str = "could not send to the Room supervisor";
const DISK_CLOSING: &str = "the Room was stopped: its disk is being closed";
const UNOWNED: &str = "the Room session lost its owner";
const UNSETTLED: &str = "the Room session kept ending; try again";
const LEAVING: &str = "the spawner is leaving";
const READY: &[u8] = b"ready";

/// How many times a request is handed to a session that ends before
/// answering it, or starts one, before it is refused.
const ATTEMPTS: usize = 4;

/// How many jobs that are over the spawner remembers. The oldest goes
/// when another is over, and its name is free again.
const LEDGER_MAX: usize = 64;

/// What asking about a job comes to.
pub(crate) type JobReply = Result<RoomJobAnswer, String>;
/// What a room disk operation comes to.
pub(crate) type DiskReply = Result<Option<RoomDiskStatus>, String>;
/// What asking the room disk's inbox comes to.
pub(crate) type InboxReply = Result<InboxAnswer, String>;
/// What asking for a page of a file comes to.
pub(crate) type PageReply = Result<Page, String>;

/// What a search of the Room's files comes to.
pub(crate) type SearchReply = Result<Found, String>;

/// What asking for a piece of a file to hand over comes to.
pub(crate) type TakeReply = Result<Took, String>;

/// What one search of the Room's files asks for, as its caller gave it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    /// What to look for.
    pub pattern: String,
    /// The absolute path of the file or directory to look in.
    pub path: String,
    /// Only files whose names match this are looked in.
    pub glob: Option<String>,
    /// Whether letters match whatever their case.
    pub ignore_case: bool,
    /// Whether the pattern is the very words to find.
    pub literal: bool,
}

/// What changing a file in the Room comes to.
pub(crate) type ChangeReply = Result<Changed, String>;

/// A change to one file in the Room, as its caller gave it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "change", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Change {
    /// Write the file whole: a new file, or one in place of the file
    /// there when `replace` is set.
    Write {
        /// The file's absolute path in the Room.
        path: String,
        /// What the file is to hold.
        #[serde(with = "hatchling_core::room::b64")]
        bytes: Vec<u8>,
        /// Whether a file already there is to be replaced.
        replace: bool,
    },
    /// Replace `old` with `new` in the file: at the one place it is, or
    /// at every place when `all` is set.
    Edit {
        /// The file's absolute path in the Room.
        path: String,
        /// The text to replace, exactly as it is in the file.
        #[serde(with = "hatchling_core::room::b64")]
        old: Vec<u8>,
        /// What to put in its place.
        #[serde(with = "hatchling_core::room::b64")]
        new: Vec<u8>,
        /// Whether every place the text is at is to be replaced.
        all: bool,
    },
}

impl Change {
    /// Whether this may be asked for.
    ///
    /// # Errors
    ///
    /// Why it may not.
    pub fn check(&self) -> Result<(), &'static str> {
        match self {
            Self::Write { path, bytes, .. } => check_write(path, bytes.len()),
            Self::Edit { path, old, new, .. } => check_edit(path, old, new),
        }
    }

    /// What the supervisor's answer to this must agree with.
    fn expect(&self) -> Expect {
        match self {
            Self::Write { bytes, replace, .. } => Expect::Write {
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                replace: *replace,
            },
            Self::Edit { all, .. } => Expect::Edit { all: *all },
        }
    }
}

/// What a change to a file in the Room came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Changed {
    /// The file was written.
    Wrote(Wrote),
    /// The edit was made.
    Edited(Edited),
}

/// What the answer to a change the supervisor was asked for must agree
/// with: the change itself has gone to the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// A file of this length, replaced only if it was to be.
    Write { size: u64, replace: bool },
    /// One place, unless every place was asked for.
    Edit { all: bool },
}

impl Expect {
    /// Whether `wrote` can be what this write came to.
    fn wrote(self, wrote: Wrote) -> bool {
        wrote.check().is_ok()
            && matches!(self, Self::Write { size, replace } if wrote.size == size && (replace || !wrote.replaced))
    }

    /// Whether `edited` can be what this edit came to.
    const fn edited(self, edited: Edited) -> bool {
        edited.check().is_ok() && matches!(self, Self::Edit { all } if all || edited.replaced == 1)
    }
}

/// Why the supervisor wrote no file, in words for whoever asked.
fn no_write(why: NoWrite) -> String {
    String::from(match why {
        NoWrite::Exists => "the Room has a file at that path already, and it wasn't replaced",
        NoWrite::NotAFile => {
            "what the Room has at that path isn't a file: it is a directory or a link, or is under something that is no directory"
        }
        NoWrite::Denied => "the Room's user may not write there",
        NoWrite::Failed => "the Room couldn't write the file",
    })
}

/// Why the supervisor made no edit, in words for whoever asked.
fn no_edit(why: NoEdit) -> String {
    match why {
        NoEdit::NoSuchFile => String::from("the Room has no such file"),
        NoEdit::Denied => {
            String::from("the Room's user may not read that file, or may not write there")
        }
        NoEdit::NotAFile => String::from("that is a directory or a link, not a file"),
        NoEdit::NotText => String::from("that file isn't text, or what was to be put in it isn't"),
        NoEdit::TooBig => {
            String::from("that file is longer than 4 MiB, or the edit would make it so")
        }
        NoEdit::NotFound => String::from(
            "the text to replace isn't in the file as it was given: nothing was changed",
        ),
        NoEdit::Ambiguous { places, lines } => {
            let named: Vec<String> = lines.iter().map(u64::to_string).collect();
            format!(
                "the text to replace is in the file at {places} places, and one was wanted: nothing was changed. The first {} start on lines {}",
                named.len(),
                named.join(", ")
            )
        }
        NoEdit::Failed => String::from("the Room couldn't make the edit"),
    }
}

/// Why the supervisor handed back nothing of a search, in words for
/// whoever asked.
fn no_search(why: NoSearch) -> String {
    String::from(match why {
        NoSearch::NoSuchPath => "the Room has nothing at that path",
        NoSearch::Refused => {
            "the search wasn't run: the pattern or the glob isn't one, or that path can't be read"
        }
        NoSearch::Failed => "the Room couldn't run the search",
    })
}

/// Why the supervisor handed back no piece of a file, in words for
/// whoever asked.
fn no_take(why: NoTake) -> String {
    String::from(match why {
        NoTake::NoSuchFile => "the Room has no such file",
        NoTake::Denied => "the Room's user may not read that file",
        NoTake::NotAFile => "that is a directory or a special file, not a file",
        NoTake::Link => {
            "that path has a link in it, and no link is followed: hand over the file itself"
        }
        NoTake::Outside => {
            "that path leads out of the workspace, or onto another file system: only a file in the workspace is handed over"
        }
        NoTake::Changed => "the file changed while it was read: hand it over once it is done",
        NoTake::Failed => "the Room couldn't read that file",
    })
}

/// Why the supervisor handed back no page, in words for whoever asked.
fn no_page(why: NoPage) -> String {
    match why {
        NoPage::NoSuchFile => String::from("the Room has no such file"),
        NoPage::Denied => String::from("the Room's user may not read that file"),
        NoPage::NotAFile => {
            String::from("that is a directory or a special file, not a file with lines to read")
        }
        NoPage::NotText => String::from("those lines of the file are not text"),
        NoPage::TooFar => {
            String::from("those lines are too far into the file: past its first 32 MiB")
        }
        NoPage::PastEnd(0) => String::from("the file is empty"),
        NoPage::PastEnd(last) => format!("the file's last line is line {last}"),
        NoPage::Failed => String::from("the Room couldn't read that file"),
    }
}

/// How long a session, and each wait in it, may last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The longest a session lives, busy or not.
    pub(crate) cap: Duration,
    /// How long it may sit with no job before it ends.
    pub(crate) idle: Duration,
    /// How long the supervisor may take to say it is ready.
    pub(crate) ready: Duration,
    /// How long the supervisor may take to answer what it's asked.
    pub(crate) answer: Duration,
    /// How long a job may take to end once it has been stopped.
    pub(crate) stop: Duration,
    /// How long between tries to clear a session that won't come down.
    pub(crate) retry: Duration,
}

impl Limits {
    /// What an installed spawner holds every session to.
    pub(crate) const INSTALLED: Self = Self {
        cap: Duration::from_secs(60 * 60),
        idle: Duration::from_secs(15 * 60),
        ready: Duration::from_secs(30),
        answer: Duration::from_secs(10),
        stop: Duration::from_secs(5),
        retry: Duration::from_secs(5),
    };
}

/// A stream's kept head and tail.
type Pieces = [Vec<u8>; 2];

/// What is known of a job that is over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Over {
    /// How it came out: ended, or lost.
    state: RoomJobState,
    /// What was kept of standard output, then of standard error.
    pieces: [Pieces; 2],
}

impl Over {
    /// A job that went with its Room.
    fn lost(why: &str) -> Self {
        Self {
            state: RoomJobState::Lost {
                why: why.to_owned(),
            },
            pieces: Default::default(),
        }
    }

    /// A kept piece of what it wrote.
    fn piece(&self, stream: Stream, end: End) -> Result<Vec<u8>, String> {
        if matches!(self.state, RoomJobState::Lost { .. }) {
            return Err(String::from(LOST));
        }
        let [stdout, stderr] = &self.pieces;
        let [head, tail] = match stream {
            Stream::Stdout => stdout,
            Stream::Stderr => stderr,
        };
        Ok(match end {
            End::Head => head.clone(),
            End::Tail => tail.clone(),
        })
    }
}

/// The jobs that are over, newest last. It outlives every session.
#[derive(Debug, Default)]
pub(crate) struct Ledger {
    over: VecDeque<(JobId, Over)>,
}

impl Ledger {
    fn get(&self, job: &JobId) -> Option<&Over> {
        self.over
            .iter()
            .find_map(|(id, over)| (id == job).then_some(over))
    }

    fn put(&mut self, job: JobId, over: Over) {
        self.over.push_back((job, over));
        // One was put, so at most one is over the count.
        if self.over.len() > LEDGER_MAX {
            self.over.pop_front();
        }
    }
}

/// The ledger, whatever became of a thread that held it.
fn opened(ledger: &Mutex<Ledger>) -> MutexGuard<'_, Ledger> {
    ledger.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A job as its caller is told of it.
fn told(job: &JobId, state: RoomJobState, started: bool) -> RoomJobAnswer {
    RoomJobAnswer {
        started,
        job: RoomJob {
            job: job.clone(),
            state,
        },
    }
}

/// A length of time in whole milliseconds.
fn millis(time: Duration) -> u64 {
    u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
}

/// What a connection asks of the session's thread.
pub(crate) enum Message {
    /// Run this job, for the connection at `requester`.
    Run {
        run: Box<RoomRun>,
        checked: Checked,
        requester: UnixStream,
        received: Instant,
        reply: Sender<JobReply>,
    },
    /// Say how this job is, waiting up to `wait` for it to end.
    Job {
        job: JobId,
        wait: Duration,
        requester: UnixStream,
        received: Instant,
        reply: Sender<JobReply>,
    },
    /// Stop this job, and say how it came out.
    Stop {
        job: JobId,
        requester: UnixStream,
        reply: Sender<JobReply>,
    },
    /// Hand back a page of a file, for the connection at `requester`.
    Page {
        path: String,
        from: u64,
        lines: u32,
        requester: UnixStream,
        reply: Sender<PageReply>,
    },
    /// Search the Room's files, for the connection at `requester`.
    Search {
        search: Search,
        requester: UnixStream,
        reply: Sender<SearchReply>,
    },
    /// Change a file in the Room, for the connection at `requester`.
    Change {
        change: Change,
        requester: UnixStream,
        reply: Sender<ChangeReply>,
    },
    /// Hand back a piece of a file in the workspace, from byte `at`, for
    /// the connection at `requester`.
    Take {
        path: String,
        at: u64,
        requester: UnixStream,
        reply: Sender<TakeReply>,
    },
    /// Say whether the installed Room works.
    Probe { reply: Sender<Result<(), String>> },
    /// Do this to the room disk.
    Disk {
        op: RoomDiskOp,
        reply: Sender<DiskReply>,
    },
    /// Do this to the room disk's inbox.
    Inbox {
        op: InboxOp,
        reply: Sender<InboxReply>,
    },
    /// End the session: the broker has gone.
    End { reason: &'static str },
}

/// The session thread's end of its channel.
pub(crate) struct Seat {
    inbox: Receiver<Message>,
    wake: UnixStream,
}

/// The connections' end of a session's channel.
struct Door {
    inbox: Sender<Message>,
    wake: UnixStream,
}

impl Door {
    /// A channel to a session thread, and that thread's end of it.
    fn pair() -> io::Result<(Self, Seat)> {
        let (inbox, theirs) = mpsc::channel();
        let (wake, woken) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        woken.set_nonblocking(true)?;
        Ok((
            Self { inbox, wake },
            Seat {
                inbox: theirs,
                wake: woken,
            },
        ))
    }

    /// Hands `message` to the session thread and wakes it. The message
    /// comes back if the thread has ended.
    fn send(&self, message: Message) -> Result<(), Message> {
        self.inbox.send(message).map_err(|refused| refused.0)?;
        // A full wake socket already has the thread on its way.
        let _ = (&self.wake).write(&[0]);
        Ok(())
    }
}

/// Empties the wake socket, and says whether its other end is still held.
fn rested(mut wake: &UnixStream) -> bool {
    let mut bytes = [0_u8; 64];
    loop {
        match wake.read(&mut bytes) {
            Ok(0) => return false,
            Ok(_) => {}
            Err(_) => return true,
        }
    }
}

/// Whether `stream` has neither input nor an end waiting. One that can't
/// be looked at isn't quiet.
fn quiet(stream: &UnixStream) -> bool {
    hatchling_sys::net::unix_stream_quiet(stream.as_fd()).unwrap_or(false)
}

/// Which of a session's own waits runs out first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Due {
    Unready,
    Cap,
    Idle,
}

impl Due {
    const fn reason(self) -> &'static str {
        match self {
            Self::Unready => UNREADY,
            Self::Cap => CAPPED,
            Self::Idle => IDLE,
        }
    }
}

/// When a session started, when it last had something in hand, and
/// whether its supervisor has said it is ready.
struct Clock {
    started: Instant,
    used: Instant,
    ready: bool,
}

impl Clock {
    /// The session's own next wait to run out, and when. A session with
    /// something in hand is never idle; its time limit holds whatever it
    /// is doing.
    fn due(&self, limits: &Limits, busy: bool) -> (Instant, Due) {
        let cap = (self.started + limits.cap, Due::Cap);
        let waiting = if !self.ready {
            (self.started + limits.ready, Due::Unready)
        } else if busy {
            cap
        } else {
            (self.used + limits.idle, Due::Idle)
        };
        if cap.0 < waiting.0 { cap } else { waiting }
    }
}

/// What the session asks the supervisor, in the order it will.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asking {
    Start(JobId),
    Stop(JobId),
    Piece(JobId, Stream, End),
    /// The page the first connection waiting for one wants.
    Page,
    /// The search the first connection waiting for one wants.
    Search,
    /// The change the first connection waiting for one wants.
    Change,
    /// The piece of a file the first connection waiting for one wants.
    Take,
}

/// A job's end as the supervisor said it, while its kept output is
/// fetched.
struct Landed {
    state: RoomJobState,
    pieces: [Pieces; 2],
    /// How many pieces are still to come.
    wanted: usize,
}

/// A job the session has in hand.
struct Flight {
    argv: Vec<String>,
    dir: String,
    stdin: Vec<u8>,
    /// When it was asked for.
    asked: Instant,
    /// When it is stopped if it is still running.
    limit: Instant,
    /// Its start has gone to the supervisor.
    sent: bool,
    /// The supervisor said it started.
    started: bool,
    /// When it was stopped, and why.
    stopping: Option<(Instant, RoomJobStopped)>,
    landed: Option<Landed>,
}

/// A connection waiting to be told of a job.
struct Waiter {
    job: JobId,
    requester: UnixStream,
    reply: Sender<JobReply>,
    /// When it is told the job is running, if it still is.
    until: Instant,
    /// Whether its ask is what started the job.
    started: bool,
}

/// A connection waiting for a page of a file.
struct Reader {
    path: String,
    from: u64,
    lines: u32,
    requester: UnixStream,
    reply: Sender<PageReply>,
}

/// A connection waiting for a search of the Room's files.
struct Seeker {
    search: Search,
    requester: UnixStream,
    reply: Sender<SearchReply>,
}

/// A connection waiting for a change to a file in the Room.
struct Changer {
    change: Change,
    requester: UnixStream,
    reply: Sender<ChangeReply>,
}

/// A connection waiting for a piece of a file to hand over.
struct Taker {
    path: String,
    at: u64,
    requester: UnixStream,
    reply: Sender<TakeReply>,
}

/// An answer kept until the Room is down.
enum Owed {
    /// How a job came out, or why it isn't known.
    Job(Sender<JobReply>, JobReply),
    /// Why there is no page.
    Page(Sender<PageReply>, String),
    /// Why nothing was searched.
    Search(Sender<SearchReply>, String),
    /// Why no change is known to have been made.
    Change(Sender<ChangeReply>, String),
    /// Why there is no piece of a file.
    Take(Sender<TakeReply>, String),
}

impl Owed {
    /// Gives the answer.
    fn pay(self) {
        match self {
            Self::Job(reply, answer) => {
                let _ = reply.send(answer);
            }
            Self::Page(reply, reason) => {
                let _ = reply.send(Err(reason));
            }
            Self::Search(reply, reason) => {
                let _ = reply.send(Err(reason));
            }
            Self::Change(reply, reason) => {
                let _ = reply.send(Err(reason));
            }
            Self::Take(reply, reason) => {
                let _ = reply.send(Err(reason));
            }
        }
    }

    /// Says instead that nothing can be answered, and why.
    fn refuse(self, reason: &str) {
        match self {
            Self::Job(reply, _) => {
                let _ = reply.send(Err(reason.to_owned()));
            }
            Self::Page(reply, _) => {
                let _ = reply.send(Err(reason.to_owned()));
            }
            Self::Search(reply, _) => {
                let _ = reply.send(Err(reason.to_owned()));
            }
            Self::Change(reply, _) => {
                let _ = reply.send(Err(reason.to_owned()));
            }
            Self::Take(reply, _) => {
                let _ = reply.send(Err(reason.to_owned()));
            }
        }
    }
}

/// Reads what the supervisor has sent so far into `heard`: never more
/// than a frame of `limit` bytes and its length. The socket doesn't block,
/// so no read here waits or is interrupted.
///
/// A closed socket is an error only when it had nothing more to give, so
/// what the supervisor sent before it closed is still looked at: the
/// close is found by the next call.
fn listen(
    mut supervisor: &UnixStream,
    heard: &mut Vec<u8>,
    limit: usize,
    closed: &'static str,
) -> Result<(), String> {
    let most = limit.saturating_add(4);
    let mut bytes = [0_u8; 8192];
    let before = heard.len();
    while heard.len() < most {
        let room = bytes.len().min(most.saturating_sub(heard.len()));
        let chunk = bytes.get_mut(..room).unwrap_or_default();
        match supervisor.read(chunk) {
            Ok(0) if heard.len() == before => return Err(String::from(closed)),
            Ok(0) => return Ok(()),
            Ok(count) => heard.extend_from_slice(chunk.get(..count).unwrap_or_default()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(format!("cannot read from the Room supervisor: {error}")),
        }
    }
    Ok(())
}

/// The first frame `heard` holds, once it holds a whole one. It is taken
/// out of `heard`.
fn frame(heard: &mut Vec<u8>, limit: usize) -> Result<Option<Vec<u8>>, String> {
    let Some((size, body)) = heard.split_first_chunk::<4>() else {
        return Ok(None);
    };
    let size = usize::try_from(u32::from_be_bytes(*size)).map_err(|error| error.to_string())?;
    if size > limit {
        return Err(String::from("oversized Room supervisor frame"));
    }
    let Some(body) = body.get(..size) else {
        return Ok(None);
    };
    let body = body.to_vec();
    heard.drain(..size.saturating_add(4));
    Ok(Some(body))
}

/// Why nothing can use the Room while its last session is still there.
fn held(reason: &str) -> String {
    format!("the last Room session isn't cleared yet: {reason}")
}

/// One Room session, as the thread that holds it keeps it.
///
/// `disk` does what may be done to the room disk while the session has it
/// claimed: anything but closing or wiping it, which end the session.
/// `inbox` puts, lists or removes a file of its inbox, under the same
/// claim.
pub(crate) struct Kept<D, P> {
    seat: Seat,
    limits: Limits,
    ledger: Arc<Mutex<Ledger>>,
    disk: D,
    inbox: P,
    /// The jobs in hand: running, or ended with output still to fetch.
    flights: BTreeMap<JobId, Flight>,
    waiters: Vec<Waiter>,
    /// The connections waiting for a page that hasn't been asked for.
    readers: VecDeque<Reader>,
    /// Who is told of the page the supervisor has been asked for.
    reading: Option<Sender<PageReply>>,
    /// The connections waiting for a search that hasn't been asked for.
    seekers: VecDeque<Seeker>,
    /// Who is told of the search the supervisor has been asked for.
    seeking: Option<Sender<SearchReply>>,
    /// The connections waiting for a change that hasn't been asked for.
    changers: VecDeque<Changer>,
    /// Who is told of the change the supervisor has been asked for, and
    /// what its answer must agree with.
    changing: Option<(Sender<ChangeReply>, Expect)>,
    /// The connections waiting for a piece of a file that hasn't been
    /// asked for.
    takers: VecDeque<Taker>,
    /// Who is told of the piece the supervisor has been asked for, and the
    /// byte it was asked from.
    taking: Option<(Sender<TakeReply>, u64)>,
    /// What is still to be asked of the supervisor.
    asks: VecDeque<Asking>,
    /// What it has been asked and hasn't answered: its nonce, and when.
    asked: Option<(String, Asking, Instant)>,
    /// Who asked whether the Room works before its supervisor was ready.
    probes: Vec<Sender<Result<(), String>>>,
    /// Who was waiting when the session ended: they hear once the Room is
    /// down, so nothing they then see is still being taken apart.
    owed: Vec<Owed>,
    /// The close or wipe that ended the session, done once it is down.
    after: Option<(RoomDiskOp, Sender<DiskReply>)>,
    /// The broker has gone: nothing waits on a cleanup that fails.
    left: bool,
}

impl<D: FnMut(RoomDiskOp) -> DiskReply, P: FnMut(&InboxOp) -> InboxReply> Kept<D, P> {
    pub(crate) const fn new(
        seat: Seat,
        limits: Limits,
        ledger: Arc<Mutex<Ledger>>,
        disk: D,
        inbox: P,
    ) -> Self {
        Self {
            seat,
            limits,
            ledger,
            disk,
            inbox,
            flights: BTreeMap::new(),
            waiters: Vec::new(),
            readers: VecDeque::new(),
            reading: None,
            seekers: VecDeque::new(),
            seeking: None,
            changers: VecDeque::new(),
            changing: None,
            takers: VecDeque::new(),
            taking: None,
            asks: VecDeque::new(),
            asked: None,
            probes: Vec::new(),
            owed: Vec::new(),
            after: None,
            left: false,
        }
    }

    /// Whether anything waits for a Room: a job, a page, a search, a
    /// change or a piece of a file.
    fn wanted(&self) -> bool {
        !self.flights.is_empty()
            || !self.readers.is_empty()
            || !self.seekers.is_empty()
            || !self.changers.is_empty()
            || !self.takers.is_empty()
    }

    /// How a job that is over came out.
    fn over(&self, job: &JobId) -> Option<RoomJobState> {
        opened(&self.ledger).get(job).map(|over| over.state.clone())
    }

    /// Whether the session has anything in hand: a job, a page or a
    /// search still to ask for, or an ask the supervisor hasn't answered.
    /// Whoever waits, waits for one of them.
    fn busy(&self) -> bool {
        !self.flights.is_empty()
            || !self.readers.is_empty()
            || !self.seekers.is_empty()
            || !self.changers.is_empty()
            || !self.takers.is_empty()
            || self.asked.is_some()
    }

    /// Takes what has been asked of the session so far. `Err` is why it
    /// ends.
    fn intake(&mut self) -> Result<(), String> {
        while let Ok(message) = self.seat.inbox.try_recv() {
            match message {
                Message::Run {
                    run,
                    checked,
                    requester,
                    received,
                    reply,
                } => self.run(*run, checked, requester, received, reply),
                Message::Job {
                    job,
                    wait,
                    requester,
                    received,
                    reply,
                } => self.wait_for(job, requester, reply, received + wait),
                Message::Stop {
                    job,
                    requester,
                    reply,
                } => {
                    let now = Instant::now();
                    self.stop(&job, RoomJobStopped::Asked, now);
                    // A stopped job ends within its grace, or the session
                    // does: this is answered then, and not before.
                    self.wait_for(job, requester, reply, now);
                }
                Message::Page {
                    path,
                    from,
                    lines,
                    requester,
                    reply,
                } => {
                    self.readers.push_back(Reader {
                        path,
                        from,
                        lines,
                        requester,
                        reply,
                    });
                    self.asks.push_back(Asking::Page);
                }
                Message::Search {
                    search,
                    requester,
                    reply,
                } => {
                    self.seekers.push_back(Seeker {
                        search,
                        requester,
                        reply,
                    });
                    self.asks.push_back(Asking::Search);
                }
                Message::Change {
                    change,
                    requester,
                    reply,
                } => {
                    self.changers.push_back(Changer {
                        change,
                        requester,
                        reply,
                    });
                    self.asks.push_back(Asking::Change);
                }
                Message::Take {
                    path,
                    at,
                    requester,
                    reply,
                } => {
                    self.takers.push_back(Taker {
                        path,
                        at,
                        requester,
                        reply,
                    });
                    self.asks.push_back(Asking::Take);
                }
                Message::Probe { reply } => self.probes.push(reply),
                Message::Disk {
                    op: op @ (RoomDiskOp::Close | RoomDiskOp::Wipe),
                    reply,
                } => {
                    self.after = Some((op, reply));
                    return Err(String::from(DISK_CLOSING));
                }
                Message::Disk { op, reply } => {
                    let _ = reply.send((self.disk)(op));
                }
                Message::Inbox { op, reply } => {
                    let _ = reply.send((self.inbox)(&op));
                }
                Message::End { reason } => {
                    self.left = true;
                    return Err(String::from(reason));
                }
            }
        }
        Ok(())
    }

    /// Takes a job to run. A name that is known starts nothing: its
    /// caller is told of the job that has it.
    fn run(
        &mut self,
        run: RoomRun,
        checked: Checked,
        requester: UnixStream,
        received: Instant,
        reply: Sender<JobReply>,
    ) {
        let RoomRun {
            job, argv, stdin, ..
        } = run;
        if let Some(state) = self.over(&job) {
            let _ = reply.send(Ok(told(&job, state, false)));
            return;
        }
        let known = self.flights.contains_key(&job);
        if !known {
            let running = self
                .flights
                .values()
                .filter(|flight| flight.landed.is_none());
            if running.count() >= RUNNING_MAX {
                let _ = reply.send(Err(String::from(FULL)));
                return;
            }
            self.flights.insert(
                job.clone(),
                Flight {
                    argv,
                    dir: checked.dir,
                    stdin,
                    asked: received,
                    limit: received + checked.limit,
                    sent: false,
                    started: false,
                    stopping: None,
                    landed: None,
                },
            );
            self.asks.push_back(Asking::Start(job.clone()));
        }
        self.waiters.push(Waiter {
            job,
            requester,
            reply,
            until: received + checked.wait,
            started: !known,
        });
    }

    /// Has a connection wait to be told of `job`: at once, if it is over
    /// or isn't known.
    fn wait_for(
        &mut self,
        job: JobId,
        requester: UnixStream,
        reply: Sender<JobReply>,
        until: Instant,
    ) {
        if let Some(state) = self.over(&job) {
            let _ = reply.send(Ok(told(&job, state, false)));
        } else if self.flights.contains_key(&job) {
            self.waiters.push(Waiter {
                job,
                requester,
                reply,
                until,
                started: false,
            });
        } else {
            let _ = reply.send(Err(String::from(NO_JOB)));
        }
    }

    /// Stops `job` if it is running: its process group is killed. One
    /// that was never sent to the supervisor is let go instead.
    fn stop(&mut self, job: &JobId, why: RoomJobStopped, now: Instant) {
        let Some(flight) = self.flights.get_mut(job) else {
            return;
        };
        if flight.landed.is_some() || flight.stopping.is_some() {
            return;
        }
        if flight.sent {
            flight.stopping = Some((now, why));
            self.asks.push_back(Asking::Stop(job.clone()));
        } else {
            self.unstarted(job, EXPIRED);
        }
    }

    /// Lets go of a job that never started: its name is free, and
    /// whoever waited for it hears why.
    fn unstarted(&mut self, job: &JobId, reason: &str) {
        self.flights.remove(job);
        self.asks
            .retain(|asking| !matches!(asking, Asking::Start(id) if id == job));
        let (theirs, others) = std::mem::take(&mut self.waiters)
            .into_iter()
            .partition(|waiter| waiter.job == *job);
        self.waiters = others;
        for waiter in theirs {
            let _ = waiter.reply.send(Err(String::from(reason)));
        }
    }

    /// Lets go of every caller that `gone` says has gone, and of the jobs
    /// that leaves unwanted.
    fn part(&mut self, mut gone: impl FnMut(&Waiter) -> bool) {
        let (parted, staying): (Vec<Waiter>, Vec<Waiter>) = std::mem::take(&mut self.waiters)
            .into_iter()
            .partition(|waiter| gone(waiter));
        self.waiters = staying;
        for waiter in parted {
            let _ = waiter.reply.send(Err(String::from(REQUESTER_GONE)));
        }
        self.unwanted();
    }

    /// Lets go of every job that nobody waits for any more and that
    /// hasn't been sent to the supervisor: nothing is started for a
    /// caller that has gone.
    fn unwanted(&mut self) {
        let unwanted: Vec<JobId> = self
            .flights
            .iter()
            .filter(|(job, flight)| {
                !flight.sent && !self.waiters.iter().any(|waiter| waiter.job == **job)
            })
            .map(|(job, _)| job.clone())
            .collect();
        for job in unwanted {
            self.unstarted(&job, REQUESTER_GONE);
        }
    }

    /// Lets go of every caller waiting for a page that has gone.
    fn unread(&mut self) {
        let (staying, gone): (VecDeque<Reader>, VecDeque<Reader>) =
            std::mem::take(&mut self.readers)
                .into_iter()
                .partition(|reader| quiet(&reader.requester));
        self.readers = staying;
        for reader in gone {
            let _ = reader.reply.send(Err(String::from(REQUESTER_GONE)));
        }
    }

    /// Lets go of every caller waiting for a search that has gone.
    fn unsought(&mut self) {
        let (staying, gone): (VecDeque<Seeker>, VecDeque<Seeker>) =
            std::mem::take(&mut self.seekers)
                .into_iter()
                .partition(|seeker| quiet(&seeker.requester));
        self.seekers = staying;
        for seeker in gone {
            let _ = seeker.reply.send(Err(String::from(REQUESTER_GONE)));
        }
    }

    /// Lets go of every caller waiting for a change that has gone: a
    /// file is changed for nobody.
    fn unchanged(&mut self) {
        let (staying, gone): (VecDeque<Changer>, VecDeque<Changer>) =
            std::mem::take(&mut self.changers)
                .into_iter()
                .partition(|changer| quiet(&changer.requester));
        self.changers = staying;
        for changer in gone {
            let _ = changer.reply.send(Err(String::from(REQUESTER_GONE)));
        }
    }

    /// Lets go of every caller waiting for a piece of a file that has
    /// gone: nothing is read for nobody.
    fn untaken(&mut self) {
        let (staying, gone): (VecDeque<Taker>, VecDeque<Taker>) = std::mem::take(&mut self.takers)
            .into_iter()
            .partition(|taker| quiet(&taker.requester));
        self.takers = staying;
        for taker in gone {
            let _ = taker.reply.send(Err(String::from(REQUESTER_GONE)));
        }
    }

    /// Sends the supervisor the next thing to ask, when it isn't still
    /// answering the last. One ask is with the supervisor at a time, so
    /// its socket has room for a whole one: a supervisor that can't take
    /// one isn't reading.
    fn ask(&mut self, mut supervisor: &UnixStream, now: Instant) -> Result<(), String> {
        while self.asked.is_none() {
            let Some(asking) = self.asks.pop_front() else {
                return Ok(());
            };
            let nonce = crate::approval::random_hex("").map_err(|error| error.to_string())?;
            let ask = match &asking {
                Asking::Start(job) => self.flights.get_mut(job).map(|flight| {
                    flight.sent = true;
                    Ask::Start {
                        nonce: nonce.clone(),
                        job: job.clone(),
                        argv: std::mem::take(&mut flight.argv),
                        dir: std::mem::take(&mut flight.dir),
                        stdin: std::mem::take(&mut flight.stdin),
                    }
                }),
                Asking::Stop(job) => self
                    .flights
                    .get(job)
                    .filter(|flight| flight.landed.is_none())
                    .map(|_| Ask::Stop {
                        nonce: nonce.clone(),
                        job: job.clone(),
                    }),
                Asking::Piece(job, stream, end) => Some(Ask::Piece {
                    nonce: nonce.clone(),
                    job: job.clone(),
                    stream: *stream,
                    end: *end,
                }),
                // Nothing is read for a caller that has gone.
                Asking::Page => {
                    self.unread();
                    self.readers.pop_front().map(|reader| {
                        self.reading = Some(reader.reply);
                        Ask::Page {
                            nonce: nonce.clone(),
                            path: reader.path,
                            from: reader.from,
                            lines: reader.lines,
                        }
                    })
                }
                // Nor is anything searched for one.
                Asking::Search => {
                    self.unsought();
                    self.seekers.pop_front().map(|seeker| {
                        self.seeking = Some(seeker.reply);
                        Ask::Search {
                            nonce: nonce.clone(),
                            pattern: seeker.search.pattern,
                            path: seeker.search.path,
                            glob: seeker.search.glob,
                            ignore_case: seeker.search.ignore_case,
                            literal: seeker.search.literal,
                        }
                    })
                }
                // And no file is changed for one.
                Asking::Change => self.change_ask(&nonce),
                // Nor is any of a file read for one.
                Asking::Take => {
                    self.untaken();
                    self.takers.pop_front().map(|taker| {
                        self.taking = Some((taker.reply, taker.at));
                        Ask::Take {
                            nonce: nonce.clone(),
                            path: taker.path,
                            at: taker.at,
                        }
                    })
                }
            };
            let Some(ask) = ask else {
                continue;
            };
            let payload = serde_json::to_vec(&ask).map_err(|error| error.to_string())?;
            let size = u32::try_from(payload.len()).map_err(|error| error.to_string())?;
            let mut frame = size.to_be_bytes().to_vec();
            frame.extend(payload);
            match supervisor.write(&frame) {
                Ok(sent) if sent == frame.len() => self.asked = Some((nonce, asking, now)),
                _ => return Err(String::from(UNSENT)),
            }
        }
        Ok(())
    }

    /// The ask for the change the first connection still waiting for one
    /// wants, with `nonce`: none when no caller is still there.
    fn change_ask(&mut self, nonce: &str) -> Option<Ask> {
        self.unchanged();
        let changer = self.changers.pop_front()?;
        self.changing = Some((changer.reply, changer.change.expect()));
        let nonce = nonce.to_owned();
        Some(match changer.change {
            Change::Write {
                path,
                bytes,
                replace,
            } => Ask::Write {
                nonce,
                path,
                bytes,
                replace,
            },
            Change::Edit {
                path,
                old,
                new,
                all,
            } => Ask::Edit {
                nonce,
                path,
                old,
                new,
                all,
            },
        })
    }

    /// Does what is due at `now`: a job at its limit is stopped, and a
    /// caller that has waited as long as it said is told its job is
    /// running. `Err` is why the session ends.
    fn expire(&mut self, clock: &Clock, now: Instant) -> Result<(), String> {
        let (at, due) = clock.due(&self.limits, self.busy());
        if at <= now {
            return Err(String::from(due.reason()));
        }
        if let Some((_, _, since)) = &self.asked {
            if *since + self.limits.answer <= now {
                return Err(String::from(UNANSWERED));
            }
        }
        let mut late = Vec::new();
        for (job, flight) in &self.flights {
            if flight.landed.is_some() {
                continue;
            }
            if let Some((since, _)) = flight.stopping {
                if since + self.limits.stop <= now {
                    return Err(String::from(UNSTOPPED));
                }
            } else if flight.limit <= now {
                late.push(job.clone());
            }
        }
        for job in late {
            self.stop(&job, RoomJobStopped::Limit, now);
        }
        self.waited(now);
        Ok(())
    }

    /// When the next thing is due.
    fn next(&self, clock: &Clock) -> Instant {
        let mut next = clock.due(&self.limits, self.busy()).0;
        if let Some((_, _, since)) = &self.asked {
            next = next.min(*since + self.limits.answer);
        }
        for flight in self.flights.values() {
            if flight.landed.is_none() {
                next = next.min(match flight.stopping {
                    Some((since, _)) => since + self.limits.stop,
                    None => flight.limit,
                });
            }
        }
        for waiter in &self.waiters {
            if self.runs(&waiter.job) {
                next = next.min(waiter.until);
            }
        }
        next
    }

    /// Whether a caller whose wait is over is told `job` is running: the
    /// supervisor has said it started, it isn't being stopped, and it
    /// hasn't ended. One that is being stopped ends within its grace, or
    /// the session does; one that has ended is over once its kept output
    /// is fetched. Their callers are told that instead.
    fn runs(&self, job: &JobId) -> bool {
        let Some(flight) = self.flights.get(job) else {
            return false;
        };
        flight.started && flight.stopping.is_none() && flight.landed.is_none()
    }

    /// Tells every caller that has waited as long as it said that its
    /// job is running, where it is.
    fn waited(&mut self, now: Instant) {
        let (waited, waiting): (Vec<Waiter>, Vec<Waiter>) = std::mem::take(&mut self.waiters)
            .into_iter()
            .partition(|waiter| waiter.until <= now && self.runs(&waiter.job));
        self.waiters = waiting;
        for waiter in waited {
            if let Some(flight) = self.flights.get(&waiter.job) {
                let ran_ms = millis(now.saturating_duration_since(flight.asked));
                let state = RoomJobState::Running { ran_ms };
                let _ = waiter
                    .reply
                    .send(Ok(told(&waiter.job, state, waiter.started)));
            }
        }
    }

    /// Keeps the launched Room until it must end, and says why it must.
    fn keep(
        &mut self,
        supervisor: &UnixStream,
        launcher: BorrowedFd<'_>,
    ) -> Result<Infallible, String> {
        supervisor
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let started = Instant::now();
        let mut clock = Clock {
            started,
            used: started,
            ready: false,
        };
        let mut heard = Vec::new();
        loop {
            self.intake()?;
            let now = Instant::now();
            let busy = self.busy();
            self.expire(&clock, now)?;
            if clock.ready {
                for probe in self.probes.drain(..) {
                    let _ = probe.send(Ok(()));
                }
                self.ask(supervisor, now)?;
            }
            let left = self.next(&clock).saturating_duration_since(now);
            let mut watched = vec![self.seat.wake.as_fd(), supervisor.as_fd(), launcher];
            watched.extend(self.waiters.iter().map(|waiter| waiter.requester.as_fd()));
            let ready = hatchling_sys::wait_readable_for(&watched, left)
                .map_err(|error| error.to_string())?;
            let is = |index: usize| ready.get(index).copied().unwrap_or(false);
            if is(0) && !rested(&self.seat.wake) {
                return Err(String::from(UNOWNED));
            }
            if is(2) {
                return Err(String::from(LAUNCHER_ENDED));
            }
            // The broker never sends a second request on a connection:
            // input or an end there is a caller that has gone. Its job,
            // once the supervisor has it, runs on.
            let mut index = 2_usize;
            self.part(|_| {
                index = index.saturating_add(1);
                is(index)
            });
            if is(1) {
                self.hear(supervisor, &mut heard, &mut clock)?;
            }
            // Idleness is counted from when something was last in hand.
            if busy || self.busy() {
                clock.used = Instant::now();
            }
        }
    }

    /// Takes what the supervisor has sent: its readiness, then answers to
    /// what it was asked and the ends of jobs it was given, and nothing
    /// else.
    fn hear(
        &mut self,
        supervisor: &UnixStream,
        heard: &mut Vec<u8>,
        clock: &mut Clock,
    ) -> Result<(), String> {
        if !clock.ready {
            listen(supervisor, heard, READY.len(), CLOSED_EARLY)?;
            if let Some(body) = frame(heard, READY.len())? {
                if body != READY {
                    return Err(String::from(BAD_READY));
                }
                clock.ready = true;
                clock.used = Instant::now();
            }
            return Ok(());
        }
        listen(supervisor, heard, hatchling_core::MAX_FRAME_BYTES, CLOSED)?;
        while let Some(body) = frame(heard, hatchling_core::MAX_FRAME_BYTES)? {
            let said: Say = serde_json::from_slice(&body).map_err(|_| String::from(BAD_SAY))?;
            self.said(said, Instant::now())?;
        }
        Ok(())
    }

    /// Takes one thing the supervisor said.
    fn said(&mut self, said: Say, now: Instant) -> Result<(), String> {
        if let Say::Ended {
            job,
            how,
            stdout,
            stderr,
        } = said
        {
            return self.ended(&job, how, [stdout, stderr], now);
        }
        let Some((nonce, asking, _)) = self.asked.take() else {
            return Err(String::from(OUT_OF_TURN));
        };
        if said.nonce() != Some(nonce.as_str()) {
            return Err(String::from(OUT_OF_TURN));
        }
        match (asking, said) {
            (Asking::Start(job), Say::Started { .. }) => {
                if let Some(flight) = self.flights.get_mut(&job) {
                    flight.started = true;
                }
                Ok(())
            }
            (Asking::Start(job), Say::NotStarted { why, .. }) => {
                self.unstarted(&job, not_started(why));
                Ok(())
            }
            // A stop that finds its job ended already crossed its end.
            (Asking::Stop(_), Say::Stopping { .. } | Say::NoJob { .. }) => Ok(()),
            (Asking::Piece(job, stream, end), Say::Piece { bytes, .. }) => {
                self.piece(&job, stream, end, bytes)
            }
            (Asking::Page, Say::Page { page, .. }) => {
                if page.check().is_err() {
                    return Err(String::from(BAD_PAGE));
                }
                self.read(Ok(page));
                Ok(())
            }
            (Asking::Page, Say::NoPage { why, .. }) => {
                self.read(Err(no_page(why)));
                Ok(())
            }
            (Asking::Search, Say::Found { found, .. }) => {
                if found.check().is_err() {
                    return Err(String::from(BAD_FOUND));
                }
                self.sought(Ok(found));
                Ok(())
            }
            (Asking::Search, Say::NoSearch { why, .. }) => {
                self.sought(Err(no_search(why)));
                Ok(())
            }
            (Asking::Change, said) => self.changed(said),
            (Asking::Take, Say::Took { took, .. }) => self.took(Ok(took)),
            (Asking::Take, Say::NoTake { why, .. }) => self.took(Err(why)),
            _ => Err(String::from(OUT_OF_TURN)),
        }
    }

    /// Takes the supervisor's answer to the piece of a file it was asked
    /// for, holds it to where it was asked from, and tells whoever waits.
    /// A piece that can't be the one asked for ends the session, and
    /// whoever waits is told why once the Room is down.
    fn took(&mut self, said: Result<Took, NoTake>) -> Result<(), String> {
        let Some(&(_, at)) = self.taking.as_ref() else {
            return Err(String::from(OUT_OF_TURN));
        };
        if said.as_ref().is_ok_and(|took| took.check(at).is_err()) {
            return Err(String::from(BAD_TOOK));
        }
        if let Some((reply, _)) = self.taking.take() {
            let _ = reply.send(said.map_err(no_take));
        }
        Ok(())
    }

    /// Takes the supervisor's answer to the change it was asked for,
    /// holds it to what was asked, and tells whoever waits. An answer to
    /// the other kind of change, or to nothing that was asked, ends the
    /// session.
    fn changed(&mut self, said: Say) -> Result<(), String> {
        let Some((reply, expect)) = self.changing.take() else {
            return Err(String::from(OUT_OF_TURN));
        };
        let answer = match (expect, said) {
            (Expect::Write { .. }, Say::Wrote { wrote, .. }) => {
                if !expect.wrote(wrote) {
                    self.changing = Some((reply, expect));
                    return Err(String::from(BAD_WROTE));
                }
                Ok(Changed::Wrote(wrote))
            }
            (Expect::Write { .. }, Say::NoWrite { why, .. }) => Err(no_write(why)),
            (Expect::Edit { .. }, Say::Edited { edited, .. }) => {
                if !expect.edited(edited) {
                    self.changing = Some((reply, expect));
                    return Err(String::from(BAD_EDITED));
                }
                Ok(Changed::Edited(edited))
            }
            (Expect::Edit { .. }, Say::NoEdit { why, .. }) => {
                if why.check().is_err() {
                    self.changing = Some((reply, expect));
                    return Err(String::from(BAD_EDITED));
                }
                Err(no_edit(why))
            }
            _ => {
                self.changing = Some((reply, expect));
                return Err(String::from(OUT_OF_TURN));
            }
        };
        let _ = reply.send(answer);
        Ok(())
    }

    /// Tells whoever waits for the search the supervisor was asked for.
    fn sought(&mut self, found: SearchReply) {
        if let Some(reply) = self.seeking.take() {
            let _ = reply.send(found);
        }
    }

    /// Tells whoever waits for the page the supervisor was asked for.
    fn read(&mut self, page: PageReply) {
        if let Some(reply) = self.reading.take() {
            let _ = reply.send(page);
        }
    }

    /// Takes the end of a job the supervisor was given, checks it, and
    /// asks for what was kept of its output.
    fn ended(
        &mut self,
        job: &JobId,
        how: Ended,
        kept: [Captured; 2],
        now: Instant,
    ) -> Result<(), String> {
        let Some(flight) = self.flights.get_mut(job) else {
            return Err(String::from(OUT_OF_TURN));
        };
        if !flight.started || flight.landed.is_some() {
            return Err(String::from(OUT_OF_TURN));
        }
        let [stdout, stderr] = kept;
        if how.check().is_err() || stdout.check().is_err() || stderr.check().is_err() {
            return Err(String::from(BAD_END));
        }
        let mut wanted = 0_usize;
        for (stream, captured) in [(Stream::Stdout, stdout), (Stream::Stderr, stderr)] {
            for (end, length) in [(End::Head, captured.head), (End::Tail, captured.tail)] {
                if length > 0 {
                    self.asks.push_back(Asking::Piece(job.clone(), stream, end));
                    wanted = wanted.saturating_add(1);
                }
            }
        }
        flight.landed = Some(Landed {
            state: RoomJobState::Ended {
                how,
                stopped: flight.stopping.map(|(_, why)| why),
                ran_ms: millis(now.saturating_duration_since(flight.asked)),
                stdout,
                stderr,
            },
            pieces: Default::default(),
            wanted,
        });
        if wanted == 0 {
            self.over_now(job);
        }
        Ok(())
    }

    /// Takes a kept piece of an ended job's output: the length the
    /// supervisor said it was, or the session ends.
    fn piece(
        &mut self,
        job: &JobId,
        stream: Stream,
        end: End,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        let Some(landed) = self
            .flights
            .get_mut(job)
            .and_then(|flight| flight.landed.as_mut())
        else {
            return Err(String::from(OUT_OF_TURN));
        };
        let RoomJobState::Ended { stdout, stderr, .. } = &landed.state else {
            return Err(String::from(OUT_OF_TURN));
        };
        let captured = match stream {
            Stream::Stdout => stdout,
            Stream::Stderr => stderr,
        };
        let length = match end {
            End::Head => captured.head,
            End::Tail => captured.tail,
        };
        if u32::try_from(bytes.len()) != Ok(length) {
            return Err(String::from(BAD_PIECE));
        }
        let [out, err] = &mut landed.pieces;
        let [head, tail] = match stream {
            Stream::Stdout => out,
            Stream::Stderr => err,
        };
        *match end {
            End::Head => head,
            End::Tail => tail,
        } = bytes;
        landed.wanted = landed.wanted.saturating_sub(1);
        let whole = landed.wanted == 0;
        if whole {
            self.over_now(job);
        }
        Ok(())
    }

    /// Moves an ended job, its output fetched, to the ledger, and tells
    /// whoever waited for it.
    fn over_now(&mut self, job: &JobId) {
        let Some(landed) = self.flights.remove(job).and_then(|flight| flight.landed) else {
            return;
        };
        let state = landed.state.clone();
        opened(&self.ledger).put(
            job.clone(),
            Over {
                state: landed.state,
                pieces: landed.pieces,
            },
        );
        let (theirs, others) = std::mem::take(&mut self.waiters)
            .into_iter()
            .partition(|waiter| waiter.job == *job);
        self.waiters = others;
        for waiter in theirs {
            let _ = waiter
                .reply
                .send(Ok(told(job, state.clone(), waiter.started)));
        }
    }

    /// Everyone still waiting on this session hears that it can't answer.
    fn refuse(&mut self, reason: &str) {
        let reason = held(reason);
        for owed in self.owed.drain(..) {
            owed.refuse(&reason);
        }
        if let Some((_, reply)) = self.after.take() {
            let _ = reply.send(Err(reason.clone()));
        }
        for probe in self.probes.drain(..) {
            let _ = probe.send(Err(reason.clone()));
        }
    }

    /// Waits for something to be asked, or for the next try at clearing
    /// the session, and answers what can be answered meanwhile. A close or
    /// a wipe is a reason to try again at once.
    fn wait_held(&mut self, reason: &str) {
        let _ = hatchling_sys::wait_readable_for(&[self.seat.wake.as_fd()], self.limits.retry);
        if !rested(&self.seat.wake) {
            self.left = true;
            return;
        }
        while let Ok(message) = self.seat.inbox.try_recv() {
            match message {
                Message::Run { reply, .. }
                | Message::Job { reply, .. }
                | Message::Stop { reply, .. } => {
                    let _ = reply.send(Err(held(reason)));
                }
                Message::Page { reply, .. } => {
                    let _ = reply.send(Err(held(reason)));
                }
                Message::Search { reply, .. } => {
                    let _ = reply.send(Err(held(reason)));
                }
                Message::Change { reply, .. } => {
                    let _ = reply.send(Err(held(reason)));
                }
                Message::Take { reply, .. } => {
                    let _ = reply.send(Err(held(reason)));
                }
                Message::Probe { reply } => {
                    let _ = reply.send(Err(held(reason)));
                }
                Message::Disk {
                    op: op @ (RoomDiskOp::Close | RoomDiskOp::Wipe),
                    reply,
                } => {
                    self.after = Some((op, reply));
                    return;
                }
                Message::Disk { op, reply } => {
                    let _ = reply.send((self.disk)(op));
                }
                Message::Inbox { op, reply } => {
                    let _ = reply.send((self.inbox)(&op));
                }
                Message::End { .. } => {
                    self.left = true;
                    return;
                }
            }
        }
    }

    /// Answers what the session still owes once it is down and its claim
    /// on the disk is let go. `launched` is how its launch went; `direct`
    /// does a disk operation the ordinary way.
    pub(crate) fn finish(
        mut self,
        launched: Result<(), String>,
        direct: impl FnOnce(RoomDiskOp) -> DiskReply,
    ) {
        if let Err(reason) = launched {
            // A launch can fail before anything was taken in: with no
            // disk to open, say. Nothing was started, so every job and
            // every page that was waiting for this Room hears why there
            // is none, and a job's name is free. Whatever else was asked
            // is let go unanswered, and is asked again without a session.
            self.flights.clear();
            for waiter in self.waiters.drain(..) {
                let _ = waiter.reply.send(Err(reason.clone()));
            }
            for reader in self.readers.drain(..) {
                let _ = reader.reply.send(Err(reason.clone()));
            }
            for seeker in self.seekers.drain(..) {
                let _ = seeker.reply.send(Err(reason.clone()));
            }
            for changer in self.changers.drain(..) {
                let _ = changer.reply.send(Err(reason.clone()));
            }
            for taker in self.takers.drain(..) {
                let _ = taker.reply.send(Err(reason.clone()));
            }
            while let Ok(message) = self.seat.inbox.try_recv() {
                match message {
                    Message::Run { reply, .. } => {
                        let _ = reply.send(Err(reason.clone()));
                    }
                    Message::Page { reply, .. } => {
                        let _ = reply.send(Err(reason.clone()));
                    }
                    Message::Search { reply, .. } => {
                        let _ = reply.send(Err(reason.clone()));
                    }
                    Message::Change { reply, .. } => {
                        let _ = reply.send(Err(reason.clone()));
                    }
                    Message::Take { reply, .. } => {
                        let _ = reply.send(Err(reason.clone()));
                    }
                    Message::Job { .. }
                    | Message::Stop { .. }
                    | Message::Probe { .. }
                    | Message::Disk { .. }
                    | Message::Inbox { .. }
                    | Message::End { .. } => {}
                }
            }
        }
        for owed in self.owed.drain(..) {
            owed.pay();
        }
        if let Some((op, reply)) = self.after.take() {
            let _ = reply.send(direct(op));
        }
    }
}

impl<D: FnMut(RoomDiskOp) -> DiskReply, P: FnMut(&InboxOp) -> InboxReply> Session for Kept<D, P> {
    fn begin(&mut self) -> Result<(), String> {
        self.intake()?;
        // Nothing is launched for callers that have all gone.
        let asked = self.wanted();
        self.part(|waiter| !quiet(&waiter.requester));
        self.unread();
        self.unsought();
        self.unchanged();
        self.untaken();
        if asked && !self.wanted() {
            return Err(String::from(REQUESTER_GONE));
        }
        Ok(())
    }

    fn hold(&mut self, supervisor: &UnixStream, launcher: BorrowedFd<'_>) {
        let reason = match self.keep(supervisor, launcher) {
            Ok(never) => match never {},
            Err(reason) => reason,
        };
        tracing::info!(%reason, "the Room session ended");
        // Every job the supervisor was given is lost with the Room. One
        // it was never given didn't start: its name stays free.
        self.asks.clear();
        self.asked = None;
        for (job, flight) in std::mem::take(&mut self.flights) {
            if flight.sent {
                opened(&self.ledger).put(job, Over::lost(&reason));
            }
        }
        for waiter in std::mem::take(&mut self.waiters) {
            let answer = self.over(&waiter.job).map_or_else(
                || Err(reason.clone()),
                |state| Ok(told(&waiter.job, state, waiter.started)),
            );
            self.owed.push(Owed::Job(waiter.reply, answer));
        }
        // A page that wasn't handed back isn't going to be.
        let unread = self.readers.drain(..).map(|reader| reader.reply);
        for reply in self.reading.take().into_iter().chain(unread) {
            self.owed.push(Owed::Page(reply, reason.clone()));
        }
        // Nor is a search that wasn't answered.
        let unsought = self.seekers.drain(..).map(|seeker| seeker.reply);
        for reply in self.seeking.take().into_iter().chain(unsought) {
            self.owed.push(Owed::Search(reply, reason.clone()));
        }
        // A change the supervisor was given and didn't answer may have
        // been made, and nobody heard: its caller is told that. One it
        // was never given wasn't made.
        if let Some((reply, _)) = self.changing.take() {
            self.owed
                .push(Owed::Change(reply, format!("{reason}{UNHEARD}")));
        }
        for changer in self.changers.drain(..) {
            self.owed.push(Owed::Change(changer.reply, reason.clone()));
        }
        // Nor is a piece of a file that wasn't handed back.
        let untaken = self.takers.drain(..).map(|taker| taker.reply);
        let taking = self.taking.take().map(|(reply, _)| reply);
        for reply in taking.into_iter().chain(untaken) {
            self.owed.push(Owed::Take(reply, reason.clone()));
        }
    }

    fn settle(&mut self, down: &mut dyn FnMut() -> Result<(), String>) {
        let mut first = true;
        loop {
            let reason = match down() {
                Ok(()) => return,
                Err(reason) => reason,
            };
            if first {
                tracing::error!(%reason, "a Room session isn't cleared yet; it stays held");
                first = false;
            }
            self.refuse(&reason);
            if self.left {
                return;
            }
            self.wait_held(&reason);
        }
    }
}

/// A session and the thread that holds it.
struct Live {
    door: Door,
    thread: JoinHandle<()>,
}

/// Whether the spawner has a session.
enum Slot {
    Empty,
    Live(Live),
    /// The broker has gone: no session starts again.
    Shut,
}

impl Slot {
    /// Hands `message` to the live session. It comes back when there is
    /// none: one that ended on its own is seen out first, so its cleanup
    /// is done before anything else uses the Room.
    fn hand(&mut self, message: Message) -> Result<(), Message> {
        let Self::Live(live) = self else {
            return Err(message);
        };
        let message = match live.door.send(message) {
            Ok(()) => return Ok(()),
            Err(message) => message,
        };
        if let Self::Live(ended) = std::mem::replace(self, Self::Empty) {
            let _ = ended.thread.join();
        }
        Err(message)
    }
}

/// How one try at asking the session went.
enum Asked<'a, T> {
    /// It answered.
    Answered(Result<T, String>),
    /// It ended before answering.
    Dropped,
    /// There is none. Nothing starts one while the slot is held.
    Alone(MutexGuard<'a, Slot>, Message, Receiver<Result<T, String>>),
}

/// Starts the thread that holds a session.
type Start = Box<dyn Fn(Seat) -> io::Result<JoinHandle<()>> + Send + Sync>;

/// The spawner's one Room session, as its connections reach it.
///
/// Every request about the Room or its disk comes through here. With a
/// session live, its thread answers; with none, the request is done
/// directly while the slot is held, so no session starts under it. What
/// is asked of a job that is over is answered from the ledger, session or
/// none.
pub(crate) struct Sessions {
    slot: Mutex<Slot>,
    start: Start,
    ledger: Arc<Mutex<Ledger>>,
}

impl Sessions {
    /// `start` starts the thread that holds a session; that thread keeps
    /// its jobs in `ledger` when they are over.
    pub(crate) fn new(
        ledger: Arc<Mutex<Ledger>>,
        start: impl Fn(Seat) -> io::Result<JoinHandle<()>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            slot: Mutex::new(Slot::Empty),
            start: Box::new(start),
            ledger,
        }
    }

    /// A job that is over, as a caller that didn't start it is told.
    fn over(&self, job: &JobId) -> Option<RoomJobAnswer> {
        let state = opened(&self.ledger).get(job)?.state.clone();
        Some(told(job, state, false))
    }

    fn ask<T>(
        &self,
        message: &impl Fn(Sender<Result<T, String>>) -> Result<Message, String>,
    ) -> Result<Asked<'_, T>, String> {
        let (reply, answer) = mpsc::channel();
        let message = message(reply)?;
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(*slot, Slot::Shut) {
            return Err(String::from(LEAVING));
        }
        if let Err(message) = slot.hand(message) {
            return Ok(Asked::Alone(slot, message, answer));
        }
        drop(slot);
        Ok(answer.recv().map_or(Asked::Dropped, Asked::Answered))
    }

    /// Asks the live session, or does it `direct`ly when there is none.
    fn ask_or<T>(
        &self,
        message: &impl Fn(Sender<Result<T, String>>) -> Result<Message, String>,
        direct: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        for _ in 0..ATTEMPTS {
            match self.ask(message)? {
                Asked::Answered(answer) => return answer,
                Asked::Dropped => {}
                Asked::Alone(_slot, ..) => return direct(),
            }
        }
        Err(String::from(UNSETTLED))
    }

    /// Runs `run` in the kept Room for the connection at `requester`,
    /// starting a session when there is none. A name the spawner knows
    /// starts nothing.
    pub(crate) fn run(&self, run: &RoomRun, requester: &UnixStream) -> JobReply {
        let checked = crate::room_job::check(run)?;
        let received = Instant::now();
        let message = |reply: Sender<JobReply>| -> Result<Message, String> {
            Ok(Message::Run {
                run: Box::new(run.clone()),
                checked: checked.clone(),
                requester: requester.try_clone().map_err(|error| error.to_string())?,
                received,
                reply,
            })
        };
        self.ask_or_start(&message, || self.over(&run.job))
    }

    /// Hands back a page of the file at `path` in the kept Room: its
    /// lines from line `from`, at most `lines` of them, for the connection
    /// at `requester`. A session is started when there is none.
    pub(crate) fn page(
        &self,
        path: &str,
        from: u64,
        lines: u32,
        requester: &UnixStream,
    ) -> PageReply {
        check_page(path, from, lines)?;
        let message = |reply: Sender<PageReply>| -> Result<Message, String> {
            Ok(Message::Page {
                path: path.to_owned(),
                from,
                lines,
                requester: requester.try_clone().map_err(|error| error.to_string())?,
                reply,
            })
        };
        self.ask_or_start(&message, || None)
    }

    /// Searches the kept Room's files for `search`, for the connection
    /// at `requester`. A session is started when there is none.
    pub(crate) fn search(&self, search: &Search, requester: &UnixStream) -> SearchReply {
        check_search(&search.pattern, &search.path, search.glob.as_deref())?;
        let message = |reply: Sender<SearchReply>| -> Result<Message, String> {
            Ok(Message::Search {
                search: search.clone(),
                requester: requester.try_clone().map_err(|error| error.to_string())?,
                reply,
            })
        };
        self.ask_or_start(&message, || None)
    }

    /// Makes `change` to a file in the kept Room, for the connection at
    /// `requester`. A session is started when there is none.
    ///
    /// A change is handed to a session again only when that session ended
    /// without having taken it. One the supervisor was given and didn't
    /// answer is never asked again: its caller is told it isn't known
    /// whether the file was changed.
    pub(crate) fn change(&self, change: &Change, requester: &UnixStream) -> ChangeReply {
        change.check()?;
        let message = |reply: Sender<ChangeReply>| -> Result<Message, String> {
            Ok(Message::Change {
                change: change.clone(),
                requester: requester.try_clone().map_err(|error| error.to_string())?,
                reply,
            })
        };
        self.ask_or_start(&message, || None)
    }

    /// Hands back a piece of the file at `path` in the kept Room's
    /// workspace, from byte `at`, for the connection at `requester`. A
    /// session is started when there is none.
    pub(crate) fn take(&self, path: &str, at: u64, requester: &UnixStream) -> TakeReply {
        check_take(path, at)?;
        let message = |reply: Sender<TakeReply>| -> Result<Message, String> {
            Ok(Message::Take {
                path: path.to_owned(),
                at,
                requester: requester.try_clone().map_err(|error| error.to_string())?,
                reply,
            })
        };
        self.ask_or_start(&message, || None)
    }

    /// Asks the live session, or starts one for what is asked when there
    /// is none. What `known` answers is the answer, and nothing is asked:
    /// it is looked at before each try.
    fn ask_or_start<T>(
        &self,
        message: &impl Fn(Sender<Result<T, String>>) -> Result<Message, String>,
        known: impl Fn() -> Option<T>,
    ) -> Result<T, String> {
        for _ in 0..ATTEMPTS {
            if let Some(known) = known() {
                return Ok(known);
            }
            match self.ask(message)? {
                Asked::Answered(answer) => return answer,
                Asked::Dropped => {}
                Asked::Alone(mut slot, first, answer) => {
                    let started = Door::pair().and_then(|(door, seat)| {
                        // The thread finds what it was started for
                        // waiting for it.
                        let _ = door.send(first);
                        let thread = (self.start)(seat)?;
                        Ok(Live { door, thread })
                    });
                    *slot = Slot::Live(
                        started
                            .map_err(|error| format!("cannot start the Room session: {error}"))?,
                    );
                    drop(slot);
                    if let Ok(answer) = answer.recv() {
                        return answer;
                    }
                }
            }
        }
        Err(String::from(UNSETTLED))
    }

    /// How `job` is, waiting up to `wait` for it to end. No session is
    /// started for it.
    pub(crate) fn job(&self, job: &JobId, wait: Duration, requester: &UnixStream) -> JobReply {
        let received = Instant::now();
        let message = |reply: Sender<JobReply>| -> Result<Message, String> {
            Ok(Message::Job {
                job: job.clone(),
                wait,
                requester: requester.try_clone().map_err(|error| error.to_string())?,
                received,
                reply,
            })
        };
        let known = || self.over(job).ok_or_else(|| String::from(NO_JOB));
        known().or_else(|_| self.ask_or(&message, known))
    }

    /// Stops `job`, and says how it came out. One that is over already is
    /// told as it is.
    pub(crate) fn stop(&self, job: &JobId, requester: &UnixStream) -> JobReply {
        let message = |reply: Sender<JobReply>| -> Result<Message, String> {
            Ok(Message::Stop {
                job: job.clone(),
                requester: requester.try_clone().map_err(|error| error.to_string())?,
                reply,
            })
        };
        let known = || self.over(job).ok_or_else(|| String::from(NO_JOB));
        known().or_else(|_| self.ask_or(&message, known))
    }

    /// A kept piece of what `job`, which is over, wrote.
    pub(crate) fn output(&self, job: &JobId, stream: Stream, end: End) -> Result<Vec<u8>, String> {
        let kept = || {
            opened(&self.ledger)
                .get(job)
                .map(|over| over.piece(stream, end))
        };
        if let Some(piece) = kept() {
            return piece;
        }
        // Running, or not known: a session with it in hand says which.
        let (ours, _theirs) = UnixStream::pair().map_err(|error| error.to_string())?;
        self.job(job, Duration::ZERO, &ours)?;
        kept().unwrap_or_else(|| Err(String::from(NOT_OVER)))
    }

    /// Whether the installed Room works: a live session says so once its
    /// supervisor is ready, and `check` finds out when there is none.
    pub(crate) fn probe(&self, check: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        let message = |reply: Sender<Result<(), String>>| -> Result<Message, String> {
            Ok(Message::Probe { reply })
        };
        self.ask_or(&message, check)
    }

    /// Does `op` to the room disk: through the live session, which a
    /// close or a wipe ends first, or `direct`ly when there is none.
    pub(crate) fn disk(
        &self,
        op: RoomDiskOp,
        direct: impl FnOnce(RoomDiskOp) -> DiskReply,
    ) -> DiskReply {
        let message = |reply: Sender<DiskReply>| -> Result<Message, String> {
            Ok(Message::Disk { op, reply })
        };
        self.ask_or(&message, || direct(op))
    }

    /// Does `op` to the room disk's inbox: puts a file in it, lists it,
    /// or takes a file out. Through the live session, which holds the
    /// disk, or `direct`ly when there is none. No session is started for
    /// it.
    pub(crate) fn inbox(
        &self,
        op: &InboxOp,
        direct: impl FnOnce(&InboxOp) -> InboxReply,
    ) -> InboxReply {
        let message = |reply: Sender<InboxReply>| -> Result<Message, String> {
            Ok(Message::Inbox {
                op: op.clone(),
                reply,
            })
        };
        self.ask_or(&message, || direct(op))
    }

    /// Ends the session, waits until it is down, and lets no other start.
    pub(crate) fn shut(&self, reason: &'static str) {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Slot::Live(live) = std::mem::replace(&mut *slot, Slot::Shut) {
            let _ = live.door.send(Message::End { reason });
            let _ = live.thread.join();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
