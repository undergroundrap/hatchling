use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as Atomic};

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::room::{NotStarted, PAGE_BYTES};

use super::*;
use crate::room_job::tests::{job, run as a_run};

/// Longer than any test takes, and short enough that a session that
/// fails to end on what a test does to it ends on this, with its reason.
const LONG: Duration = Duration::from_secs(10);

/// Long enough to be told from no wait at all, and short enough to wait
/// out.
const SHORT: Duration = Duration::from_millis(300);

const fn limits() -> Limits {
    Limits {
        cap: LONG,
        idle: LONG,
        ready: LONG,
        answer: LONG,
        stop: LONG,
        retry: LONG,
    }
}

type Disk = Box<dyn FnMut(RoomDiskOp) -> DiskReply + Send>;

/// The far ends of everything a session watches.
struct Rig {
    door: Door,
    /// The supervisor's own end of its socket.
    room: UnixStream,
    /// The spawner's end of it.
    supervisor: UnixStream,
    /// Readable once `alive` is dropped, as a pidfd is once its process ends.
    launcher: OwnedFd,
    alive: OwnedFd,
    /// What the session did to the disk while it held it.
    ops: Arc<Mutex<Vec<RoomDiskOp>>>,
    ledger: Arc<Mutex<Ledger>>,
}

fn new_rig(limits: Limits) -> (Rig, Kept<Disk>) {
    let (door, seat) = Door::pair().unwrap();
    let (supervisor, room) = UnixStream::pair().unwrap();
    room.set_read_timeout(Some(LONG)).unwrap();
    let (launcher, alive) = hatchling_sys::pipe().unwrap();
    let ops = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&ops);
    let disk: Disk = Box::new(move |op| {
        seen.lock().unwrap().push(op);
        Ok(None)
    });
    let ledger: Arc<Mutex<Ledger>> = Arc::default();
    (
        Rig {
            door,
            room,
            supervisor,
            launcher,
            alive,
            ops,
            ledger: Arc::clone(&ledger),
        },
        Kept::new(seat, limits, ledger, disk),
    )
}

/// What a job that the ordinary limits hold is held to.
fn ordinary() -> Checked {
    Checked {
        dir: String::from("/workspace"),
        limit: LONG,
        wait: LONG,
    }
}

/// Asks the session for a job, held to `checked`: the broker's end of the
/// connection it is asked on, and where its answer comes.
fn ask_held(door: &Door, name: &str, checked: Checked) -> (UnixStream, Receiver<JobReply>) {
    let (broker, requester) = UnixStream::pair().unwrap();
    let (reply, answer) = mpsc::channel();
    let sent = door.send(Message::Run {
        run: Box::new(a_run(name, &["true"])),
        checked,
        requester,
        received: Instant::now(),
        reply,
    });
    assert!(sent.is_ok(), "the session had ended");
    (broker, answer)
}

fn ask_run(door: &Door, name: &str) -> (UnixStream, Receiver<JobReply>) {
    ask_held(door, name, ordinary())
}

fn ask_job(door: &Door, name: &str, wait: Duration) -> (UnixStream, Receiver<JobReply>) {
    let (broker, requester) = UnixStream::pair().unwrap();
    let (reply, answer) = mpsc::channel();
    let sent = door.send(Message::Job {
        job: job(name),
        wait,
        requester,
        received: Instant::now(),
        reply,
    });
    assert!(sent.is_ok(), "the session had ended");
    (broker, answer)
}

fn ask_stop(door: &Door, name: &str) -> (UnixStream, Receiver<JobReply>) {
    let (broker, requester) = UnixStream::pair().unwrap();
    let (reply, answer) = mpsc::channel();
    let sent = door.send(Message::Stop {
        job: job(name),
        requester,
        reply,
    });
    assert!(sent.is_ok(), "the session had ended");
    (broker, answer)
}

/// Asks the session for a page of the file at `path`: the broker's end of
/// the connection it is asked on, and where its answer comes.
fn ask_page(door: &Door, path: &str, from: u64, lines: u32) -> (UnixStream, Receiver<PageReply>) {
    let (broker, requester) = UnixStream::pair().unwrap();
    let (reply, answer) = mpsc::channel();
    let sent = door.send(Message::Page {
        path: path.to_owned(),
        from,
        lines,
        requester,
        reply,
    });
    assert!(sent.is_ok(), "the session had ended");
    (broker, answer)
}

/// Takes the ask for a page as the supervisor hears it: its nonce, and
/// what it asks for.
fn page_asked(room: &UnixStream) -> (String, (String, u64, u32)) {
    let ask = hears(room);
    let Ask::Page {
        nonce,
        path,
        from,
        lines,
    } = ask
    else {
        unreachable!("asked {ask:?}, not for a page");
    };
    (nonce, (path, from, lines))
}

/// A page of `text`, with more of its file after it.
fn page_of(text: &[u8]) -> Page {
    Page {
        bytes: text.to_vec(),
        more: true,
        cut: false,
        size: 99,
    }
}

fn ask_disk(door: &Door, op: RoomDiskOp) -> Receiver<DiskReply> {
    let (reply, answer) = mpsc::channel();
    assert!(door.send(Message::Disk { op, reply }).is_ok());
    answer
}

fn ask_probe(door: &Door) -> Receiver<Result<(), String>> {
    let (reply, answer) = mpsc::channel();
    assert!(door.send(Message::Probe { reply }).is_ok());
    answer
}

fn wait<T>(answer: &Receiver<T>) -> T {
    answer.recv_timeout(LONG).expect("no answer came")
}

/// Whether nothing comes for `answer` in a moment.
fn unanswered<T>(answer: &Receiver<T>) -> bool {
    answer.recv_timeout(SHORT).is_err()
}

fn say_ready(room: &UnixStream) {
    write_frame(room, READY).unwrap();
}

/// What the supervisor is asked next.
fn hears(room: &UnixStream) -> Ask {
    let ask: Ask = serde_json::from_slice(&read_frame(room).unwrap()).unwrap();
    assert_eq!(ask.nonce().len(), 64);
    ask
}

fn says(room: &UnixStream, said: &Say) {
    write_frame(room, &serde_json::to_vec(said).unwrap()).unwrap();
}

/// Whether the supervisor is asked nothing for `time`.
fn unasked(room: &UnixStream, time: Duration) -> bool {
    hatchling_sys::wait_readable_for(&[room.as_fd()], time).unwrap() == [false]
}

/// Takes the start of the job `name` as the supervisor is asked for it,
/// and says it started.
fn starts(room: &UnixStream, name: &str) -> Ask {
    let ask = hears(room);
    let Ask::Start {
        nonce, job: asked, ..
    } = &ask
    else {
        unreachable!("asked {ask:?}, not to start {name}");
    };
    assert_eq!(*asked, job(name));
    says(
        room,
        &Say::Started {
            nonce: nonce.clone(),
        },
    );
    ask
}

/// What was kept of a short stream.
fn short(out: &[u8]) -> Captured {
    let length = u32::try_from(out.len()).unwrap();
    Captured {
        head: length,
        spilled: u64::from(length),
        ..Captured::default()
    }
}

/// Says the job `name` ended, having written `out`, and hands over what
/// was kept of it when asked.
fn ends(room: &UnixStream, name: &str, how: Ended, out: &[u8]) {
    says(
        room,
        &Say::Ended {
            job: job(name),
            how,
            stdout: short(out),
            stderr: Captured::default(),
        },
    );
    if out.is_empty() {
        return;
    }
    let ask = hears(room);
    let Ask::Piece {
        nonce,
        job: asked,
        stream,
        end,
    } = ask
    else {
        unreachable!("asked {ask:?}, not for a piece of {name}");
    };
    assert_eq!((asked, stream, end), (job(name), Stream::Stdout, End::Head));
    says(
        room,
        &Say::Piece {
            nonce,
            bytes: out.to_vec(),
        },
    );
}

/// A whole job, as the supervisor serves it.
fn serves(room: &UnixStream, name: &str, code: u8, out: &[u8]) {
    starts(room, name);
    ends(room, name, Ended::Exited(code), out);
}

/// How a job that ended came out: how, why it was stopped, and what was
/// kept of its standard output.
fn outcome(state: &RoomJobState) -> (Ended, Option<RoomJobStopped>, Captured) {
    let RoomJobState::Ended {
        how,
        stopped,
        stdout,
        ..
    } = state
    else {
        unreachable!("{state:?} isn't an end");
    };
    (*how, *stopped, *stdout)
}

/// The answer to an ask: whether it started the job, and how the job is.
fn answered(answer: &Receiver<JobReply>) -> (bool, RoomJobState) {
    let told = wait(answer).unwrap();
    (told.started, told.job.state)
}

/// The ends of a session's Room that a test may close: each stays open
/// until the session has ended unless the test takes it.
struct Far {
    room: Option<UnixStream>,
    alive: Option<OwnedFd>,
}

impl Far {
    const fn room(&self) -> &UnixStream {
        self.room.as_ref().unwrap()
    }
}

/// Ends `stream` for its other end now. Dropping it alone doesn't where
/// the end must be seen at once: another test's forked child may hold a
/// copy of it for a moment, and the socket closes only with the last.
fn gone(stream: &UnixStream) {
    stream.shutdown(std::net::Shutdown::Both).unwrap();
}

fn send(mut room: &UnixStream, bytes: &[u8]) {
    room.write_all(bytes).unwrap();
}

/// Closes a session's supervisor socket when a test leaves, however it
/// leaves: a test that fails must not wait on a session nothing ends.
struct ClosedAtEnd(UnixStream);

impl Drop for ClosedAtEnd {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// Waits for a session to end by itself, for longer than any of its
/// waits, then closes its supervisor's socket, so that the thread that
/// holds it can be joined either way.
fn seen_out<T>(held: std::thread::ScopedJoinHandle<'_, T>, room: &UnixStream) -> T {
    let until = Instant::now() + LONG * 2;
    while !held.is_finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = room.shutdown(std::net::Shutdown::Both);
    held.join().unwrap()
}

/// Why a session ended that `script` was the supervisor of, with a first
/// job, `first`, asked for when `asked`. Nothing else ends it: its owner,
/// its requester and whatever `script` left open outlive it.
fn ended_with(limits: Limits, asked: bool, script: impl FnOnce(&mut Far)) -> String {
    let (rig, mut kept) = new_rig(limits);
    let Rig {
        door,
        room,
        supervisor,
        launcher,
        alive,
        ..
    } = rig;
    let _asked = asked.then(|| ask_run(&door, "first"));
    let closer = room.try_clone().unwrap();
    let mut far = Far {
        room: Some(room),
        alive: Some(alive),
    };
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(closer.try_clone().unwrap());
        let held = scope.spawn(|| kept.keep(&supervisor, launcher.as_fd()).unwrap_err());
        script(&mut far);
        seen_out(held, &closer)
    })
}

fn ended_by(asked: bool, script: impl FnOnce(&mut Far)) -> String {
    ended_with(limits(), asked, script)
}

#[test]
fn an_installed_session_lives_an_hour_and_idles_fifteen_minutes() {
    assert_eq!(
        Limits::INSTALLED,
        Limits {
            cap: Duration::from_secs(3600),
            idle: Duration::from_secs(900),
            ready: Duration::from_secs(30),
            answer: Duration::from_secs(10),
            stop: Duration::from_secs(5),
            retry: Duration::from_secs(5),
        }
    );
}

