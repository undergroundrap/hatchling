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

use hatchling_core::room::{Ask, Captured, End, Ended, JobId, RUNNING_MAX, Say, Stream};

use crate::room_disk::{RoomDiskOp, RoomDiskStatus};
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
    /// Say whether the installed Room works.
    Probe { reply: Sender<Result<(), String>> },
    /// Do this to the room disk.
    Disk {
        op: RoomDiskOp,
        reply: Sender<DiskReply>,
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
pub(crate) struct Kept<D> {
    seat: Seat,
    limits: Limits,
    ledger: Arc<Mutex<Ledger>>,
    disk: D,
    /// The jobs in hand: running, or ended with output still to fetch.
    flights: BTreeMap<JobId, Flight>,
    waiters: Vec<Waiter>,
    /// What is still to be asked of the supervisor.
    asks: VecDeque<Asking>,
    /// What it has been asked and hasn't answered: its nonce, and when.
    asked: Option<(String, Asking, Instant)>,
    /// Who asked whether the Room works before its supervisor was ready.
    probes: Vec<Sender<Result<(), String>>>,
    /// Who was waiting when the session ended: they hear once the Room is
    /// down, so nothing they then see is still being taken apart.
    owed: Vec<(Sender<JobReply>, JobReply)>,
    /// The close or wipe that ended the session, done once it is down.
    after: Option<(RoomDiskOp, Sender<DiskReply>)>,
    /// The broker has gone: nothing waits on a cleanup that fails.
    left: bool,
}

impl<D: FnMut(RoomDiskOp) -> DiskReply> Kept<D> {
    pub(crate) const fn new(
        seat: Seat,
        limits: Limits,
        ledger: Arc<Mutex<Ledger>>,
        disk: D,
    ) -> Self {
        Self {
            seat,
            limits,
            ledger,
            disk,
            flights: BTreeMap::new(),
            waiters: Vec::new(),
            asks: VecDeque::new(),
            asked: None,
            probes: Vec::new(),
            owed: Vec::new(),
            after: None,
            left: false,
        }
    }

    /// How a job that is over came out.
    fn over(&self, job: &JobId) -> Option<RoomJobState> {
        opened(&self.ledger).get(job).map(|over| over.state.clone())
    }

    /// Whether the session has anything in hand: a job, or an ask the
    /// supervisor hasn't answered. Whoever waits, waits for a job in hand.
    fn busy(&self) -> bool {
        !self.flights.is_empty() || self.asked.is_some()
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
            _ => Err(String::from(OUT_OF_TURN)),
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
        for (reply, _) in self.owed.drain(..) {
            let _ = reply.send(Err(reason.clone()));
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
            // disk to open, say. Nothing was started, so every job that
            // was waiting for this Room hears why there is none, and its
            // name is free. Whatever else was asked is let go unanswered,
            // and is asked again without a session.
            self.flights.clear();
            for waiter in self.waiters.drain(..) {
                let _ = waiter.reply.send(Err(reason.clone()));
            }
            while let Ok(message) = self.seat.inbox.try_recv() {
                if let Message::Run { reply, .. } = message {
                    let _ = reply.send(Err(reason.clone()));
                }
            }
        }
        for (reply, answer) in self.owed.drain(..) {
            let _ = reply.send(answer);
        }
        if let Some((op, reply)) = self.after.take() {
            let _ = reply.send(direct(op));
        }
    }
}

impl<D: FnMut(RoomDiskOp) -> DiskReply> Session for Kept<D> {
    fn begin(&mut self) -> Result<(), String> {
        self.intake()?;
        // Nothing is launched for callers that have all gone.
        let asked = !self.flights.is_empty();
        self.part(|waiter| !quiet(&waiter.requester));
        if asked && self.flights.is_empty() {
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
            self.owed.push((waiter.reply, answer));
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
        for _ in 0..ATTEMPTS {
            if let Some(known) = self.over(&run.job) {
                return Ok(known);
            }
            match self.ask(&message)? {
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
