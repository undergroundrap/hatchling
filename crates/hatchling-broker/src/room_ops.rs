//! A session's use of the Room: the agent's own computer.
//!
//! An agent session with its person there (a chat) may run a command in
//! the Room, ask after one it ran, and stop one. The command is a program
//! and its arguments, as they are; the spawner starts it in the Room, as
//! the Room's user, and what comes back is the Room's own words: bounded
//! here, and untrusted wherever it goes.
//!
//! A session names nothing: the broker gives each command its name, so no
//! session can ask after another's. A command still running when its
//! session ends is stopped, since nothing would be there to hear how it
//! came out.

use std::collections::BTreeSet;

use hatchling_core::room::{Captured, End, Ended, JobId, Stream, spill_path};
use hatchling_core::ui::{RoomDiskCondition, RoomDiskView};
use hatchling_core::{
    ROOM_HEAD_BYTES, ROOM_TAIL_BYTES, ROOM_WAIT_MAX_SECS, ROOM_WAIT_SECS, RoomOp, RoomResult,
    RoomStopped, RoomText,
};

use crate::config::Config;
use crate::room_job::{RoomJob, RoomJobAnswer, RoomJobState, RoomJobStopped, RoomRun};

/// What a session without its person there is told.
const UNATTENDED: &str =
    "commands run only in a chat, with the owner there: this run has no computer";

/// What a session is told where there is no Room to ask.
const NO_ROOM: &str = "there is no computer on this install";

/// What runs a command in the Room: the spawner, or a stand-in for it.
pub(crate) trait Rooms {
    /// As [`crate::spawner::Spawner::room_run`].
    fn run(&self, run: &RoomRun) -> Result<RoomJobAnswer, String>;
    /// As [`crate::spawner::Spawner::room_job`].
    fn job(&self, job: &JobId, wait_secs: u32) -> Result<RoomJobAnswer, String>;
    /// As [`crate::spawner::Spawner::room_stop`].
    fn stop(&self, job: &JobId) -> Result<RoomJobAnswer, String>;
    /// As [`crate::spawner::Spawner::room_output`].
    fn output(&self, job: &JobId, stream: Stream, end: End) -> Result<Vec<u8>, String>;
}

impl Rooms for crate::spawner::Spawner {
    fn run(&self, run: &RoomRun) -> Result<RoomJobAnswer, String> {
        self.room_run(run)
    }

    fn job(&self, job: &JobId, wait_secs: u32) -> Result<RoomJobAnswer, String> {
        self.room_job(job, wait_secs)
    }

    fn stop(&self, job: &JobId) -> Result<RoomJobAnswer, String> {
        self.room_stop(job)
    }

    fn output(&self, job: &JobId, stream: Stream, end: End) -> Result<Vec<u8>, String> {
        self.room_output(job, stream, end)
    }
}

/// Whether there is a Room a run could be offered: a spawner, and its
/// room disk open with nothing turning the room off.
pub(crate) fn open() -> bool {
    is_open(
        crate::spawner::spawner().is_some(),
        crate::room_disk::view_here,
    )
}

/// Whether there is a Room to offer, with a spawner or none (`spawner`)
/// and the room disk as `disk` says it is: asked only where there is a
/// spawner to ask.
fn is_open(spawner: bool, disk: impl FnOnce() -> RoomDiskView) -> bool {
    spawner && {
        let disk = disk();
        disk.state == RoomDiskCondition::Open && disk.off.is_none()
    }
}

/// Whether a session under `config` has its person there: a chat, as its
/// web and its activity take it.
pub(crate) const fn attended(config: &Config) -> bool {
    config.web.attended && config.scheduler.followup.is_some()
}

/// Whether a run is offered its computer: a chat (`chat`), with the Room
/// open. Whether it is open is asked only for a chat.
pub(crate) fn offered(chat: bool, open: impl FnOnce() -> bool) -> bool {
    chat && open()
}

/// One session's commands in the Room.
pub(crate) struct RoomSession<'r> {
    /// Whether the session may ask: its person is there.
    attended: bool,
    /// What runs its commands, where there is something to.
    rooms: Option<&'r dyn Rooms>,
    /// What every name this session gives a command starts with.
    stem: String,
    /// How many commands it has named.
    named: u32,
    /// The commands it ran, by name.
    ran: BTreeSet<JobId>,
    /// Those of them known to be over.
    over: BTreeSet<JobId>,
}

impl Drop for RoomSession<'_> {
    /// A session that ends stops what it left running: nothing is there
    /// to ask how it came out.
    fn drop(&mut self) {
        if let Some(rooms) = self.rooms {
            for job in self.ran.difference(&self.over) {
                if let Err(error) = rooms.stop(job) {
                    tracing::warn!(job = job.as_str(), %error, "a session's Room command could not be stopped as it ended");
                }
            }
        }
    }
}