/// Which of a session's own waits ends it first: readiness until the
/// supervisor is ready, then idleness unless it has something in hand,
/// and the time limit over all of them. A tie goes to the wait itself.
#[test]
fn the_next_wait_to_run_out_follows_what_the_session_is_doing() {
    let limits = Limits {
        cap: Duration::from_secs(100),
        idle: Duration::from_secs(10),
        ready: Duration::from_secs(5),
        ..limits()
    };
    let started = Instant::now();
    let at = |seconds| started + Duration::from_secs(seconds);
    let clock = |used, ready| Clock {
        started,
        used: at(used),
        ready,
    };
    let unready = clock(0, false);
    assert_eq!(unready.due(&limits, false), (at(5), Due::Unready));
    assert_eq!(unready.due(&limits, true), (at(5), Due::Unready));

    let idle = clock(20, true);
    assert_eq!(idle.due(&limits, false), (at(30), Due::Idle));
    assert_eq!(idle.due(&limits, true), (at(100), Due::Cap));

    assert_eq!(clock(95, true).due(&limits, false), (at(100), Due::Cap));
    assert_eq!(clock(90, true).due(&limits, false), (at(100), Due::Idle));
    let short = Limits {
        cap: Duration::from_secs(3),
        ..limits
    };
    assert_eq!(unready.due(&short, false), (at(3), Due::Cap));
    let even = Limits {
        cap: Duration::from_secs(5),
        ..limits
    };
    assert_eq!(unready.due(&even, false), (at(5), Due::Unready));

    assert_eq!(Due::Unready.reason(), UNREADY);
    assert_eq!(Due::Cap.reason(), CAPPED);
    assert_eq!(Due::Idle.reason(), IDLE);
}

/// A frame is taken only when it is whole, none longer than its limit is
/// taken at all, and what follows a frame stays for the next taking.
#[test]
fn a_frame_is_taken_whole_and_the_next_stays() {
    let mut heard = b"\0\0\0".to_vec();
    assert_eq!(frame(&mut heard, 5), Ok(None));
    assert_eq!(heard, b"\0\0\0");
    let mut heard = b"\0\0\0\x05read".to_vec();
    assert_eq!(frame(&mut heard, 5), Ok(None));
    heard.push(b'y');
    assert_eq!(frame(&mut heard, 5), Ok(Some(b"ready".to_vec())));
    assert!(heard.is_empty(), "a taken frame stayed");
    let mut heard = b"\0\0\0\x02ab\0\0\0\x01c\0\0".to_vec();
    assert_eq!(frame(&mut heard, 5), Ok(Some(b"ab".to_vec())));
    assert_eq!(frame(&mut heard, 5), Ok(Some(b"c".to_vec())));
    assert_eq!(frame(&mut heard, 5), Ok(None));
    assert_eq!(heard, b"\0\0", "the start of the next frame was lost");
    let mut heard = b"\0\0\0\x06".to_vec();
    assert!(frame(&mut heard, 5).unwrap_err().contains("oversized"));
    let mut heard = b"\0\0\0\0".to_vec();
    assert_eq!(frame(&mut heard, 0), Ok(Some(Vec::new())));
    assert!(heard.is_empty());
}

/// Listening takes what is there and stops; it never holds more than a
/// frame of its limit with its length, and it says a closed socket is
/// closed.
#[test]
fn listening_is_bounded_by_the_frame_it_is_for() {
    let (stream, mut peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    let mut heard = Vec::new();
    assert_eq!(listen(&stream, &mut heard, 10, "closed"), Ok(()));
    assert!(heard.is_empty());
    peer.write_all(&[7; 9000]).unwrap();
    assert_eq!(listen(&stream, &mut heard, 8200, "closed"), Ok(()));
    assert_eq!(heard.len(), 8204, "the bound is a frame and its length");
    assert_eq!(listen(&stream, &mut heard, 8200, "closed"), Ok(()));
    assert_eq!(heard.len(), 8204, "a full buffer took more");
    heard.clear();
    assert_eq!(listen(&stream, &mut heard, 8200, "closed"), Ok(()));
    assert_eq!(heard, vec![7; 796], "what was left was lost");
    heard.clear();
    peer.write_all(b"last").unwrap();
    gone(&peer);
    drop(peer);
    assert_eq!(listen(&stream, &mut heard, 10, "closed"), Ok(()));
    assert_eq!(heard, b"last", "what came before the close was dropped");
    assert_eq!(
        listen(&stream, &mut heard, 10, "closed"),
        Err(String::from("closed"))
    );
    assert_eq!(heard, b"last");
}

/// A read that fails is an end, not silence to wait through. A socket
/// that was never connected fails every read.
#[test]
fn a_failed_read_from_the_supervisor_is_not_silence() {
    use rustix::net::{AddressFamily, SocketType, socket};
    let stream = UnixStream::from(socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap());
    stream.set_nonblocking(true).unwrap();
    let mut heard = Vec::new();
    let said = listen(&stream, &mut heard, 10, "closed").unwrap_err();
    assert!(
        said.starts_with("cannot read from the Room supervisor"),
        "{said}"
    );
    assert!(heard.is_empty());
}

/// An ask is with the supervisor only when all of it went at once. One
/// the socket took part of isn't sent, and awaits no answer.
#[test]
fn an_ask_the_supervisor_cannot_take_whole_is_not_sent() {
    let (rig, mut kept) = new_rig(limits());
    rig.supervisor.set_nonblocking(true).unwrap();
    // As small as the kernel allows: far less than the start below.
    rustix::net::sockopt::set_socket_send_buffer_size(&rig.supervisor, 1).unwrap();
    let (_broker, requester) = UnixStream::pair().unwrap();
    let (reply, _answer) = mpsc::channel();
    let mut large = a_run("large", &["true"]);
    large.stdin = vec![0; crate::room_job::STDIN_BYTES];
    kept.run(large, ordinary(), requester, Instant::now(), reply);
    assert_eq!(
        kept.ask(&rig.supervisor, Instant::now()),
        Err(String::from(UNSENT))
    );
    assert!(kept.asked.is_none(), "an unsent ask awaits an answer");

    let (rig, mut kept) = new_rig(limits());
    rig.supervisor.set_nonblocking(true).unwrap();
    let (_broker, requester) = UnixStream::pair().unwrap();
    let (reply, _answer) = mpsc::channel();
    kept.run(
        a_run("small", &["true"]),
        ordinary(),
        requester,
        Instant::now(),
        reply,
    );
    assert_eq!(kept.ask(&rig.supervisor, Instant::now()), Ok(()));
    let asked = hears(&rig.room);
    assert_eq!(
        kept.asked.as_ref().map(|(nonce, ..)| nonce.as_str()),
        Some(asked.nonce())
    );
    // One at a time: nothing more goes until that is answered.
    assert_eq!(kept.ask(&rig.supervisor, Instant::now()), Ok(()));
    assert!(unasked(&rig.room, Duration::ZERO));
}

#[test]
fn the_wake_socket_is_emptied_and_says_when_its_owner_is_gone() {
    let (door, seat) = Door::pair().unwrap();
    assert!(rested(&seat.wake), "an empty wake socket is still owned");
    for _ in 0..200 {
        assert!(door.send(Message::End { reason: "x" }).is_ok());
    }
    assert!(rested(&seat.wake));
    let quiet = hatchling_sys::wait_readable_for(&[seat.wake.as_fd()], Duration::ZERO).unwrap();
    assert_eq!(quiet, [false], "wake bytes were left behind");
    gone(&door.wake);
    drop(door);
    assert!(!rested(&seat.wake));
}

#[test]
fn a_length_of_time_is_told_in_whole_milliseconds() {
    assert_eq!(millis(Duration::from_millis(1500)), 1500);
    assert_eq!(millis(Duration::from_micros(2999)), 2);
    assert_eq!(millis(Duration::ZERO), 0);
    assert_eq!(millis(Duration::MAX), u64::MAX);
}

/// When the session next has something to do: the earliest of its own
/// wait, an ask the supervisor hasn't answered, each running job's limit
/// or the end of its grace once it is stopped, and the wait of each
/// caller whose job runs.
#[test]
fn the_next_thing_due_is_the_earliest_in_hand() {
    let limits = Limits {
        cap: Duration::from_secs(100),
        idle: Duration::from_secs(50),
        ready: Duration::from_secs(5),
        answer: Duration::from_secs(7),
        stop: Duration::from_secs(3),
        retry: LONG,
    };
    let (_rig, mut kept) = new_rig(limits);
    let began = Instant::now();
    let at = |seconds: u64| began + Duration::from_secs(seconds);
    let clock = Clock {
        started: began,
        used: began,
        ready: true,
    };
    assert_eq!(kept.next(&clock), at(50), "idle, with nothing in hand");
    kept.flights.insert(
        job("a"),
        Flight {
            argv: Vec::new(),
            dir: String::new(),
            stdin: Vec::new(),
            asked: began,
            limit: at(20),
            sent: true,
            started: true,
            stopping: None,
            landed: None,
        },
    );
    assert_eq!(kept.next(&clock), at(20), "a running job's limit");
    kept.asked = Some((String::from("n"), Asking::Stop(job("a")), at(1)));
    assert_eq!(kept.next(&clock), at(8), "an ask's answer");
    kept.asked = None;
    let stopped = Some((at(2), RoomJobStopped::Limit));
    kept.flights.get_mut(&job("a")).unwrap().stopping = stopped;
    assert_eq!(kept.next(&clock), at(5), "a stopped job's grace");
    // A caller's wait counts only while its job runs.
    let (_broker, requester) = UnixStream::pair().unwrap();
    let (reply, _answer) = mpsc::channel();
    kept.waiters.push(Waiter {
        job: job("a"),
        requester,
        reply,
        until: at(1),
        started: true,
    });
    assert_eq!(kept.next(&clock), at(5), "a wait on a job being stopped");
    kept.flights.get_mut(&job("a")).unwrap().stopping = None;
    assert_eq!(kept.next(&clock), at(1), "a wait on a running job");
    kept.flights.get_mut(&job("a")).unwrap().started = false;
    assert_eq!(kept.next(&clock), at(20), "a wait on a job not started");
    // A job that has ended has no limit left to reach, and isn't running.
    let flight = kept.flights.get_mut(&job("a")).unwrap();
    flight.started = true;
    flight.landed = Some(Landed {
        state: RoomJobState::Lost { why: String::new() },
        pieces: Default::default(),
        wanted: 1,
    });
    assert_eq!(kept.next(&clock), at(100), "an ended job");
}

/// A job that is running keeps its session from idling, though nobody
/// waits for it and nothing is asked of the supervisor.
#[test]
fn a_running_job_keeps_its_session_from_idling() {
    let (rig, mut kept) = new_rig(Limits {
        idle: SHORT,
        ..limits()
    });
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let impatient = Checked {
            wait: Duration::ZERO,
            ..ordinary()
        };
        let (_broker, answer) = ask_held(&rig.door, "quiet", impatient);
        starts(&rig.room, "quiet");
        assert!(matches!(answered(&answer).1, RoomJobState::Running { .. }));
        std::thread::sleep(SHORT * 3);
        assert!(!held.is_finished(), "a session with a job running idled");
        ends(&rig.room, "quiet", Ended::Exited(0), b"");
        assert_eq!(seen_out(held, &rig.room), IDLE);
    });
}

