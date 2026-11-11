//! The Room kept between commands.
//!
//! The install has one Room session at a time. One thread of the spawner
//! holds it from its launch until it is down: the launch, the wait for each
//! command, every reason it ends and the cleanup after are that thread's
//! alone. Connections hand it messages and wait for its answers, so nothing
//! about a session is decided in two places.
//!
//! A session ends at its time limit, when it has been idle, when a command
//! in it fails to finish, when whoever asked for that command goes, when the
//! disk under it is closed or wiped, and when the broker goes. It always
//! ends the same way: its first process is stopped and confirmed gone, its
//! cgroup is drained and its image mapping closed. Until that has worked the
//! thread keeps the Room's disk claimed and refuses new work.

use std::cmp::Ordering;
use std::convert::Infallible;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::room_disk::{RoomDiskOp, RoomDiskStatus};
use crate::room_foreground::{RoomForegroundResult, decode_result};
use crate::room_launch::Session;

/// A second command while one runs: refused, never queued out of sight.
const BUSY: &str = "the Room is busy with another command";
const EXPIRED: &str = "Room foreground deadline expired";
const UNREADY: &str = "the Room supervisor did not become ready in time";
const CAPPED: &str = "the Room session reached its time limit";
const IDLE: &str = "the Room session was idle";
const REQUESTER_GONE: &str = "the broker ended the Room foreground request";
const LAUNCHER_ENDED: &str = "the Room's first process ended";
const OUT_OF_TURN: &str = "the Room supervisor spoke out of turn or closed";
const CLOSED: &str = "the Room supervisor closed before its result was whole";
const CLOSED_EARLY: &str = "the Room supervisor closed before readiness";
const BAD_READY: &str = "the Room supervisor sent invalid readiness";
const UNSENT: &str = "could not dispatch the Room command";
const DISK_CLOSING: &str = "the Room was stopped: its disk is being closed";
const UNOWNED: &str = "the Room session lost its owner";
const UNSETTLED: &str = "the Room session kept ending; try again";
const LEAVING: &str = "the spawner is leaving";
const READY: &[u8] = b"ready";

/// How many times a request is handed to a session that ends before
/// answering it, or starts one, before it is refused.
const ATTEMPTS: usize = 4;

/// What a command in the Room comes to.
pub(crate) type RunReply = Result<RoomForegroundResult, String>;
/// What a room disk operation comes to.
pub(crate) type DiskReply = Result<Option<RoomDiskStatus>, String>;

/// How long a session, and each wait in it, may last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The longest a session lives, busy or not.
    pub(crate) cap: Duration,
    /// How long it may sit with no command before it ends.
    pub(crate) idle: Duration,
    /// How long one command may take, from the moment it is asked for.
    pub(crate) command: Duration,
    /// How long the supervisor may take to say it is ready.
    pub(crate) ready: Duration,
    /// How long between tries to clear a session that won't come down.
    pub(crate) retry: Duration,
}

impl Limits {
    /// What an installed spawner holds every session to.
    pub(crate) const INSTALLED: Self = Self {
        cap: Duration::from_secs(60 * 60),
        idle: Duration::from_secs(15 * 60),
        command: Duration::from_secs(30),
        ready: Duration::from_secs(30),
        retry: Duration::from_secs(5),
    };
}

/// What a connection asks of the session's thread.
pub(crate) enum Message {
    /// Run this command, for the connection at `requester`.
    Run {
        argv: Vec<String>,
        requester: UnixStream,
        received: Instant,
        reply: Sender<RunReply>,
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

/// Which wait of a session runs out first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Due {
    Unready,
    Command,
    Cap,
    Idle,
}

impl Due {
    const fn reason(self) -> &'static str {
        match self {
            Self::Unready => UNREADY,
            Self::Command => EXPIRED,
            Self::Cap => CAPPED,
            Self::Idle => IDLE,
        }
    }
}

/// When a session started, when it last finished something, and whether
/// its supervisor has said it is ready.
struct Clock {
    started: Instant,
    used: Instant,
    ready: bool,
}

impl Clock {
    /// The next wait to run out, and when. A session with a command in
    /// hand is never idle; its time limit holds whatever it is doing.
    fn due(&self, limits: &Limits, command: Option<Instant>) -> (Instant, Due) {
        let waiting = if self.ready {
            (self.used + limits.idle, Due::Idle)
        } else {
            (self.started + limits.ready, Due::Unready)
        };
        let next = match command {
            Some(deadline) if self.ready || deadline < waiting.0 => (deadline, Due::Command),
            _ => waiting,
        };
        let cap = self.started + limits.cap;
        if cap < next.0 { (cap, Due::Cap) } else { next }
    }
}