impl<'r> RoomSession<'r> {
    /// The Room for a session under `config`, asked through the broker's
    /// spawner. A session's person is there in a chat, as for its web
    /// and its activity.
    pub(crate) fn new(config: &Config) -> Self {
        let rooms = crate::spawner::spawner().map(|spawner| -> &dyn Rooms { spawner });
        Self::over_rooms(
            attended(config),
            rooms,
            // A name's stem is short for a model to say again, and unlike
            // any other session's. With no entropy there is none, and
            // nothing is run.
            crate::approval::random_hex("j")
                .ok()
                .and_then(|hex| hex.get(..9).map(str::to_owned)),
        )
    }

    /// The Room `rooms` for a session that is `attended`, naming its
    /// commands from `stem`.
    fn over_rooms(attended: bool, rooms: Option<&'r dyn Rooms>, stem: Option<String>) -> Self {
        Self {
            attended,
            rooms: rooms.filter(|_| stem.is_some()),
            stem: stem.unwrap_or_default(),
            named: 0,
            ran: BTreeSet::new(),
            over: BTreeSet::new(),
        }
    }

    /// Answers `op`.
    pub(crate) fn handle(&mut self, op: &RoomOp) -> RoomResult {
        if !self.attended {
            return refused(UNATTENDED);
        }
        let Some(rooms) = self.rooms else {
            return RoomResult::Failed {
                reason: String::from(NO_ROOM),
                job: None,
            };
        };
        match op {
            RoomOp::Run {
                argv,
                dir,
                stdin,
                wait_secs,
                limit_secs,
            } => {
                let Some(job) = self.name() else {
                    return refused("this chat has run as many commands as it may");
                };
                let run = RoomRun {
                    job: job.clone(),
                    argv: argv.clone(),
                    dir: dir.clone(),
                    stdin: stdin.clone().unwrap_or_default().into_bytes(),
                    limit_secs: *limit_secs,
                    wait_secs: wait(*wait_secs),
                };
                // What can't be asked is known here, before anything is.
                if let Err(reason) = crate::room_job::check(&run) {
                    return RoomResult::Refused { reason };
                }
                tracing::info!(
                    job = job.as_str(),
                    program = argv.first().map_or("", String::as_str),
                    arguments = argv.len().saturating_sub(1),
                    "a session runs a command in the Room"
                );
                self.ran.insert(job.clone());
                match rooms.run(&run) {
                    Ok(answer) => self.told(rooms, answer.job),
                    Err(reason) => self.unanswered(rooms, &job, reason),
                }
            }
            RoomOp::Job { job, wait_secs } => {
                let Some(job) = self.own(job) else {
                    return RoomResult::Unknown { job: job.clone() };
                };
                match rooms.job(&job, wait(*wait_secs)) {
                    Ok(answer) => self.told(rooms, answer.job),
                    Err(reason) => RoomResult::Failed {
                        reason,
                        job: Some(job.as_str().to_owned()),
                    },
                }
            }
            RoomOp::Stop { job } => {
                let Some(job) = self.own(job) else {
                    return RoomResult::Unknown { job: job.clone() };
                };
                match rooms.stop(&job) {
                    Ok(answer) => self.told(rooms, answer.job),
                    Err(reason) => RoomResult::Failed {
                        reason,
                        job: Some(job.as_str().to_owned()),
                    },
                }
            }
        }
    }

    /// The next name this session gives a command, while it has one.
    fn name(&mut self) -> Option<JobId> {
        self.named = self.named.checked_add(1)?;
        format!("{}-{}", self.stem, self.named).parse().ok()
    }

    /// The command `name` names, if this session ran it.
    fn own(&self, name: &str) -> Option<JobId> {
        let job: JobId = name.parse().ok()?;
        self.ran.contains(&job).then_some(job)
    }

    /// A run the spawner didn't answer with a job: whether one was
    /// started all the same is asked before anything is said, so nothing
    /// is run twice for a lost answer.
    fn unanswered(&mut self, rooms: &dyn Rooms, job: &JobId, reason: String) -> RoomResult {
        match rooms.job(job, 0) {
            Ok(answer) => self.told(rooms, answer.job),
            Err(none) if none == crate::room_session::NO_JOB => {
                self.ran.remove(job);
                RoomResult::Failed { reason, job: None }
            }
            Err(_) => RoomResult::Failed {
                reason,
                job: Some(job.as_str().to_owned()),
            },
        }
    }