/// The ledger keeps only so many jobs that are over: the oldest goes.
#[test]
fn the_ledger_lets_its_oldest_job_go() {
    let mut ledger = Ledger::default();
    for number in 0..=LEDGER_MAX {
        ledger.put(job(&format!("job-{number}")), Over::lost("then"));
    }
    assert!(ledger.get(&job("job-0")).is_none());
    assert_eq!(ledger.get(&job("job-1")), Some(&Over::lost("then")));
    assert!(ledger.get(&job(&format!("job-{LEDGER_MAX}"))).is_some());
    assert_eq!(ledger.over.len(), LEDGER_MAX);
}

/// A second job runs in the Room the first one started: the session is
/// still held between them, each is asked of the supervisor as it was
/// asked for, and each answer is its own job's, with what it wrote.
#[test]
fn a_session_runs_one_job_after_another_in_the_same_room() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        let (_first_broker, first) = ask_run(&rig.door, "one");
        say_ready(&rig.room);
        let asked = starts(&rig.room, "one");
        let Ask::Start {
            argv, dir, stdin, ..
        } = asked
        else {
            unreachable!()
        };
        assert_eq!(
            (argv.as_slice(), dir.as_str()),
            (&[String::from("true")][..], "/workspace")
        );
        assert!(stdin.is_empty());
        ends(&rig.room, "one", Ended::Exited(23), b"out\n");
        let (started, state) = answered(&first);
        assert!(started);
        assert_eq!(outcome(&state), (Ended::Exited(23), None, short(b"out\n")));

        let (_second_broker, second) = ask_run(&rig.door, "two");
        serves(&rig.room, "two", 0, b"");
        assert_eq!(outcome(&answered(&second).1).0, Ended::Exited(0));
        assert!(!held.is_finished(), "the session ended between jobs");
        assert!(rig.door.send(Message::End { reason: "done" }).is_ok());
        assert_eq!(held.join().unwrap(), "done");
    });
    assert!(opened(&rig.ledger).get(&job("two")).is_some());
    let kept = opened(&rig.ledger).get(&job("one")).cloned().unwrap();
    assert_eq!(kept.piece(Stream::Stdout, End::Head), Ok(b"out\n".to_vec()));
    assert_eq!(kept.piece(Stream::Stdout, End::Tail), Ok(Vec::new()));
    assert_eq!(kept.piece(Stream::Stderr, End::Head), Ok(Vec::new()));
}

/// A job asked for before the supervisor is ready waits for it, and what
/// the supervisor says may come in parts, or several things at once.
#[test]
fn a_job_waits_for_readiness_and_what_is_said_may_come_in_parts() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        let (_broker, answer) = ask_run(&rig.door, "early");
        assert!(unasked(&rig.room, SHORT), "asked before readiness");
        send(&rig.room, b"\0\0\0\x05rea");
        assert!(unasked(&rig.room, SHORT), "asked on part of readiness");
        send(&rig.room, b"dy");
        let Ask::Start { nonce, .. } = hears(&rig.room) else {
            unreachable!()
        };
        // Its start and its end, in one write cut in two.
        let mut both = Vec::new();
        for said in [
            Say::Started { nonce },
            Say::Ended {
                job: job("early"),
                how: Ended::Exited(4),
                stdout: Captured::default(),
                stderr: Captured::default(),
            },
        ] {
            let body = serde_json::to_vec(&said).unwrap();
            both.extend(u32::try_from(body.len()).unwrap().to_be_bytes());
            both.extend(body);
        }
        let (first, rest) = both.split_at(both.len() - 7);
        send(&rig.room, first);
        assert!(unanswered(&answer), "answered on part of an end");
        send(&rig.room, rest);
        assert_eq!(outcome(&answered(&answer).1).0, Ended::Exited(4));
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// Jobs run side by side, each ends as itself, and only so many run at
/// once: one more is refused, and has room once another has ended.
#[test]
fn only_so_many_jobs_run_at_once_and_each_ends_as_itself() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let names = ["a", "b", "c", "d"];
        assert_eq!(names.len(), RUNNING_MAX);
        let mut asked = Vec::new();
        for name in names {
            asked.push(ask_run(&rig.door, name));
            starts(&rig.room, name);
        }
        let (_broker, fifth) = ask_run(&rig.door, "e");
        assert_eq!(wait(&fifth), Err(String::from(FULL)));
        // The last to start is the first to end.
        for (code, name) in (1_u8..).zip(names.iter().rev()) {
            ends(&rig.room, name, Ended::Exited(code), name.as_bytes());
        }
        for ((_, answer), code) in asked.iter().zip([4_u8, 3, 2, 1]) {
            assert_eq!(outcome(&answered(answer).1).0, Ended::Exited(code));
        }
        let (_broker, fifth) = ask_run(&rig.door, "e");
        serves(&rig.room, "e", 0, b"");
        assert!(answered(&fifth).0);
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// A name the session knows starts nothing: whoever asks by it again is
/// told of the job that has it, while it runs and once it is over.
#[test]
fn a_name_that_is_known_is_never_run_again() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let (_first_broker, first) = ask_run(&rig.door, "once");
        starts(&rig.room, "once");
        let (_again_broker, again) = ask_run(&rig.door, "once");
        assert!(unasked(&rig.room, SHORT), "a known name was started again");
        ends(&rig.room, "once", Ended::Exited(9), b"");
        let (started, state) = answered(&first);
        assert!(started);
        let (restarted, same) = answered(&again);
        assert!(!restarted, "the second ask says it started the job");
        assert_eq!(same, state);
        // Over, it is answered from the ledger.
        let (_later_broker, later) = ask_run(&rig.door, "once");
        assert_eq!(answered(&later), (false, state));
        assert!(unasked(&rig.room, SHORT), "a job that was over was started");
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// A caller waits for its job only as long as it said: then it is told
/// the job is running, once the supervisor has said it started. The job
/// runs on, and whoever asks by its name is told how it came out.
#[test]
fn a_caller_waits_only_so_long_and_its_job_runs_on() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let impatient = Checked {
            wait: Duration::ZERO,
            ..ordinary()
        };
        let (_broker, answer) = ask_held(&rig.door, "slow", impatient);
        let Ask::Start { nonce, .. } = hears(&rig.room) else {
            unreachable!()
        };
        assert!(unanswered(&answer), "told it runs before it started");
        says(&rig.room, &Say::Started { nonce });
        let (started, state) = answered(&answer);
        assert!(started);
        assert!(matches!(state, RoomJobState::Running { .. }), "{state:?}");

        let (_now_broker, now) = ask_job(&rig.door, "slow", Duration::ZERO);
        assert!(matches!(
            answered(&now),
            (false, RoomJobState::Running { .. })
        ));
        let (_wait_broker, waiting) = ask_job(&rig.door, "slow", LONG);
        assert!(unanswered(&waiting), "a wait was answered while it runs");
        ends(&rig.room, "slow", Ended::Exited(0), b"done\n");
        let (started, state) = answered(&waiting);
        assert!(!started);
        assert_eq!(outcome(&state).2, short(b"done\n"));
        let (_after_broker, after) = ask_job(&rig.door, "slow", LONG);
        assert_eq!(answered(&after), (false, state));
        let (_none_broker, none) = ask_job(&rig.door, "never", LONG);
        assert_eq!(wait(&none), Err(String::from(NO_JOB)));
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// A job still running at its limit is stopped, and says so. The session
/// goes on: the next job runs in the same Room.
#[test]
fn a_job_past_its_limit_is_stopped_and_the_session_goes_on() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let brief = Checked {
            limit: SHORT,
            ..ordinary()
        };
        let asked = Instant::now();
        let (_broker, answer) = ask_held(&rig.door, "long", brief);
        starts(&rig.room, "long");
        let Ask::Stop {
            nonce,
            job: stopped,
        } = hears(&rig.room)
        else {
            unreachable!()
        };
        assert!(asked.elapsed() >= SHORT, "stopped before its limit");
        assert_eq!(stopped, job("long"));
        says(&rig.room, &Say::Stopping { nonce });
        assert!(unasked(&rig.room, SHORT), "stopped twice");
        ends(&rig.room, "long", Ended::Signaled(9), b"");
        let (how, why, _) = outcome(&answered(&answer).1);
        assert_eq!(
            (how, why),
            (Ended::Signaled(9), Some(RoomJobStopped::Limit))
        );

        let (_next_broker, next) = ask_run(&rig.door, "next");
        serves(&rig.room, "next", 0, b"");
        assert_eq!(outcome(&answered(&next).1).1, None);
        assert!(!held.is_finished(), "a limit ended the session");
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// A caller that waits as long as its job may run is told how the job
/// came out when it is stopped at its limit, not that it is running.
#[test]
fn a_caller_that_waits_out_the_limit_is_told_how_the_job_came_out() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let brief = Checked {
            limit: SHORT,
            wait: SHORT,
            ..ordinary()
        };
        let (_broker, answer) = ask_held(&rig.door, "long", brief);
        starts(&rig.room, "long");
        let Ask::Stop { nonce, .. } = hears(&rig.room) else {
            unreachable!()
        };
        // Its wait is over, and it is being stopped: nothing is said
        // until it has ended.
        let (_also_broker, also) = ask_job(&rig.door, "long", Duration::ZERO);
        assert!(unanswered(&answer), "told it runs as it was being stopped");
        assert!(unanswered(&also), "told it runs as it was being stopped");
        says(&rig.room, &Say::Stopping { nonce });
        ends(&rig.room, "long", Ended::Signaled(9), b"");
        let (started, state) = answered(&answer);
        assert!(started);
        assert_eq!(outcome(&state).1, Some(RoomJobStopped::Limit));
        assert_eq!(answered(&also), (false, state));
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// A stop that is asked for is asked of the supervisor once, and whoever
/// asked it and whoever ran the job are told how the job came out. A job
/// that is over is told as it is, and one that isn't known is said so.
#[test]
fn a_stop_is_asked_once_and_says_how_the_job_came_out() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let (_broker, ran) = ask_run(&rig.door, "stopped");
        starts(&rig.room, "stopped");
        let (_stop_broker, stop) = ask_stop(&rig.door, "stopped");
        let (_twice_broker, twice) = ask_stop(&rig.door, "stopped");
        let Ask::Stop { nonce, .. } = hears(&rig.room) else {
            unreachable!()
        };
        says(&rig.room, &Say::Stopping { nonce });
        assert!(unasked(&rig.room, SHORT), "stopped twice");
        ends(&rig.room, "stopped", Ended::Signaled(9), b"");
        let (started, state) = answered(&ran);
        assert!(started);
        assert_eq!(outcome(&state).1, Some(RoomJobStopped::Asked));
        assert_eq!(answered(&stop), (false, state.clone()));
        assert_eq!(answered(&twice), (false, state.clone()));
        let (_over_broker, over) = ask_stop(&rig.door, "stopped");
        assert_eq!(answered(&over), (false, state));
        assert!(unasked(&rig.room, SHORT), "a job that was over was stopped");
        let (_none_broker, none) = ask_stop(&rig.door, "never");
        assert_eq!(wait(&none), Err(String::from(NO_JOB)));

        // A stop that crosses its job's end: the supervisor has no such
        // job by then, and that is no fault.
        let (_cross_broker, crossed) = ask_run(&rig.door, "crossed");
        starts(&rig.room, "crossed");
        let (_cross_stop_broker, cross_stop) = ask_stop(&rig.door, "crossed");
        let Ask::Stop { nonce, .. } = hears(&rig.room) else {
            unreachable!()
        };
        ends(&rig.room, "crossed", Ended::Exited(0), b"");
        says(&rig.room, &Say::NoJob { nonce });
        assert_eq!(outcome(&answered(&crossed).1).0, Ended::Exited(0));
        assert!(wait(&cross_stop).is_ok());
        assert!(!held.is_finished(), "a crossed stop ended the session");
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// A job that doesn't end within its grace after it was stopped ends the
/// session: it is lost with its Room, and is said to be, once the Room is
/// down.
#[test]
fn a_job_that_does_not_end_when_stopped_ends_the_session() {
    let (rig, mut kept) = new_rig(Limits {
        stop: SHORT,
        ..limits()
    });
    let (_broker, ran) = ask_run(&rig.door, "stuck");
    let (stop, stopped_at) = std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| kept.hold(&rig.supervisor, rig.launcher.as_fd()));
        say_ready(&rig.room);
        starts(&rig.room, "stuck");
        let (_stop_broker, stop) = ask_stop(&rig.door, "stuck");
        let Ask::Stop { nonce, .. } = hears(&rig.room) else {
            unreachable!()
        };
        let stopped_at = Instant::now();
        says(&rig.room, &Say::Stopping { nonce });
        seen_out(held, &rig.room);
        (stop, stopped_at)
    });
    assert!(stopped_at.elapsed() >= SHORT, "its grace wasn't waited out");
    assert!(ran.try_recv().is_err(), "answered before the Room was down");
    kept.finish(Ok(()), |_| unreachable!());
    let lost = RoomJobState::Lost {
        why: String::from(UNSTOPPED),
    };
    assert_eq!(answered(&ran), (true, lost.clone()));
    assert_eq!(answered(&stop), (false, lost));
    let kept = opened(&rig.ledger).get(&job("stuck")).cloned().unwrap();
    assert_eq!(
        kept.piece(Stream::Stdout, End::Head),
        Err(String::from(LOST))
    );
}