/// The command a session has in hand.
struct Flight {
    argv: Vec<String>,
    requester: UnixStream,
    reply: Sender<RunReply>,
    deadline: Instant,
    /// What the supervisor must echo, once the command has gone to it.
    nonce: Option<String>,
}

impl Flight {
    /// Sends the command to the supervisor, whole and at once. One command
    /// is with the supervisor at a time, so its socket has room for a
    /// whole request: a supervisor that can't take one isn't reading.
    fn dispatch(&mut self, mut supervisor: &UnixStream) -> Result<(), String> {
        let nonce = crate::approval::random_hex("").map_err(|error| error.to_string())?;
        let request = std::iter::once("run")
            .chain(self.argv.iter().map(String::as_str))
            .chain(std::iter::once(nonce.as_str()))
            .collect::<Vec<_>>();
        let payload = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        let size = u32::try_from(payload.len()).map_err(|error| error.to_string())?;
        let mut frame = size.to_be_bytes().to_vec();
        frame.extend(payload);
        match supervisor.write(&frame) {
            Ok(sent) if sent == frame.len() => {
                self.nonce = Some(nonce);
                Ok(())
            }
            _ => Err(String::from(UNSENT)),
        }
    }
}

/// Reads what the supervisor has sent so far into `heard`: never more
/// than a frame of `limit` bytes and one byte past it, which is enough to
/// tell a frame that is too long or followed by more. The socket doesn't
/// block, so no read here waits or is interrupted.
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
    let most = limit.saturating_add(5);
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

/// The frame `heard` holds, once it holds a whole one and nothing else.
/// It is taken out of `heard`.
fn frame(heard: &mut Vec<u8>, limit: usize) -> Result<Option<Vec<u8>>, String> {
    let Some((size, body)) = heard.split_first_chunk::<4>() else {
        return Ok(None);
    };
    let size = usize::try_from(u32::from_be_bytes(*size)).map_err(|error| error.to_string())?;
    if size > limit {
        return Err(String::from("oversized Room supervisor frame"));
    }
    match body.len().cmp(&size) {
        Ordering::Less => Ok(None),
        Ordering::Greater => Err(String::from("extra data after the Room supervisor's frame")),
        Ordering::Equal => {
            let body = body.to_vec();
            heard.clear();
            Ok(Some(body))
        }
    }
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
    disk: D,
    flight: Option<Flight>,
    /// Who asked whether the Room works before its supervisor was ready.
    probes: Vec<Sender<Result<(), String>>>,
    /// The command the session ended on: it hears why once the Room is
    /// down, so nothing it then sees is still being taken apart.
    owed: Option<(Sender<RunReply>, String)>,
    /// The close or wipe that ended the session, done once it is down.
    after: Option<(RoomDiskOp, Sender<DiskReply>)>,
    /// The broker has gone: nothing waits on a cleanup that fails.
    left: bool,
}

impl<D: FnMut(RoomDiskOp) -> DiskReply> Kept<D> {
    pub(crate) const fn new(seat: Seat, limits: Limits, disk: D) -> Self {
        Self {
            seat,
            limits,
            disk,
            flight: None,
            probes: Vec::new(),
            owed: None,
            after: None,
            left: false,
        }
    }