    /// What the session is told of `job`, as the spawner says it is.
    fn told(&mut self, rooms: &dyn Rooms, job: RoomJob) -> RoomResult {
        let name = job.job.as_str().to_owned();
        match job.state {
            RoomJobState::Running { ran_ms } => RoomResult::Running { job: name, ran_ms },
            RoomJobState::Lost { why } => {
                self.over.insert(job.job);
                RoomResult::Lost { job: name, why }
            }
            RoomJobState::Ended {
                how,
                stopped,
                ran_ms,
                stdout,
                stderr,
            } => {
                let stdout = text(rooms, &job.job, Stream::Stdout, &stdout);
                let stderr = text(rooms, &job.job, Stream::Stderr, &stderr);
                self.over.insert(job.job);
                let (exit, signal) = match how {
                    Ended::Exited(code) => (Some(code), None),
                    Ended::Signaled(signal) => (None, Some(signal)),
                };
                RoomResult::Ended {
                    job: name,
                    exit,
                    signal,
                    stopped: stopped.map(|why| match why {
                        RoomJobStopped::Limit => RoomStopped::Limit,
                        RoomJobStopped::Asked => RoomStopped::Asked,
                    }),
                    ran_ms,
                    stdout,
                    stderr,
                }
            }
        }
    }
}

/// A refusal, in `reason`'s words.
fn refused(reason: &str) -> RoomResult {
    RoomResult::Refused {
        reason: reason.to_owned(),
    }
}

/// How long an op waits for its command to end, of the wait it names.
fn wait(asked: Option<u32>) -> u32 {
    asked.unwrap_or(ROOM_WAIT_SECS).min(ROOM_WAIT_MAX_SECS)
}

/// What the ended command `job` wrote to `stream`, as much as is shown,
/// from the pieces the spawner kept. A piece that can't be had shows
/// nothing, and says how much that leaves out.
fn text(rooms: &dyn Rooms, job: &JobId, stream: Stream, kept: &Captured) -> RoomText {
    let piece = |end, held: u32| {
        if held == 0 {
            return Vec::new();
        }
        rooms.output(job, stream, end).unwrap_or_else(|error| {
            tracing::warn!(job = job.as_str(), %error, "what a Room command wrote could not be had");
            Vec::new()
        })
    };
    let head = piece(End::Head, kept.head);
    let tail = piece(End::Tail, kept.tail);
    let (head, tail, left_out) = shown(&head, &tail, kept);
    RoomText {
        head,
        tail,
        left_out,
        total: kept.total(),
        file: spill_path(job, stream),
        file_cut: kept.spill_cut,
    }
}

/// The start and the end of a stream that are shown, and how many of its
/// bytes aren't, from the kept pieces `head` and `tail` and the counts
/// `kept`. A stream no longer than what is shown is all in the start.
fn shown(head: &[u8], tail: &[u8], kept: &Captured) -> (String, String, u64) {
    let whole = kept.dropped == 0;
    if whole && head.len().saturating_add(tail.len()) <= ROOM_HEAD_BYTES + ROOM_TAIL_BYTES {
        let all = [head, tail].concat();
        let left_out = kept.total().saturating_sub(length(&all));
        return (words(&all), String::new(), left_out);
    }
    let first = start(head, ROOM_HEAD_BYTES);
    // The end is the tail piece's. Where that is short, the stream has no
    // gap, and its end reaches back into the head piece.
    let rest = if whole {
        [head.get(first.len()..).unwrap_or_default(), tail].concat()
    } else {
        tail.to_vec()
    };
    let last = finish(&rest, ROOM_TAIL_BYTES);
    let left_out = kept
        .total()
        .saturating_sub(length(first))
        .saturating_sub(length(last));
    (words(first), words(last), left_out)
}

/// How many bytes `bytes` is.
fn length(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

/// `bytes` as text: what isn't UTF-8 is shown as the replacement
/// character.
fn words(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Whether `byte` is inside a UTF-8 character, not the start of one.
const fn inside(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

/// The longest a character is in UTF-8, less one: how far inside one a
/// cut can be.
const INSIDE_MOST: usize = 3;

/// At most the first `most` bytes of `bytes`, ending on a whole character
/// where it is cut. Bytes that are no character's are cut where they are.
fn start(bytes: &[u8], most: usize) -> &[u8] {
    let end = bytes.len().min(most);
    let whole = |at: &usize| bytes.get(*at).is_none_or(|byte| !inside(*byte));
    let cut = (end.saturating_sub(INSIDE_MOST)..=end)
        .rev()
        .find(whole)
        .unwrap_or(end);
    bytes.get(..cut).unwrap_or_default()
}

/// At most the last `most` bytes of `bytes`, starting on a whole character
/// where it is cut. Bytes that are no character's are cut where they are.
fn finish(bytes: &[u8], most: usize) -> &[u8] {
    let from = bytes.len().saturating_sub(most);
    let whole = |at: &usize| bytes.get(*at).is_none_or(|byte| !inside(*byte));
    let cut = (from..=from.saturating_add(INSIDE_MOST))
        .find(whole)
        .unwrap_or(from);
    bytes.get(cut..).unwrap_or_default()
}

#[cfg(test)]
mod tests;