/// A session with nothing in hand ends at its idle limit, counted from
/// when it last had something.
#[test]
fn an_idle_session_ends_at_its_idle_limit_counted_from_its_last_job() {
    let idle = Limits {
        idle: SHORT * 2,
        ..limits()
    };
    let started = Instant::now();
    let said = ended_with(idle, false, |far| say_ready(far.room()));
    assert_eq!(said, IDLE);
    assert!(started.elapsed() >= SHORT * 2);

    let mut finished = None;
    let said = ended_with(idle, true, |far| {
        say_ready(far.room());
        starts(far.room(), "first");
        std::thread::sleep(SHORT * 3);
        ends(far.room(), "first", Ended::Exited(0), b"");
        finished = Some(Instant::now());
    });
    assert_eq!(said, IDLE, "a session with a job in hand isn't idle");
    assert!(
        finished.unwrap().elapsed() >= SHORT * 2,
        "idleness was counted from before its last job ended"
    );
}

/// The time limit holds whatever the session is doing, and a job running
/// then is lost with it.
#[test]
fn a_session_ends_at_its_time_limit_whatever_it_is_doing() {
    let capped = Limits {
        cap: SHORT * 2,
        ..limits()
    };
    let said = ended_with(capped, true, |far| {
        say_ready(far.room());
        starts(far.room(), "first");
    });
    assert_eq!(said, CAPPED);

    let (rig, mut kept) = new_rig(capped);
    let (_broker, ran) = ask_run(&rig.door, "cut");
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| kept.hold(&rig.supervisor, rig.launcher.as_fd()));
        say_ready(&rig.room);
        starts(&rig.room, "cut");
        seen_out(held, &rig.room);
    });
    kept.finish(Ok(()), |_| unreachable!());
    let lost = RoomJobState::Lost {
        why: String::from(CAPPED),
    };
    assert_eq!(answered(&ran), (true, lost));
}

/// A supervisor that is never ready, or stops answering, ends the
/// session.
#[test]
fn a_supervisor_that_goes_quiet_ends_the_session() {
    let unready = Limits {
        ready: SHORT,
        ..limits()
    };
    assert_eq!(ended_with(unready, true, |_| {}), UNREADY);
    let unanswering = Limits {
        answer: SHORT,
        ..limits()
    };
    let said = ended_with(unanswering, true, |far| {
        say_ready(far.room());
        let _unanswered = hears(far.room());
    });
    assert_eq!(said, UNANSWERED);
}

/// A job whose limit passes before the Room could start it is let go:
/// its caller hears why, nothing is asked of the supervisor for it, and
/// its name is free.
#[test]
fn a_job_whose_limit_passes_before_it_is_sent_is_let_go() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        let brief = Checked {
            limit: SHORT,
            ..ordinary()
        };
        let (_broker, answer) = ask_held(&rig.door, "late", brief);
        let (_other_broker, other) = ask_run(&rig.door, "other");
        assert_eq!(wait(&answer), Err(String::from(EXPIRED)));
        say_ready(&rig.room);
        // What else was waiting to be asked still is.
        serves(&rig.room, "other", 0, b"");
        assert!(answered(&other).0);
        assert!(unasked(&rig.room, SHORT), "a job let go was started");
        let (_again_broker, again) = ask_run(&rig.door, "late");
        serves(&rig.room, "late", 0, b"");
        assert!(answered(&again).0, "its name wasn't free");
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
    assert!(opened(&rig.ledger).get(&job("late")).is_some());
}

/// A job the supervisor says it didn't start is let go with why, and its
/// name is free.
#[test]
fn a_job_the_supervisor_does_not_start_is_let_go_with_why() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let (_broker, answer) = ask_run(&rig.door, "absent");
        let (_also_broker, also) = ask_job(&rig.door, "absent", LONG);
        let Ask::Start { nonce, .. } = hears(&rig.room) else {
            unreachable!()
        };
        let why = NotStarted::NoSuchProgram;
        says(&rig.room, &Say::NotStarted { nonce, why });
        let said = String::from(not_started(why));
        assert_eq!(wait(&answer), Err(said.clone()));
        assert_eq!(wait(&also), Err(said));
        let (_again_broker, again) = ask_run(&rig.door, "absent");
        serves(&rig.room, "absent", 0, b"");
        assert!(answered(&again).0, "its name wasn't free");
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// A caller that goes, or says more, is let go. Its job, once the
/// supervisor has it, runs on and is kept; one the supervisor never had
/// is let go with its caller.
#[test]
fn a_caller_that_goes_leaves_its_started_job_running() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        // Asked before readiness, and gone before it: never started.
        let (broker, _answer) = ask_run(&rig.door, "unsent");
        gone(&broker);
        std::thread::sleep(SHORT);
        say_ready(&rig.room);
        assert!(unasked(&rig.room, SHORT), "started for a caller that went");

        let (broker, _answer) = ask_run(&rig.door, "sent");
        starts(&rig.room, "sent");
        gone(&broker);
        let (mut talker, _talking) = ask_job(&rig.door, "sent", LONG);
        talker.write_all(b"more").unwrap();
        std::thread::sleep(SHORT);
        assert!(!held.is_finished(), "a caller that went ended the session");
        ends(&rig.room, "sent", Ended::Exited(5), b"");
        let (_later_broker, later) = ask_job(&rig.door, "sent", LONG);
        assert_eq!(outcome(&answered(&later).1).0, Ended::Exited(5));
        let (_none_broker, none) = ask_job(&rig.door, "unsent", LONG);
        assert_eq!(wait(&none), Err(String::from(NO_JOB)));
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// What ends a session from outside it: its owner going, the Room's first
/// process ending, and the supervisor closing, before or after readiness.
#[test]
fn a_session_ends_with_its_owner_its_room_or_its_supervisor() {
    assert_eq!(
        ended_by(true, |far| {
            say_ready(far.room());
            starts(far.room(), "first");
            far.alive = None;
        }),
        LAUNCHER_ENDED
    );
    assert_eq!(
        ended_by(false, |far| {
            let room = far.room.take().unwrap();
            gone(&room);
        }),
        CLOSED_EARLY
    );
    assert_eq!(
        ended_by(true, |far| {
            say_ready(far.room());
            starts(far.room(), "first");
            let room = far.room.take().unwrap();
            gone(&room);
        }),
        CLOSED
    );
    assert_eq!(
        ended_by(false, |far| send(far.room(), b"\0\0\0\x05readx")),
        BAD_READY
    );
    assert!(
        ended_by(false, |far| send(far.room(), b"\0\0\0\x06ready!")).contains("oversized"),
        "readiness is exactly its five bytes"
    );
}

/// The supervisor may answer only what it was asked, and say of a job
/// only that it ended, once, after saying it started. Anything else ends
/// the session.
#[test]
fn a_supervisor_out_of_step_ends_the_session() {
    let ended = |name: &str| Say::Ended {
        job: job(name),
        how: Ended::Exited(0),
        stdout: Captured::default(),
        stderr: Captured::default(),
    };
    let nonce = || "0".repeat(64);
    // Something that can't be read, and something too long to be one.
    assert_eq!(
        ended_by(false, |far| {
            say_ready(far.room());
            write_frame(far.room(), b"{\"say\":\"done\"}").unwrap();
        }),
        BAD_SAY
    );
    assert!(
        ended_by(false, |far| {
            say_ready(far.room());
            send(far.room(), &u32::MAX.to_be_bytes());
        })
        .contains("oversized")
    );
    // An answer when nothing was asked, and one to something else.
    assert_eq!(
        ended_by(false, |far| {
            say_ready(far.room());
            says(far.room(), &Say::Started { nonce: nonce() });
        }),
        OUT_OF_TURN
    );
    assert_eq!(
        ended_by(true, |far| {
            say_ready(far.room());
            let _asked = hears(far.room());
            says(far.room(), &Say::Started { nonce: nonce() });
        }),
        OUT_OF_TURN
    );
    // The wrong answer to what was asked.
    assert_eq!(
        ended_by(true, |far| {
            say_ready(far.room());
            let asked = hears(far.room());
            let nonce = asked.nonce().to_owned();
            says(far.room(), &Say::Stopping { nonce });
        }),
        OUT_OF_TURN
    );
    // An end of a job it was never given, of one it hasn't said started,
    // and of one that ended already.
    assert_eq!(
        ended_by(false, |far| {
            say_ready(far.room());
            says(far.room(), &ended("never"));
        }),
        OUT_OF_TURN
    );
    assert_eq!(
        ended_by(true, |far| {
            say_ready(far.room());
            let _asked = hears(far.room());
            says(far.room(), &ended("first"));
        }),
        OUT_OF_TURN
    );
    assert_eq!(
        ended_by(true, |far| {
            say_ready(far.room());
            starts(far.room(), "first");
            says(far.room(), &ended("first"));
            says(far.room(), &ended("first"));
        }),
        OUT_OF_TURN
    );
    // And of one whose end was said, while what it wrote is being fetched.
    assert_eq!(
        ended_by(true, |far| {
            say_ready(far.room());
            starts(far.room(), "first");
            let loud = Say::Ended {
                job: job("first"),
                how: Ended::Exited(0),
                stdout: short(b"four"),
                stderr: Captured::default(),
            };
            says(far.room(), &loud);
            says(far.room(), &loud);
        }),
        OUT_OF_TURN
    );
}