    /// Takes what has been asked of the session so far. `Err` is why it
    /// ends.
    fn intake(&mut self) -> Result<(), String> {
        while let Ok(message) = self.seat.inbox.try_recv() {
            match message {
                Message::Run {
                    argv,
                    requester,
                    received,
                    reply,
                } => {
                    if self.flight.is_some() {
                        let _ = reply.send(Err(String::from(BUSY)));
                    } else {
                        self.flight = Some(Flight {
                            argv,
                            requester,
                            reply,
                            deadline: received + self.limits.command,
                            nonce: None,
                        });
                    }
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
            if clock.ready {
                for probe in self.probes.drain(..) {
                    let _ = probe.send(Ok(()));
                }
                if let Some(flight) = self.flight.as_mut().filter(|flight| flight.nonce.is_none()) {
                    flight.dispatch(supervisor)?;
                }
            }
            let command = self.flight.as_ref().map(|flight| flight.deadline);
            let (at, due) = clock.due(&self.limits, command);
            let left = at.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(String::from(due.reason()));
            }
            let mut watched = vec![self.seat.wake.as_fd(), supervisor.as_fd(), launcher];
            watched.extend(self.flight.as_ref().map(|flight| flight.requester.as_fd()));
            let ready = hatchling_sys::wait_readable_for(&watched, left)
                .map_err(|error| error.to_string())?;
            let is = |index: usize| ready.get(index).copied().unwrap_or(false);
            if is(0) && !rested(&self.seat.wake) {
                return Err(String::from(UNOWNED));
            }
            // The broker never sends a second request on a connection:
            // input or an end there cancels the command, and the session.
            if is(3) {
                return Err(String::from(REQUESTER_GONE));
            }
            if is(2) {
                return Err(String::from(LAUNCHER_ENDED));
            }
            if is(1) {
                self.hear(supervisor, &mut heard, &mut clock)?;
            }
        }
    }

    /// Takes what the supervisor has sent: its readiness, then the result
    /// of the command it was given, and nothing at any other time.
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
        let Some(nonce) = self.flight.as_ref().and_then(|flight| flight.nonce.clone()) else {
            return Err(String::from(OUT_OF_TURN));
        };
        listen(supervisor, heard, hatchling_core::MAX_FRAME_BYTES, CLOSED)?;
        let Some(body) = frame(heard, hatchling_core::MAX_FRAME_BYTES)? else {
            return Ok(());
        };
        let result = decode_result(&body, &nonce)?;
        if let Some(flight) = self.flight.take() {
            let _ = flight.reply.send(Ok(result));
        }
        clock.used = Instant::now();
        Ok(())
    }

    /// Everyone still waiting on this session hears that it can't answer.
    fn refuse(&mut self, reason: &str) {
        let reason = held(reason);
        if let Some(flight) = self.flight.take() {
            let _ = flight.reply.send(Err(reason.clone()));
        }
        if let Some((reply, _)) = self.owed.take() {
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
                Message::Run { reply, .. } => {
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
            if let Some(flight) = self.flight.take() {
                let _ = flight.reply.send(Err(reason.clone()));
            }
            // A launch can fail before anything was taken in: with no
            // disk to open, say. Every command that was waiting for this
            // Room hears why there is none. Whatever else was asked is
            // let go unanswered, and is asked again without a session.
            while let Ok(message) = self.seat.inbox.try_recv() {
                if let Message::Run { reply, .. } = message {
                    let _ = reply.send(Err(reason.clone()));
                }
            }
        }
        if let Some((reply, reason)) = self.owed.take() {
            let _ = reply.send(Err(reason));
        }
        if let Some((op, reply)) = self.after.take() {
            let _ = reply.send(direct(op));
        }
    }
}

impl<D: FnMut(RoomDiskOp) -> DiskReply> Session for Kept<D> {
    fn begin(&mut self) -> Result<(), String> {
        self.intake()?;
        match &self.flight {
            Some(flight) if !quiet(&flight.requester) => Err(String::from(REQUESTER_GONE)),
            _ => Ok(()),
        }
    }

    fn hold(&mut self, supervisor: &UnixStream, launcher: BorrowedFd<'_>) {
        let reason = match self.keep(supervisor, launcher) {
            Ok(never) => match never {},
            Err(reason) => reason,
        };
        tracing::info!(%reason, "the Room session ended");
        if let Some(flight) = self.flight.take() {
            self.owed = Some((flight.reply, reason));
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
/// directly while the slot is held, so no session starts under it.
pub(crate) struct Sessions {
    slot: Mutex<Slot>,
    start: Start,
}

impl Sessions {
    pub(crate) fn new(
        start: impl Fn(Seat) -> io::Result<JoinHandle<()>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            slot: Mutex::new(Slot::Empty),
            start: Box::new(start),
        }
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

    /// Runs `argv` in the kept Room for the connection at `requester`,
    /// starting a session when there is none.
    pub(crate) fn run(&self, argv: &[String], requester: &UnixStream) -> RunReply {
        let received = Instant::now();
        let message = |reply: Sender<RunReply>| -> Result<Message, String> {
            Ok(Message::Run {
                argv: argv.to_vec(),
                requester: requester.try_clone().map_err(|error| error.to_string())?,
                received,
                reply,
            })
        };
        for _ in 0..ATTEMPTS {
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