/// What the supervisor says of a job's end is checked before it is an
/// answer: an end no process can have, counts that can't be true
/// together, a piece that isn't the length it said, or no piece at all,
/// end the session.
#[test]
fn a_jobs_end_is_checked_before_it_is_an_answer() {
    let with = |how: Ended, stdout: Captured| {
        ended_by(true, move |far| {
            say_ready(far.room());
            starts(far.room(), "first");
            says(
                far.room(),
                &Say::Ended {
                    job: job("first"),
                    how,
                    stdout,
                    stderr: Captured::default(),
                },
            );
        })
    };
    assert_eq!(with(Ended::Signaled(0), Captured::default()), BAD_END);
    let tail_alone = Captured {
        tail: 1,
        spilled: 1,
        ..Captured::default()
    };
    assert_eq!(with(Ended::Exited(0), tail_alone), BAD_END);
    let stderr_wrong = ended_by(true, |far| {
        say_ready(far.room());
        starts(far.room(), "first");
        says(
            far.room(),
            &Say::Ended {
                job: job("first"),
                how: Ended::Exited(0),
                stdout: Captured::default(),
                stderr: tail_alone,
            },
        );
    });
    assert_eq!(stderr_wrong, BAD_END);

    let piece = |answer: fn(String) -> Say| {
        ended_by(true, move |far| {
            say_ready(far.room());
            starts(far.room(), "first");
            says(
                far.room(),
                &Say::Ended {
                    job: job("first"),
                    how: Ended::Exited(0),
                    stdout: short(b"four"),
                    stderr: Captured::default(),
                },
            );
            let asked = hears(far.room());
            says(far.room(), &answer(asked.nonce().to_owned()));
        })
    };
    let longer = |nonce| Say::Piece {
        nonce,
        bytes: b"fives".to_vec(),
    };
    let shorter = |nonce| Say::Piece {
        nonce,
        bytes: b"thr".to_vec(),
    };
    assert_eq!(piece(longer), BAD_PIECE);
    assert_eq!(piece(shorter), BAD_PIECE);
    assert_eq!(piece(|nonce| Say::NoJob { nonce }), OUT_OF_TURN);
}

/// Both pieces of both streams are fetched, each where it belongs.
#[test]
fn every_kept_piece_of_a_jobs_output_is_fetched() {
    let full = u32::try_from(hatchling_core::room::PIECE_BYTES).unwrap();
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let (_broker, answer) = ask_run(&rig.door, "loud");
        starts(&rig.room, "loud");
        let long = Captured {
            head: full,
            tail: 3,
            dropped: 0,
            spilled: u64::from(full) + 3,
            spill_cut: false,
        };
        says(
            &rig.room,
            &Say::Ended {
                job: job("loud"),
                how: Ended::Exited(0),
                stdout: long,
                stderr: short(b"err"),
            },
        );
        let mut also = None;
        for (stream, end, fill) in [
            (
                Stream::Stdout,
                End::Head,
                vec![b'h'; hatchling_core::room::PIECE_BYTES],
            ),
            (Stream::Stdout, End::Tail, b"end".to_vec()),
            (Stream::Stderr, End::Head, b"err".to_vec()),
        ] {
            assert!(unanswered(&answer), "answered before its output was whole");
            let Ask::Piece {
                nonce,
                stream: asked,
                end: which,
                ..
            } = hears(&rig.room)
            else {
                unreachable!()
            };
            assert_eq!((asked, which), (stream, end));
            // Its end has been heard: a wait that runs out from here on
            // is told how it came out, once its output is whole.
            let (_, late) = also.get_or_insert_with(|| ask_job(&rig.door, "loud", Duration::ZERO));
            assert!(unanswered(late), "told it runs when it had ended");
            says(&rig.room, &Say::Piece { nonce, bytes: fill });
        }
        let (_, _, stdout) = outcome(&answered(&answer).1);
        assert_eq!(stdout, long);
        let (_, late) = also.unwrap();
        assert_eq!(outcome(&answered(&late).1).2, long);
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
    let kept = opened(&rig.ledger).get(&job("loud")).cloned().unwrap();
    assert_eq!(
        kept.piece(Stream::Stdout, End::Head).unwrap().len(),
        hatchling_core::room::PIECE_BYTES
    );
    assert_eq!(kept.piece(Stream::Stdout, End::Tail), Ok(b"end".to_vec()));
    assert_eq!(kept.piece(Stream::Stderr, End::Head), Ok(b"err".to_vec()));
    assert_eq!(kept.piece(Stream::Stderr, End::Tail), Ok(Vec::new()));
}

/// A live session answers a check that the Room works, once its
/// supervisor is ready, and not before.
#[test]
fn a_live_session_says_the_room_works_once_it_is_ready() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        let early = ask_probe(&rig.door);
        assert!(unanswered(&early), "an unready Room was said to work");
        say_ready(&rig.room);
        assert_eq!(wait(&early), Ok(()));
        assert_eq!(wait(&ask_probe(&rig.door)), Ok(()));
        assert!(!held.is_finished(), "a check ended the session");
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// The disk under a session: what leaves it open is done by the session,
/// in its own hold on the disk, even while a job runs. A close or a wipe
/// ends the session, and is done only once the Room is down; the job that
/// was running is lost with it.
#[test]
fn closing_or_wiping_the_disk_ends_the_session_first() {
    for closing in [RoomDiskOp::Close, RoomDiskOp::Wipe] {
        let (rig, mut kept) = new_rig(limits());
        let (_broker, answer) = ask_run(&rig.door, "running");
        let closed = std::thread::scope(|scope| {
            let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
            let held = scope.spawn(|| kept.hold(&rig.supervisor, rig.launcher.as_fd()));
            say_ready(&rig.room);
            starts(&rig.room, "running");
            for op in [RoomDiskOp::Status, RoomDiskOp::Open, RoomDiskOp::Create] {
                assert_eq!(wait(&ask_disk(&rig.door, op)), Ok(None));
            }
            assert!(!held.is_finished(), "a status ended the session");
            let closed = ask_disk(&rig.door, closing);
            held.join().unwrap();
            closed
        });
        assert_eq!(
            *rig.ops.lock().unwrap(),
            [RoomDiskOp::Status, RoomDiskOp::Open, RoomDiskOp::Create],
            "the session did more to the disk it holds than it may"
        );
        assert!(
            answer.try_recv().is_err(),
            "answered before the Room was down"
        );
        assert!(closed.try_recv().is_err(), "closed under a live Room");
        let mut direct = Vec::new();
        kept.finish(Ok(()), |op| {
            direct.push(op);
            Err(String::from("done directly"))
        });
        assert_eq!(direct, [closing]);
        assert_eq!(wait(&closed), Err(String::from("done directly")));
        let lost = RoomJobState::Lost {
            why: String::from(DISK_CLOSING),
        };
        assert_eq!(answered(&answer), (true, lost));
    }
}

/// A session that ends before the supervisor was given a job says why to
/// whoever asked for it, and its name is free: nothing of it is lost,
/// because nothing of it ran.
#[test]
fn a_job_never_given_to_the_supervisor_is_not_lost() {
    let (rig, mut kept) = new_rig(limits());
    let (_broker, answer) = ask_run(&rig.door, "ungiven");
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| kept.hold(&rig.supervisor, rig.launcher.as_fd()));
        std::thread::sleep(SHORT);
        assert!(rig.door.send(Message::End { reason: "gone" }).is_ok());
        held.join().unwrap();
    });
    kept.finish(Ok(()), |_| unreachable!());
    assert_eq!(wait(&answer), Err(String::from("gone")));
    assert!(opened(&rig.ledger).get(&job("ungiven")).is_none());
}

/// Before anything is launched: a job whose caller has already gone
/// launches nothing, and hears why the launch didn't happen; so does a
/// session whose disk is being closed.
#[test]
fn nothing_is_launched_for_a_caller_that_has_gone() {
    let (rig, mut kept) = new_rig(limits());
    assert_eq!(kept.begin(), Ok(()), "nothing asked is nothing against it");
    let (_broker, answer) = ask_run(&rig.door, "wanted");
    assert_eq!(kept.begin(), Ok(()));
    kept.finish(Err(String::from("no launch")), |_| unreachable!());
    assert_eq!(wait(&answer), Err(String::from("no launch")));

    let (rig, mut kept) = new_rig(limits());
    let (broker, answer) = ask_run(&rig.door, "unwanted");
    let (_staying, stayed) = ask_run(&rig.door, "wanted");
    gone(&broker);
    drop(broker);
    assert_eq!(kept.begin(), Ok(()), "one caller is still there");
    assert_eq!(wait(&answer), Err(String::from(REQUESTER_GONE)));
    assert!(kept.flights.contains_key(&job("wanted")));
    assert!(!kept.flights.contains_key(&job("unwanted")));
    kept.finish(Err(String::from("no launch")), |_| unreachable!());
    assert_eq!(wait(&stayed), Err(String::from("no launch")));

    let (rig, mut kept) = new_rig(limits());
    let (broker, answer) = ask_run(&rig.door, "unwanted");
    gone(&broker);
    drop(broker);
    assert_eq!(kept.begin(), Err(String::from(REQUESTER_GONE)));
    kept.finish(Err(String::from(REQUESTER_GONE)), |_| unreachable!());
    assert_eq!(wait(&answer), Err(String::from(REQUESTER_GONE)));

    let (rig, mut kept) = new_rig(limits());
    let closed = ask_disk(&rig.door, RoomDiskOp::Close);
    assert_eq!(kept.begin(), Err(String::from(DISK_CLOSING)));
    kept.finish(Err(String::from(DISK_CLOSING)), |op| {
        assert_eq!(op, RoomDiskOp::Close);
        Ok(None)
    });
    assert_eq!(wait(&closed), Ok(None));
}

/// A launch that fails before the session took anything in still answers
/// the job it was started for, and any other waiting for the same Room,
/// with why there is none. A check that was waiting is let go unanswered,
/// to be made again without a session.
#[test]
fn a_launch_that_fails_at_once_answers_every_waiting_job() {
    let (rig, kept) = new_rig(limits());
    let (_first_broker, first) = ask_run(&rig.door, "first");
    let (_second_broker, second) = ask_run(&rig.door, "second");
    let (_page_broker, read) = ask_page(&rig.door, "/workspace/a", 1, 1);
    let check = ask_probe(&rig.door);
    kept.finish(Err(String::from("no disk here")), |_| unreachable!());
    assert_eq!(wait(&first), Err(String::from("no disk here")));
    assert_eq!(wait(&second), Err(String::from("no disk here")));
    assert_eq!(wait(&read), Err(String::from("no disk here")));
    assert!(
        check.recv_timeout(LONG).is_err(),
        "a check was answered by a Room that never launched"
    );
    assert!(opened(&rig.ledger).over.is_empty(), "a job was kept");
}

/// A Room that comes down at the first try is left at once, and nothing
/// it owes is refused.
#[test]
fn a_session_that_comes_down_is_left_at_once() {
    let (_rig, mut kept) = new_rig(limits());
    let (reply, answer) = mpsc::channel();
    kept.owed
        .push(Owed::Job(reply, Err(String::from("why it ended"))));
    let (reply, read) = mpsc::channel();
    kept.owed
        .push(Owed::Page(reply, String::from("why it ended")));
    let mut tries = 0;
    kept.settle(&mut || {
        tries += 1;
        Ok(())
    });
    assert_eq!(tries, 1);
    kept.finish(Ok(()), |_| unreachable!());
    assert_eq!(wait(&answer), Err(String::from("why it ended")));
    assert_eq!(wait(&read), Err(String::from("why it ended")));
}

/// What a held session says to whoever asks of it, for a cleanup that
/// failed with "stuck".
const HELD: &str = "the last Room session isn't cleared yet: stuck";

/// A cleanup that fails for as long as it is stuck, and counts its tries.
/// It can't fail more than `most` times, and comes unstuck when a test
/// ends early, so a session that never gives up still ends.
struct Cleanup {
    stuck: AtomicBool,
    tries: AtomicUsize,
    most: usize,
}

impl Cleanup {
    const fn stuck(most: usize) -> Self {
        Self {
            stuck: AtomicBool::new(true),
            tries: AtomicUsize::new(0),
            most,
        }
    }

    fn down(&self) -> Result<(), String> {
        let tries = self.tries.fetch_add(1, Atomic::SeqCst) + 1;
        if self.stuck.load(Atomic::SeqCst) && tries <= self.most {
            Err(String::from("stuck"))
        } else {
            Ok(())
        }
    }

    fn tries(&self) -> usize {
        self.tries.load(Atomic::SeqCst)
    }

    fn clear(&self) {
        self.stuck.store(false, Atomic::SeqCst);
    }
}

struct ClearedAtEnd<'a>(&'a Cleanup);

impl Drop for ClearedAtEnd<'_> {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// A session whose Room can't be taken down stays in charge: it tries
/// again on its own, refuses jobs, stops and checks with the reason,
/// still answers for the disk it holds, and is left once the Room is
/// down.
#[test]
fn a_session_that_cannot_be_cleared_stays_held_and_says_so() {
    let (rig, mut kept) = new_rig(Limits {
        retry: Duration::from_millis(100),
        ..limits()
    });
    let (reply, owed) = mpsc::channel();
    kept.owed
        .push(Owed::Job(reply, Err(String::from("why it ended"))));
    let (reply, unread) = mpsc::channel();
    kept.owed
        .push(Owed::Page(reply, String::from("why it ended")));
    let cleanup = Cleanup::stuck(1000);
    std::thread::scope(|scope| {
        let settled = scope.spawn(|| kept.settle(&mut || cleanup.down()));
        let _cleared = ClearedAtEnd(&cleanup);
        assert_eq!(
            wait(&owed),
            Err(String::from(HELD)),
            "a failed cleanup is what the caller hears"
        );
        assert_eq!(wait(&unread), Err(String::from(HELD)));
        let (_page_broker, read) = ask_page(&rig.door, "/workspace/a", 1, 1);
        assert_eq!(wait(&read), Err(String::from(HELD)));
        let (_broker, answer) = ask_run(&rig.door, "refused");
        assert_eq!(wait(&answer), Err(String::from(HELD)));
        let (_job_broker, asked) = ask_job(&rig.door, "refused", LONG);
        assert_eq!(wait(&asked), Err(String::from(HELD)));
        let (_stop_broker, stopped) = ask_stop(&rig.door, "refused");
        assert_eq!(wait(&stopped), Err(String::from(HELD)));
        assert_eq!(wait(&ask_probe(&rig.door)), Err(String::from(HELD)));
        assert_eq!(wait(&ask_disk(&rig.door, RoomDiskOp::Status)), Ok(None));
        let before = cleanup.tries();
        std::thread::sleep(Duration::from_millis(450));
        assert!(
            cleanup.tries() >= before + 2,
            "a held session stopped trying to clear itself"
        );
        assert!(!settled.is_finished(), "a held session was let go");
        cleanup.clear();
        settled.join().unwrap();
    });
    assert_eq!(*rig.ops.lock().unwrap(), [RoomDiskOp::Status]);
}

/// A close or a wipe asked of a held session is a reason to try again at
/// once: refused with the reason while the Room still can't be cleared,
/// and done once it can.
#[test]
fn a_close_makes_a_held_session_try_again_at_once() {
    let (rig, mut kept) = new_rig(limits());
    let cleanup = Cleanup::stuck(50);
    let closed = std::thread::scope(|scope| {
        let settled = scope.spawn(|| kept.settle(&mut || cleanup.down()));
        let _cleared = ClearedAtEnd(&cleanup);
        let refused = ask_disk(&rig.door, RoomDiskOp::Close);
        assert_eq!(wait(&refused), Err(String::from(HELD)));
        assert_eq!(cleanup.tries(), 2, "the close didn't make it try again");
        cleanup.clear();
        let closed = ask_disk(&rig.door, RoomDiskOp::Wipe);
        settled.join().unwrap();
        closed
    });
    assert_eq!(cleanup.tries(), 3);
    assert!(
        closed.try_recv().is_err(),
        "wiped before the claim was let go"
    );
    kept.finish(Ok(()), |op| {
        assert_eq!(op, RoomDiskOp::Wipe);
        Ok(None)
    });
    assert_eq!(wait(&closed), Ok(None));
}

/// When the broker has gone nothing waits on a cleanup that fails: the
/// session tries once more and is left, so the spawner can leave too.
#[test]
fn a_held_session_is_left_when_the_broker_has_gone() {
    for owner_dropped in [false, true] {
        let (rig, mut kept) = new_rig(limits());
        let cleanup = Cleanup::stuck(3);
        let started = Instant::now();
        std::thread::scope(|scope| {
            let settled = scope.spawn(|| kept.settle(&mut || cleanup.down()));
            while cleanup.tries() == 0 {
                assert!(started.elapsed() < LONG, "the session never tried");
                std::thread::yield_now();
            }
            if owner_dropped {
                drop(rig.door);
            } else {
                assert!(rig.door.send(Message::End { reason: "gone" }).is_ok());
            }
            settled.join().unwrap();
        });
        assert_eq!(cleanup.tries(), 2);
        assert!(started.elapsed() < LONG / 2, "waited out the retry");
    }

    let (_rig, mut kept) = new_rig(limits());
    kept.left = true;
    let cleanup = Cleanup::stuck(3);
    kept.settle(&mut || cleanup.down());
    assert_eq!(
        cleanup.tries(),
        1,
        "a session the broker left waited to try again"
    );
}

/// What a stand-in session answers for the job `name`: that it runs, and
/// has for as many milliseconds as the stand-in's number.
pub(crate) fn stood_in(generation: usize, name: &str) -> RoomJobAnswer {
    let ran_ms = u64::try_from(generation).unwrap();
    told(&job(name), RoomJobState::Running { ran_ms }, true)
}

/// The page a stand-in session hands back: what it was asked for, and
/// the stand-in's number.
pub(crate) fn stood_page(generation: usize, path: &str, from: u64, lines: u32) -> Page {
    Page {
        bytes: format!("session {generation}: {lines} lines of {path} from {from}").into_bytes(),
        ..Page::default()
    }
}

/// A stand-in for the thread that holds a session: it answers with its
/// own number, and ends when told to or on the jobs that say so.
fn stand_in(generation: usize, seat: &Seat) {
    while let Ok(message) = seat.inbox.recv() {
        match message {
            Message::Run { run, reply, .. } => match run.job.as_str() {
                "vanish" => return,
                "once" => {
                    let _ = reply.send(Ok(stood_in(generation, "once")));
                    return;
                }
                name => {
                    let _ = reply.send(Ok(stood_in(generation, name)));
                }
            },
            Message::Job {
                job, wait, reply, ..
            } => {
                let said = format!("session {generation} asked of {job} for {wait:?}");
                let _ = reply.send(Err(said));
            }
            Message::Stop { job, reply, .. } => {
                let _ = reply.send(Err(format!("session {generation} stopped {job}")));
            }
            Message::Page {
                path,
                from,
                lines,
                reply,
                ..
            } => {
                if path == "/vanish" {
                    return;
                }
                let _ = reply.send(Ok(stood_page(generation, &path, from, lines)));
            }
            Message::Probe { reply } => {
                let _ = reply.send(Ok(()));
            }
            Message::Disk { op, reply } => {
                let _ = reply.send(Err(format!("session {generation} {op:?}")));
            }
            Message::End { .. } => return,
        }
    }
}

/// Sessions held by stand-ins: how many were started, and how many ended.
/// A stand-in takes a moment to end after it stops listening, so a caller
/// that doesn't wait for it is caught not waiting.
pub(crate) fn stand_ins() -> (Sessions, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let started = Arc::new(AtomicUsize::new(0));
    let ended = Arc::new(AtomicUsize::new(0));
    let (count, done) = (Arc::clone(&started), Arc::clone(&ended));
    let sessions = Sessions::new(Arc::default(), move |seat| {
        let generation = count.fetch_add(1, Atomic::SeqCst) + 1;
        let done = Arc::clone(&done);
        std::thread::Builder::new().spawn(move || {
            stand_in(generation, &seat);
            // As the real thread does: its end of the channel goes first,
            // and only then is it on its way out.
            drop(seat);
            std::thread::sleep(Duration::from_millis(200));
            done.fetch_add(1, Atomic::SeqCst);
        })
    });
    (sessions, started, ended)
}

fn run(sessions: &Sessions, name: &str) -> JobReply {
    let (_broker, requester) = UnixStream::pair().unwrap();
    sessions.run(&a_run(name, &["true"]), &requester)
}

#[test]
fn the_first_job_starts_a_session_and_the_next_uses_it() {
    let (sessions, started, ended) = stand_ins();
    assert_eq!(run(&sessions, "a"), Ok(stood_in(1, "a")));
    assert_eq!(run(&sessions, "b"), Ok(stood_in(1, "b")));
    assert_eq!(started.load(Atomic::SeqCst), 1);
    assert_eq!(ended.load(Atomic::SeqCst), 0);
}

#[test]
fn a_session_that_ended_is_replaced_by_the_next_job() {
    let (sessions, started, _ended) = stand_ins();
    assert_eq!(run(&sessions, "once"), Ok(stood_in(1, "once")));
    assert_eq!(run(&sessions, "a"), Ok(stood_in(2, "a")));
    assert_eq!(run(&sessions, "b"), Ok(stood_in(2, "b")));
    assert_eq!(started.load(Atomic::SeqCst), 2);
}

#[test]
fn a_session_that_keeps_ending_unanswered_is_given_up_on() {
    let (sessions, started, _ended) = stand_ins();
    assert_eq!(run(&sessions, "vanish"), Err(String::from(UNSETTLED)));
    let tried = started.load(Atomic::SeqCst);
    assert!((2..=ATTEMPTS).contains(&tried), "started {tried} sessions");
    let (sessions, started, _ended) = stand_ins();
    assert_eq!(run(&sessions, "a"), Ok(stood_in(1, "a")));
    assert_eq!(
        sessions.probe(|| unreachable!("a live session answers")),
        Ok(())
    );
    assert_eq!(started.load(Atomic::SeqCst), 1);
}

/// A job that may not be asked of the Room starts no session, and a name
/// in the ledger is answered from it: no session is started or asked.
#[test]
fn a_run_is_checked_and_a_known_name_answered_before_any_session() {
    let (sessions, started, _ended) = stand_ins();
    let (_broker, requester) = UnixStream::pair().unwrap();
    let said = sessions
        .run(&a_run("j", &["./script"]), &requester)
        .unwrap_err();
    assert!(said.contains("interpreter"), "{said}");
    opened(&sessions.ledger).put(job("over"), Over::lost("then"));
    let lost = RoomJobState::Lost {
        why: String::from("then"),
    };
    let known = Ok(told(&job("over"), lost, false));
    assert_eq!(run(&sessions, "over"), known);
    assert_eq!(
        sessions.job(&job("over"), Duration::ZERO, &requester),
        known
    );
    assert_eq!(sessions.stop(&job("over"), &requester), known);
    assert_eq!(
        sessions.output(&job("over"), Stream::Stdout, End::Head),
        Err(String::from(LOST))
    );
    assert_eq!(started.load(Atomic::SeqCst), 0, "a session was started");
}

/// Asking of a job, stopping one and reading one's output start no
/// session: with none, a name that isn't known is said not to be. With
/// one, it is asked.
#[test]
fn asking_of_a_job_starts_no_session() {
    let (sessions, started, _ended) = stand_ins();
    let (_broker, requester) = UnixStream::pair().unwrap();
    let none = Err(String::from(NO_JOB));
    assert_eq!(sessions.job(&job("j"), LONG, &requester), none);
    assert_eq!(sessions.stop(&job("j"), &requester), none);
    assert_eq!(
        sessions.output(&job("j"), Stream::Stdout, End::Head),
        Err(String::from(NO_JOB))
    );
    assert_eq!(started.load(Atomic::SeqCst), 0, "a session was started");

    assert_eq!(run(&sessions, "a"), Ok(stood_in(1, "a")));
    assert_eq!(
        sessions.job(&job("j"), LONG, &requester),
        Err(String::from("session 1 asked of j for 10s"))
    );
    assert_eq!(
        sessions.stop(&job("j"), &requester),
        Err(String::from("session 1 stopped j"))
    );
    assert_eq!(
        sessions.output(&job("j"), Stream::Stdout, End::Head),
        Err(String::from("session 1 asked of j for 0ns"))
    );
    assert_eq!(started.load(Atomic::SeqCst), 1);
}

/// The output of a job is read from the ledger once it is over; of one
/// that a session still has running, it is said not to be over.
#[test]
fn output_is_read_once_a_job_is_over() {
    let ledger: Arc<Mutex<Ledger>> = Arc::default();
    let sessions = Sessions::new(Arc::clone(&ledger), |seat: Seat| {
        std::thread::Builder::new().spawn(move || {
            while let Ok(message) = seat.inbox.recv() {
                let (name, reply) = match message {
                    Message::Run { run, reply, .. } => (run.job, reply),
                    Message::Job { job, reply, .. } | Message::Stop { job, reply, .. } => {
                        (job, reply)
                    }
                    Message::Page { .. }
                    | Message::Probe { .. }
                    | Message::Disk { .. }
                    | Message::End { .. } => continue,
                };
                let state = RoomJobState::Running { ran_ms: 1 };
                let _ = reply.send(Ok(told(&name, state, false)));
            }
        })
    });
    assert!(matches!(
        run(&sessions, "running"),
        Ok(RoomJobAnswer { .. })
    ));
    assert_eq!(
        sessions.output(&job("running"), Stream::Stdout, End::Head),
        Err(String::from(NOT_OVER))
    );
    let over = Over {
        state: RoomJobState::Ended {
            how: Ended::Exited(0),
            stopped: None,
            ran_ms: 3,
            stdout: short(b"kept"),
            stderr: Captured::default(),
        },
        pieces: [[b"kept".to_vec(), Vec::new()], Default::default()],
    };
    opened(&ledger).put(job("over"), over);
    assert_eq!(
        sessions.output(&job("over"), Stream::Stdout, End::Head),
        Ok(b"kept".to_vec())
    );
    assert_eq!(
        sessions.output(&job("over"), Stream::Stderr, End::Tail),
        Ok(Vec::new())
    );
}

/// With no session, a check or a disk operation is done directly; with
/// one, the session answers. One that has ended is seen out before
/// anything is done directly.
#[test]
fn the_disk_and_the_check_go_through_a_live_session_only() {
    let (sessions, started, ended) = stand_ins();
    assert_eq!(
        sessions.probe(|| Err(String::from("checked directly"))),
        Err(String::from("checked directly"))
    );
    assert_eq!(
        sessions.disk(RoomDiskOp::Status, |op| Err(format!("direct {op:?}"))),
        Err(String::from("direct Status"))
    );
    assert_eq!(started.load(Atomic::SeqCst), 0, "a check started a session");

    assert_eq!(run(&sessions, "a"), Ok(stood_in(1, "a")));
    assert_eq!(
        sessions.probe(|| unreachable!("a live session answers")),
        Ok(())
    );
    assert_eq!(
        sessions.disk(RoomDiskOp::Open, |_| unreachable!("a live session answers")),
        Err(String::from("session 1 Open"))
    );

    assert_eq!(run(&sessions, "once"), Ok(stood_in(1, "once")));
    let done = sessions.disk(RoomDiskOp::Close, |op| {
        assert_eq!(ended.load(Atomic::SeqCst), 1, "the session wasn't seen out");
        Err(format!("direct {op:?}"))
    });
    assert_eq!(done, Err(String::from("direct Close")));
    assert_eq!(started.load(Atomic::SeqCst), 1);
}

/// When the broker goes the session is ended and seen out, and nothing
/// starts or is done after.
#[test]
fn a_shut_spawner_ends_its_session_and_starts_no_other() {
    let (sessions, started, ended) = stand_ins();
    assert_eq!(run(&sessions, "a"), Ok(stood_in(1, "a")));
    sessions.shut("the broker has gone");
    assert_eq!(ended.load(Atomic::SeqCst), 1, "the session wasn't seen out");
    assert_eq!(run(&sessions, "b"), Err(String::from(LEAVING)));
    assert_eq!(
        sessions.probe(|| unreachable!("nothing is done after")),
        Err(String::from(LEAVING))
    );
    assert_eq!(
        sessions.disk(RoomDiskOp::Status, |_| unreachable!(
            "nothing is done after"
        )),
        Err(String::from(LEAVING))
    );
    assert_eq!(started.load(Atomic::SeqCst), 1);

    let (idle, started, _ended) = stand_ins();
    idle.shut("the broker has gone");
    assert_eq!(run(&idle, "a"), Err(String::from(LEAVING)));
    assert_eq!(started.load(Atomic::SeqCst), 0);
}

#[test]
fn a_session_that_cannot_start_is_refused_with_why() {
    let sessions = Sessions::new(Arc::default(), |_| Err(io::Error::other("no threads left")));
    let said = run(&sessions, "a").unwrap_err();
    assert_eq!(said, "cannot start the Room session: no threads left");
    let said = run(&sessions, "a").unwrap_err();
    assert_eq!(said, "cannot start the Room session: no threads left");
}

/// The job a session is started for is waiting for its thread when it
/// starts, as it was asked for, with the connection it was asked on.
#[test]
fn a_started_session_finds_its_first_job_waiting() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let saw = Arc::clone(&seen);
    let sessions = Sessions::new(Arc::default(), move |seat: Seat| {
        let saw = Arc::clone(&saw);
        std::thread::Builder::new().spawn(move || {
            if let Ok(Message::Run {
                run,
                checked,
                mut requester,
                reply,
                ..
            }) = seat.inbox.try_recv()
            {
                saw.lock().unwrap().push((*run, checked));
                requester.write_all(b"!").unwrap();
                let _ = reply.send(Ok(stood_in(7, "echo")));
            }
        })
    });
    let (mut broker, requester) = UnixStream::pair().unwrap();
    broker.set_read_timeout(Some(LONG)).unwrap();
    let asked = RoomRun {
        dir: Some(String::from("/session")),
        stdin: b"in".to_vec(),
        limit_secs: Some(30),
        wait_secs: 2,
        ..a_run("echo", &["echo", "hello"])
    };
    assert_eq!(sessions.run(&asked, &requester), Ok(stood_in(7, "echo")));
    let held = Checked {
        dir: String::from("/session"),
        limit: Duration::from_secs(30),
        wait: Duration::from_secs(2),
    };
    assert_eq!(*seen.lock().unwrap(), [(asked, held)]);
    let mut byte = [0_u8; 1];
    broker.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"!");
}

/// A page is asked of the supervisor in its turn, as it was asked for,
/// one thing at a time, and its answer goes to whoever asked. That a file
/// isn't there is no fault of the session's. Nothing of a page is kept.
#[test]
fn a_page_is_asked_of_the_supervisor_in_its_turn_and_handed_back() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        // Asked for before the supervisor is ready, it waits, as a job
        // does.
        let (_first_broker, first) = ask_page(&rig.door, "/workspace/notes.txt", 3, 40);
        let (_job_broker, ran) = ask_run(&rig.door, "between");
        let (_second_broker, second) = ask_page(&rig.door, "/etc/os-release", 1, 1);
        assert!(unasked(&rig.room, SHORT), "asked before it was ready");
        say_ready(&rig.room);
        let (nonce, asked) = page_asked(&rig.room);
        assert_eq!(asked, (String::from("/workspace/notes.txt"), 3, 40));
        assert!(unasked(&rig.room, SHORT), "asked two things at once");
        assert!(unanswered(&first));
        let said = Say::Page {
            nonce,
            page: page_of(b"three\n"),
        };
        says(&rig.room, &said);
        assert_eq!(wait(&first), Ok(page_of(b"three\n")));
        starts(&rig.room, "between");
        let (nonce, asked) = page_asked(&rig.room);
        assert_eq!(asked, (String::from("/etc/os-release"), 1, 1));
        let said = Say::NoPage {
            nonce,
            why: NoPage::NoSuchFile,
        };
        says(&rig.room, &said);
        assert_eq!(
            wait(&second),
            Err(String::from("the Room has no such file"))
        );
        ends(&rig.room, "between", Ended::Exited(0), b"");
        assert_eq!(outcome(&answered(&ran).1).0, Ended::Exited(0));
        // The longest page there can be is one.
        let (_third_broker, third) = ask_page(&rig.door, "/workspace/long", 1, 1);
        let (nonce, _) = page_asked(&rig.room);
        let full = Page {
            bytes: vec![b'x'; PAGE_BYTES],
            more: true,
            cut: true,
            size: 0,
        };
        let said = Say::Page {
            nonce,
            page: full.clone(),
        };
        says(&rig.room, &said);
        assert_eq!(wait(&third), Ok(full));
        assert!(!held.is_finished(), "the session ended over a page");
        assert!(rig.door.send(Message::End { reason: "done" }).is_ok());
        assert_eq!(held.join().unwrap(), "done");
    });
    assert_eq!(
        opened(&rig.ledger).over.len(),
        1,
        "a page was kept as a job"
    );
}

/// How a page comes out when the session that was asked for it ends on
/// what `script` has its supervisor do with the ask's nonce: nothing is
/// heard until the Room is down, and then why it ended.
fn read_by(script: impl FnOnce(&UnixStream, String)) -> PageReply {
    let (rig, mut kept) = new_rig(limits());
    let (_broker, answer) = ask_page(&rig.door, "/workspace/a", 1, 1);
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| kept.hold(&rig.supervisor, rig.launcher.as_fd()));
        say_ready(&rig.room);
        let (nonce, _) = page_asked(&rig.room);
        script(&rig.room, nonce);
        seen_out(held, &rig.room);
    });
    assert!(
        answer.try_recv().is_err(),
        "answered before the Room was down"
    );
    kept.finish(Ok(()), |_| unreachable!());
    wait(&answer)
}

/// A page that can't be one, an answer to something else, and anything
/// but a page in answer to the ask for one end the session, and whoever
/// asked hears why.
#[test]
fn a_page_that_cannot_be_one_ends_the_session() {
    let page = |length: usize, more: bool, cut: bool| Page {
        bytes: vec![b'x'; length],
        more,
        cut,
        size: 0,
    };
    for bad in [
        page(PAGE_BYTES + 1, true, false),
        page(PAGE_BYTES - 1, true, true),
        page(PAGE_BYTES, false, true),
    ] {
        let read = read_by(|room, nonce| says(room, &Say::Page { nonce, page: bad }));
        assert_eq!(read, Err(String::from(BAD_PAGE)));
    }
    let read = read_by(|room, _| {
        let said = Say::Page {
            nonce: "0".repeat(64),
            page: Page::default(),
        };
        says(room, &said);
    });
    assert_eq!(read, Err(String::from(OUT_OF_TURN)));
    let read = read_by(|room, nonce| says(room, &Say::Started { nonce }));
    assert_eq!(read, Err(String::from(OUT_OF_TURN)));
    let read = read_by(|room, nonce| {
        let said = Say::Piece {
            nonce,
            bytes: b"text".to_vec(),
        };
        says(room, &said);
    });
    assert_eq!(read, Err(String::from(OUT_OF_TURN)));
    let read = read_by(|room, _| gone(room));
    assert_eq!(read, Err(String::from(CLOSED)));
    // A page in answer to anything else is out of turn too.
    let wrong: [fn(String) -> Say; 2] = [
        |nonce| Say::Page {
            nonce,
            page: Page::default(),
        },
        |nonce| Say::NoPage {
            nonce,
            why: NoPage::Failed,
        },
    ];
    for said in wrong {
        let ended = ended_by(true, |far| {
            say_ready(far.room());
            let asked = hears(far.room());
            says(far.room(), &said(asked.nonce().to_owned()));
        });
        assert_eq!(ended, OUT_OF_TURN);
    }
}

/// When a session ends, a page it was asked for and one still waiting
/// are answered once the Room is down, with why it ended.
#[test]
fn a_page_not_handed_back_is_answered_once_the_room_is_down() {
    let (rig, mut kept) = new_rig(limits());
    let (_asked_broker, asked) = ask_page(&rig.door, "/workspace/a", 1, 1);
    let (_waiting_broker, waiting) = ask_page(&rig.door, "/workspace/b", 1, 1);
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| kept.hold(&rig.supervisor, rig.launcher.as_fd()));
        say_ready(&rig.room);
        let (_nonce, of) = page_asked(&rig.room);
        assert_eq!(of.0, "/workspace/a");
        assert!(rig.door.send(Message::End { reason: "gone" }).is_ok());
        held.join().unwrap();
    });
    assert!(asked.try_recv().is_err() && waiting.try_recv().is_err());
    assert_eq!(kept.owed.len(), 2);
    assert!(kept.readers.is_empty() && kept.reading.is_none());
    kept.finish(Ok(()), |_| unreachable!());
    assert_eq!(wait(&asked), Err(String::from("gone")));
    assert_eq!(wait(&waiting), Err(String::from("gone")));
}

/// Before anything is launched: a page whose caller has gone launches
/// nothing, and a page to ask for is something in hand.
#[test]
fn nothing_is_launched_for_a_reader_that_has_gone() {
    let (rig, mut kept) = new_rig(limits());
    assert!(!kept.busy());
    let (_broker, answer) = ask_page(&rig.door, "/workspace/a", 1, 1);
    assert_eq!(kept.begin(), Ok(()));
    assert!(kept.busy(), "a page to ask for is something in hand");
    kept.finish(Err(String::from("no launch")), |_| unreachable!());
    assert_eq!(wait(&answer), Err(String::from("no launch")));

    let (rig, mut kept) = new_rig(limits());
    let (broker, answer) = ask_page(&rig.door, "/workspace/unwanted", 1, 1);
    let (_staying, stayed) = ask_page(&rig.door, "/workspace/wanted", 1, 1);
    gone(&broker);
    drop(broker);
    assert_eq!(kept.begin(), Ok(()), "one caller is still there");
    assert_eq!(wait(&answer), Err(String::from(REQUESTER_GONE)));
    let left: Vec<&str> = kept
        .readers
        .iter()
        .map(|reader| reader.path.as_str())
        .collect();
    assert_eq!(left, ["/workspace/wanted"]);
    kept.finish(Err(String::from("no launch")), |_| unreachable!());
    assert_eq!(wait(&stayed), Err(String::from("no launch")));

    let (rig, mut kept) = new_rig(limits());
    let (broker, answer) = ask_page(&rig.door, "/workspace/unwanted", 1, 1);
    gone(&broker);
    drop(broker);
    assert_eq!(kept.begin(), Err(String::from(REQUESTER_GONE)));
    assert_eq!(wait(&answer), Err(String::from(REQUESTER_GONE)));

    // A job's caller that is still there is a reason to launch when a
    // page's has gone, and a page's when a job's has.
    let (rig, mut kept) = new_rig(limits());
    let (broker, _answer) = ask_page(&rig.door, "/workspace/unwanted", 1, 1);
    let (_staying, _ran) = ask_run(&rig.door, "wanted");
    gone(&broker);
    drop(broker);
    assert_eq!(kept.begin(), Ok(()));
    let (rig, mut kept) = new_rig(limits());
    let (broker, _ran) = ask_run(&rig.door, "unwanted");
    let (_staying, _answer) = ask_page(&rig.door, "/workspace/wanted", 1, 1);
    gone(&broker);
    drop(broker);
    assert_eq!(kept.begin(), Ok(()));
}

/// A caller that goes while its page waits its turn isn't read for: the
/// supervisor is asked only for the page of the one still there.
#[test]
fn nothing_is_read_for_a_caller_that_has_gone() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let _closed = ClosedAtEnd(rig.room.try_clone().unwrap());
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        let (broker, answer) = ask_page(&rig.door, "/workspace/unwanted", 1, 1);
        let (_staying, stayed) = ask_page(&rig.door, "/workspace/wanted", 2, 3);
        gone(&broker);
        drop(broker);
        say_ready(&rig.room);
        let (nonce, asked) = page_asked(&rig.room);
        assert_eq!(asked, (String::from("/workspace/wanted"), 2, 3));
        assert_eq!(wait(&answer), Err(String::from(REQUESTER_GONE)));
        let said = Say::Page {
            nonce,
            page: page_of(b"two\n"),
        };
        says(&rig.room, &said);
        assert_eq!(wait(&stayed), Ok(page_of(b"two\n")));
        assert!(unasked(&rig.room, SHORT), "asked for a page nobody wants");
        assert!(rig.door.send(Message::End { reason: "done" }).is_ok());
        assert_eq!(held.join().unwrap(), "done");
    });
}

#[test]
fn every_reason_there_is_no_page_has_its_words() {
    let words = [
        NoPage::NoSuchFile,
        NoPage::Denied,
        NoPage::NotAFile,
        NoPage::NotText,
        NoPage::TooFar,
        NoPage::PastEnd(0),
        NoPage::PastEnd(12),
        NoPage::Failed,
    ]
    .map(no_page);
    let mut distinct = words.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), words.len());
    assert!(no_page(NoPage::NoSuchFile).contains("no such file"));
    assert!(no_page(NoPage::Denied).contains("may not read"));
    assert!(no_page(NoPage::NotAFile).contains("directory"));
    assert!(no_page(NoPage::NotText).contains("not text"));
    assert!(no_page(NoPage::TooFar).contains("32 MiB"));
    assert_eq!(no_page(NoPage::PastEnd(0)), "the file is empty");
    assert_eq!(
        no_page(NoPage::PastEnd(12)),
        "the file's last line is line 12"
    );
    assert_eq!(
        no_page(NoPage::PastEnd(1)),
        "the file's last line is line 1"
    );
    assert!(no_page(NoPage::Failed).contains("couldn't read"));
}

fn read(sessions: &Sessions, path: &str) -> PageReply {
    let (_broker, requester) = UnixStream::pair().unwrap();
    sessions.page(path, 2, 30, &requester)
}

/// A page starts a session when there is none, as a job does, and is
/// asked of the one there is. One that may not be asked for starts none.
#[test]
fn a_page_starts_a_session_and_the_next_uses_it() {
    let (sessions, started, ended) = stand_ins();
    assert_eq!(
        read(&sessions, "/workspace/a"),
        Ok(stood_page(1, "/workspace/a", 2, 30))
    );
    assert_eq!(
        read(&sessions, "/workspace/b"),
        Ok(stood_page(1, "/workspace/b", 2, 30))
    );
    assert_eq!(run(&sessions, "j"), Ok(stood_in(1, "j")));
    assert_eq!(started.load(Atomic::SeqCst), 1);
    assert_eq!(ended.load(Atomic::SeqCst), 0);

    let (sessions, started, _ended) = stand_ins();
    let (_broker, requester) = UnixStream::pair().unwrap();
    let said = read(&sessions, "workspace/a").unwrap_err();
    assert!(said.contains("absolute"), "{said}");
    let said = sessions.page("/a", 0, 1, &requester).unwrap_err();
    assert!(said.contains("counted from 1"), "{said}");
    let said = sessions.page("/a", 1, 0, &requester).unwrap_err();
    assert!(said.contains("2000 lines"), "{said}");
    assert_eq!(started.load(Atomic::SeqCst), 0, "a session was started");
    // A session that keeps ending before it answers is given up on.
    assert_eq!(read(&sessions, "/vanish"), Err(String::from(UNSETTLED)));
    let tried = started.load(Atomic::SeqCst);
    assert!((2..=ATTEMPTS).contains(&tried), "started {tried} sessions");
}

/// The page a session is started for is waiting for its thread when it
/// starts, as it was asked for, with the connection it was asked on.
#[test]
fn a_started_session_finds_its_first_page_waiting() {
    let sessions = Sessions::new(Arc::default(), |seat: Seat| {
        std::thread::Builder::new().spawn(move || {
            if let Ok(Message::Page {
                path,
                from,
                lines,
                mut requester,
                reply,
            }) = seat.inbox.try_recv()
            {
                requester.write_all(b"!").unwrap();
                let _ = reply.send(Ok(stood_page(7, &path, from, lines)));
            }
        })
    });
    let (mut broker, requester) = UnixStream::pair().unwrap();
    broker.set_read_timeout(Some(LONG)).unwrap();
    assert_eq!(
        sessions.page("/workspace/a", 5, 9, &requester),
        Ok(stood_page(7, "/workspace/a", 5, 9))
    );
    let mut byte = [0_u8; 1];
    broker.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"!");
}
