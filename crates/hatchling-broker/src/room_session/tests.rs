use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as Atomic};

use hatchling_core::framing::{read_frame, write_frame};

use super::*;
use crate::room_foreground::RoomForegroundStatus;
use crate::room_foreground::tests::{EXITED, different_nonce, result_frame};

/// Longer than any test takes, and short enough that a session that
/// fails to end on what a test does to it ends on this, with its reason.
const LONG: Duration = Duration::from_secs(10);

const fn limits() -> Limits {
    Limits {
        cap: LONG,
        idle: LONG,
        command: LONG,
        ready: LONG,
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
    (
        Rig {
            door,
            room,
            supervisor,
            launcher,
            alive,
            ops,
        },
        Kept::new(seat, limits, disk),
    )
}

/// Asks the session for a command: the broker's end of the connection it
/// is asked on, and where its answer comes.
fn ask_run(door: &Door, argv: &[&str]) -> (UnixStream, Receiver<RunReply>) {
    let (broker, requester) = UnixStream::pair().unwrap();
    let (reply, answer) = mpsc::channel();
    let sent = door.send(Message::Run {
        argv: argv.iter().map(|part| String::from(*part)).collect(),
        requester,
        received: Instant::now(),
        reply,
    });
    assert!(sent.is_ok(), "the session had ended");
    (broker, answer)
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

fn say_ready(room: &UnixStream) {
    write_frame(room, READY).unwrap();
}

/// The command the supervisor was sent, and the nonce it came with.
fn take_command(room: &UnixStream) -> (Vec<String>, String) {
    let mut request: Vec<String> = serde_json::from_slice(&read_frame(room).unwrap()).unwrap();
    let nonce = request.pop().unwrap();
    assert_eq!(nonce.len(), 64);
    (request, nonce)
}

/// Answers the next command as the supervisor would, and says what it was.
fn serve_one(room: &UnixStream) -> Vec<String> {
    let (request, nonce) = take_command(room);
    write_frame(room, &result_frame(&nonce, EXITED)).unwrap();
    request
}

fn exited(answer: &Receiver<RunReply>) {
    assert_eq!(
        wait(answer).unwrap().status,
        RoomForegroundStatus::Exited(23)
    );
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

/// Why a session ended that `script` was the supervisor of, with a first
/// command asked for when `asked`. Nothing else ends it: its owner, its
/// requester and whatever `script` left open outlive it.
fn ended_by(asked: bool, script: impl FnOnce(&mut Far)) -> String {
    let (rig, mut kept) = new_rig(limits());
    let Rig {
        door,
        room,
        supervisor,
        launcher,
        alive,
        ops: _,
    } = rig;
    let _asked = asked.then(|| ask_run(&door, &["/bin/true"]));
    let mut far = Far {
        room: Some(room),
        alive: Some(alive),
    };
    std::thread::scope(|scope| {
        let held = scope.spawn(|| kept.keep(&supervisor, launcher.as_fd()).unwrap_err());
        script(&mut far);
        held.join().unwrap()
    })
}

#[test]
fn an_installed_session_lives_an_hour_and_idles_fifteen_minutes() {
    assert_eq!(
        Limits::INSTALLED,
        Limits {
            cap: Duration::from_secs(3600),
            idle: Duration::from_secs(900),
            command: Duration::from_secs(30),
            ready: Duration::from_secs(30),
            retry: Duration::from_secs(5),
        }
    );
}

/// Which wait ends a session first: readiness until the supervisor is
/// ready, then the command in hand or else idleness, and the time limit
/// over all of them. A tie goes to the wait itself, not the limit.
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
    assert_eq!(unready.due(&limits, None), (at(5), Due::Unready));
    assert_eq!(unready.due(&limits, Some(at(3))), (at(3), Due::Command));
    assert_eq!(unready.due(&limits, Some(at(7))), (at(5), Due::Unready));
    assert_eq!(unready.due(&limits, Some(at(5))), (at(5), Due::Unready));

    let idle = clock(20, true);
    assert_eq!(idle.due(&limits, None), (at(30), Due::Idle));
    assert_eq!(idle.due(&limits, Some(at(50))), (at(50), Due::Command));
    assert_eq!(idle.due(&limits, Some(at(25))), (at(25), Due::Command));

    assert_eq!(clock(95, true).due(&limits, None), (at(100), Due::Cap));
    assert_eq!(clock(90, true).due(&limits, None), (at(100), Due::Idle));
    assert_eq!(
        clock(0, true).due(&limits, Some(at(150))),
        (at(100), Due::Cap)
    );
    assert_eq!(
        clock(0, true).due(&limits, Some(at(100))),
        (at(100), Due::Command)
    );
    let short = Limits {
        cap: Duration::from_secs(3),
        ..limits
    };
    assert_eq!(unready.due(&short, None), (at(3), Due::Cap));

    assert_eq!(Due::Unready.reason(), UNREADY);
    assert_eq!(Due::Command.reason(), EXPIRED);
    assert_eq!(Due::Cap.reason(), CAPPED);
    assert_eq!(Due::Idle.reason(), IDLE);
}

/// A frame is taken only when it is whole and alone: no part of one, none
/// longer than its limit, and nothing after it.
#[test]
fn a_frame_is_taken_whole_and_alone() {
    let mut heard = b"\0\0\0".to_vec();
    assert_eq!(frame(&mut heard, 5), Ok(None));
    assert_eq!(heard, b"\0\0\0");
    let mut heard = b"\0\0\0\x05read".to_vec();
    assert_eq!(frame(&mut heard, 5), Ok(None));
    heard.push(b'y');
    assert_eq!(frame(&mut heard, 5), Ok(Some(b"ready".to_vec())));
    assert!(heard.is_empty(), "a taken frame stayed");
    let mut heard = b"\0\0\0\x05ready!".to_vec();
    assert!(frame(&mut heard, 5).unwrap_err().contains("extra data"));
    let mut heard = b"\0\0\0\x06".to_vec();
    assert!(frame(&mut heard, 5).unwrap_err().contains("oversized"));
    let mut heard = b"\0\0\0\0".to_vec();
    assert_eq!(frame(&mut heard, 0), Ok(Some(Vec::new())));
}

/// Listening takes what is there and stops; it never takes more than a
/// frame of its limit and a byte past it, and it says a closed socket is
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
    assert_eq!(heard.len(), 8205, "the bound is a frame and a byte");
    assert_eq!(listen(&stream, &mut heard, 8200, "closed"), Ok(()));
    assert_eq!(heard.len(), 8205, "a full buffer took more");
    heard.clear();
    assert_eq!(listen(&stream, &mut heard, 8200, "closed"), Ok(()));
    assert_eq!(heard, vec![7; 795], "what was left was lost");
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

/// A command is with the supervisor only when all of it went at once. One
/// the socket took part of isn't sent, and awaits no result.
#[test]
fn a_command_the_supervisor_cannot_take_whole_is_not_sent() {
    let flight = |argv: Vec<String>| {
        let (_broker, requester) = UnixStream::pair().unwrap();
        let (reply, _answer) = mpsc::channel();
        Flight {
            argv,
            requester,
            reply,
            deadline: Instant::now() + LONG,
            nonce: None,
        }
    };
    let (supervisor, room) = UnixStream::pair().unwrap();
    supervisor.set_nonblocking(true).unwrap();
    // As small as the kernel allows: far less than the command below.
    rustix::net::sockopt::set_socket_send_buffer_size(&supervisor, 1).unwrap();
    let mut large = flight(vec![String::from("/bin/true"), "x".repeat(1 << 20)]);
    assert_eq!(large.dispatch(&supervisor), Err(String::from(UNSENT)));
    assert!(large.nonce.is_none(), "an unsent command awaits a result");
    drop((supervisor, room));

    let (supervisor, room) = UnixStream::pair().unwrap();
    room.set_read_timeout(Some(LONG)).unwrap();
    supervisor.set_nonblocking(true).unwrap();
    let mut small = flight(vec![String::from("/bin/true")]);
    assert_eq!(small.dispatch(&supervisor), Ok(()));
    let (request, nonce) = take_command(&room);
    assert_eq!(request, ["run", "/bin/true"]);
    assert_eq!(small.nonce, Some(nonce));
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

/// A second command runs in the Room the first one started: the session
/// is still held between them, and each answer is its own command's.
#[test]
fn a_session_runs_one_command_after_another_in_the_same_room() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        let (_first_broker, first) = ask_run(&rig.door, &["/bin/true", "one arg"]);
        say_ready(&rig.room);
        assert_eq!(serve_one(&rig.room), ["run", "/bin/true", "one arg"]);
        exited(&first);
        let (_second_broker, second) = ask_run(&rig.door, &["/bin/echo"]);
        assert_eq!(serve_one(&rig.room), ["run", "/bin/echo"]);
        exited(&second);
        assert!(!held.is_finished(), "the session ended between commands");
        assert!(rig.door.send(Message::End { reason: "done" }).is_ok());
        assert_eq!(held.join().unwrap(), "done");
    });
    assert!(
        kept.left,
        "a session told to end still waits on its cleanup"
    );
}

/// A command isn't sent before the supervisor is ready, and a result
/// that comes in two parts is one result.
#[test]
fn a_command_waits_for_readiness_and_its_result_may_come_in_parts() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        let (_broker, answer) = ask_run(&rig.door, &["/bin/true"]);
        std::thread::sleep(Duration::from_millis(200));
        let early = hatchling_sys::wait_readable_for(&[rig.room.as_fd()], Duration::ZERO).unwrap();
        assert_eq!(early, [false], "a command went to an unready supervisor");
        say_ready(&rig.room);
        let (_, nonce) = take_command(&rig.room);
        let result = result_frame(&nonce, EXITED);
        let mut bytes = u32::try_from(result.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend(result);
        let (first, rest) = bytes.split_at(9);
        send(&rig.room, first);
        std::thread::sleep(Duration::from_millis(200));
        assert!(answer.try_recv().is_err(), "part of a result was a result");
        send(&rig.room, rest);
        exited(&answer);
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// A session with nothing to do ends once it has been idle for its limit,
/// counted from its last command; a command that takes longer than the
/// limit isn't idleness.
#[test]
fn an_idle_session_ends_at_its_idle_limit_counted_from_its_last_command() {
    let idle = Duration::from_millis(400);
    let (rig, mut kept) = new_rig(Limits { idle, ..limits() });
    std::thread::scope(|scope| {
        let held = scope.spawn(|| {
            let reason = kept
                .keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err();
            (reason, Instant::now())
        });
        let (_broker, answer) = ask_run(&rig.door, &["/bin/true"]);
        say_ready(&rig.room);
        let (_, nonce) = take_command(&rig.room);
        std::thread::sleep(idle * 2);
        let answered = Instant::now();
        write_frame(&rig.room, &result_frame(&nonce, EXITED)).unwrap();
        exited(&answer);
        let (reason, ended) = held.join().unwrap();
        assert_eq!(reason, IDLE);
        let waited = ended.duration_since(answered);
        assert!(waited >= idle, "ended {waited:?} after its last command");
        assert!(waited < LONG / 2, "idled for {waited:?}");
    });
}

/// A session ends at its time limit though a command is running in it.
/// The command hears why only once the Room is down.
#[test]
fn a_session_ends_at_its_time_limit_whatever_it_is_doing() {
    let cap = Duration::from_millis(500);
    let (rig, mut kept) = new_rig(Limits { cap, ..limits() });
    let (_broker, answer) = ask_run(&rig.door, &["/bin/true"]);
    std::thread::scope(|scope| {
        let started = Instant::now();
        let held = scope.spawn(|| {
            kept.hold(&rig.supervisor, rig.launcher.as_fd());
            Instant::now()
        });
        say_ready(&rig.room);
        take_command(&rig.room);
        let lived = held.join().unwrap().duration_since(started);
        assert!(lived >= cap, "ended after {lived:?}");
        assert!(lived < LONG / 2, "lived {lived:?}");
    });
    assert!(
        answer.try_recv().is_err(),
        "answered before the Room was down"
    );
    kept.finish(Ok(()), |_| unreachable!("no disk operation is owed"));
    assert_eq!(wait(&answer), Err(String::from(CAPPED)));
}

/// A command that doesn't finish by its deadline, counted from when it
/// was asked for, ends the session, and hears so once the Room is down.
#[test]
fn a_command_past_its_deadline_ends_the_session() {
    let command = Duration::from_millis(400);
    let (rig, mut kept) = new_rig(Limits {
        command,
        ..limits()
    });
    let asked = Instant::now();
    let (_broker, answer) = ask_run(&rig.door, &["/bin/sleep", "60"]);
    std::thread::scope(|scope| {
        let held = scope.spawn(|| {
            kept.hold(&rig.supervisor, rig.launcher.as_fd());
            Instant::now()
        });
        say_ready(&rig.room);
        take_command(&rig.room);
        let took = held.join().unwrap().duration_since(asked);
        assert!(took >= command, "ended after {took:?}");
        assert!(took < LONG / 2, "took {took:?}");
    });
    assert!(
        answer.try_recv().is_err(),
        "answered before the Room was down"
    );
    kept.finish(Ok(()), |_| unreachable!("no disk operation is owed"));
    assert_eq!(wait(&answer), Err(String::from(EXPIRED)));
}

/// A supervisor that never says it is ready ends the session at the
/// readiness limit; part of a readiness frame isn't readiness.
#[test]
fn a_supervisor_that_is_never_ready_ends_the_session() {
    let ready = Duration::from_millis(400);
    let (rig, mut kept) = new_rig(Limits { ready, ..limits() });
    let started = Instant::now();
    std::thread::scope(|scope| {
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        send(&rig.room, b"\0\0\0\x05rea");
        assert_eq!(held.join().unwrap(), UNREADY);
    });
    let waited = started.elapsed();
    assert!(waited >= ready, "gave up after {waited:?}");
    assert!(waited < LONG / 2, "waited {waited:?}");
}

/// Whoever asked for a command going, or saying more, cancels it and
/// ends the session, before the supervisor is ready and after.
#[test]
fn a_requester_that_goes_or_says_more_ends_the_session() {
    for (ready, says_more) in [(false, false), (true, false), (true, true)] {
        let (rig, mut kept) = new_rig(limits());
        let (mut broker, _answer) = ask_run(&rig.door, &["/bin/true"]);
        std::thread::scope(|scope| {
            let held = scope.spawn(|| {
                kept.keep(&rig.supervisor, rig.launcher.as_fd())
                    .unwrap_err()
            });
            if ready {
                say_ready(&rig.room);
                take_command(&rig.room);
            }
            if says_more {
                broker.write_all(b"x").unwrap();
            } else {
                drop(broker);
            }
            assert_eq!(held.join().unwrap(), REQUESTER_GONE);
        });
    }
}

/// The supervisor is heard only when it is its turn: its readiness, then
/// the result of the command it was given. Anything else ends the session.
#[test]
fn a_supervisor_out_of_step_ends_the_session() {
    let said = |bytes: &'static [u8]| ended_by(false, move |far| send(far.room(), bytes));
    assert_eq!(said(b"\0\0\0\x05other"), BAD_READY);
    assert_eq!(said(b"\0\0\0\x04read"), BAD_READY);
    assert!(said(b"\0\0\0\x06").contains("oversized"));
    assert!(said(b"\0\0\0\x05ready!").contains("extra data"));
    assert_eq!(ended_by(false, |far| drop(far.room.take())), CLOSED_EARLY);
    assert_eq!(
        ended_by(false, |far| {
            say_ready(far.room());
            std::thread::sleep(Duration::from_millis(100));
            send(far.room(), b"!");
        }),
        OUT_OF_TURN
    );
    assert_eq!(
        ended_by(false, |far| {
            say_ready(far.room());
            drop(far.room.take());
        }),
        OUT_OF_TURN
    );
    assert_eq!(
        ended_by(false, |far| {
            say_ready(far.room());
            drop(far.alive.take());
        }),
        LAUNCHER_ENDED
    );
}

/// A result is taken only whole, alone, for this dispatch and within the
/// frame limit; one exactly at the limit is taken.
#[test]
fn a_result_is_checked_before_it_is_an_answer() {
    let answered = |answer: fn(&UnixStream, &str)| {
        ended_by(true, move |far| {
            say_ready(far.room());
            let (_, nonce) = take_command(far.room());
            answer(far.room(), &nonce);
        })
    };
    let different = answered(|room, nonce| {
        write_frame(room, &result_frame(&different_nonce(nonce), EXITED)).unwrap();
    });
    assert!(different.contains("different dispatch"), "{different}");
    let invalid = answered(|room, _| write_frame(room, b"invalid").unwrap());
    assert!(invalid.contains("invalid foreground result"), "{invalid}");
    let oversized = answered(|room, _| send(room, &[0, 1, 0, 1]));
    assert!(oversized.contains("oversized"), "{oversized}");
    let extra = answered(|room, nonce| {
        let result = result_frame(nonce, EXITED);
        let mut bytes = u32::try_from(result.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend(result);
        bytes.push(b'!');
        send(room, &bytes);
    });
    assert!(extra.contains("extra data"), "{extra}");
    let cut = answered(|room, _| {
        send(room, &[0, 0, 0, 8, b'{']);
        room.shutdown(std::net::Shutdown::Both).unwrap();
    });
    assert_eq!(cut, CLOSED);

    let (rig, mut kept) = new_rig(limits());
    let (_broker, answer) = ask_run(&rig.door, &["/bin/true"]);
    std::thread::scope(|scope| {
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        say_ready(&rig.room);
        let (_, nonce) = take_command(&rig.room);
        let mut result = result_frame(&nonce, EXITED);
        result.resize(hatchling_core::MAX_FRAME_BYTES, b' ');
        write_frame(&rig.room, &result).unwrap();
        exited(&answer);
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// One command at a time: a second while one runs is refused at once, and
/// the first still finishes.
#[test]
fn a_second_command_is_refused_while_one_runs() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        let (_first_broker, first) = ask_run(&rig.door, &["/bin/true"]);
        say_ready(&rig.room);
        let (_, nonce) = take_command(&rig.room);
        let (_second_broker, second) = ask_run(&rig.door, &["/bin/false"]);
        assert_eq!(wait(&second), Err(String::from(BUSY)));
        write_frame(&rig.room, &result_frame(&nonce, EXITED)).unwrap();
        exited(&first);
        let (_third_broker, third) = ask_run(&rig.door, &["/bin/false"]);
        assert_eq!(serve_one(&rig.room), ["run", "/bin/false"]);
        exited(&third);
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// Whether the Room works is answered by a live session once its
/// supervisor is ready, and not before.
#[test]
fn a_live_session_says_the_room_works_once_it_is_ready() {
    let (rig, mut kept) = new_rig(limits());
    std::thread::scope(|scope| {
        let held = scope.spawn(|| {
            kept.keep(&rig.supervisor, rig.launcher.as_fd())
                .unwrap_err()
        });
        let early = ask_probe(&rig.door);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            early.try_recv().is_err(),
            "an unready Room was said to work"
        );
        say_ready(&rig.room);
        assert_eq!(wait(&early), Ok(()));
        assert_eq!(wait(&ask_probe(&rig.door)), Ok(()));
        assert!(!held.is_finished(), "a check ended the session");
        drop(rig.door);
        assert_eq!(held.join().unwrap(), UNOWNED);
    });
}

/// The disk under a session: what leaves it open is done by the session,
/// in its own hold on the disk, even while a command runs. A close or a
/// wipe ends the session, and is done only once the Room is down.
#[test]
fn closing_or_wiping_the_disk_ends_the_session_first() {
    for closing in [RoomDiskOp::Close, RoomDiskOp::Wipe] {
        let (rig, mut kept) = new_rig(limits());
        let (_broker, answer) = ask_run(&rig.door, &["/bin/true"]);
        let closed = std::thread::scope(|scope| {
            let held = scope.spawn(|| kept.hold(&rig.supervisor, rig.launcher.as_fd()));
            say_ready(&rig.room);
            take_command(&rig.room);
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
        assert_eq!(wait(&answer), Err(String::from(DISK_CLOSING)));
    }
}

/// Before anything is launched: a command whose requester has already
/// gone launches nothing, and hears why the launch didn't happen; so
/// does a session whose disk is being closed.
#[test]
fn nothing_is_launched_for_a_requester_that_has_gone() {
    let (rig, mut kept) = new_rig(limits());
    assert_eq!(kept.begin(), Ok(()), "nothing asked is nothing against it");
    let (_broker, answer) = ask_run(&rig.door, &["/bin/true"]);
    assert_eq!(kept.begin(), Ok(()));
    kept.finish(Err(String::from("no launch")), |_| unreachable!());
    assert_eq!(wait(&answer), Err(String::from("no launch")));

    let (rig, mut kept) = new_rig(limits());
    let (broker, answer) = ask_run(&rig.door, &["/bin/true"]);
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
/// the command it was started for, and any other waiting for the same
/// Room, with why there is none. A check that was waiting is let go
/// unanswered, to be made again without a session.
#[test]
fn a_launch_that_fails_at_once_answers_every_waiting_command() {
    let (rig, kept) = new_rig(limits());
    let (_first_broker, first) = ask_run(&rig.door, &["/bin/true"]);
    let (_second_broker, second) = ask_run(&rig.door, &["/bin/false"]);
    let check = ask_probe(&rig.door);
    kept.finish(Err(String::from("no disk here")), |_| unreachable!());
    assert_eq!(wait(&first), Err(String::from("no disk here")));
    assert_eq!(wait(&second), Err(String::from("no disk here")));
    assert!(
        check.recv_timeout(LONG).is_err(),
        "a check was answered by a Room that never launched"
    );
}

/// A Room that comes down at the first try is left at once, and nothing
/// it owes is refused.
#[test]
fn a_session_that_comes_down_is_left_at_once() {
    let (_rig, mut kept) = new_rig(limits());
    let (reply, answer) = mpsc::channel();
    kept.owed = Some((reply, String::from("why it ended")));
    let mut tries = 0;
    kept.settle(&mut || {
        tries += 1;
        Ok(())
    });
    assert_eq!(tries, 1);
    kept.finish(Ok(()), |_| unreachable!());
    assert_eq!(wait(&answer), Err(String::from("why it ended")));
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
/// again on its own, refuses commands and checks with the reason, still
/// answers for the disk it holds, and is left once the Room is down.
#[test]
fn a_session_that_cannot_be_cleared_stays_held_and_says_so() {
    let (rig, mut kept) = new_rig(Limits {
        retry: Duration::from_millis(100),
        ..limits()
    });
    let (reply, owed) = mpsc::channel();
    kept.owed = Some((reply, String::from("why it ended")));
    let cleanup = Cleanup::stuck(1000);
    std::thread::scope(|scope| {
        let settled = scope.spawn(|| kept.settle(&mut || cleanup.down()));
        let _cleared = ClearedAtEnd(&cleanup);
        assert_eq!(
            wait(&owed),
            Err(String::from(HELD)),
            "a failed cleanup is what the command hears"
        );
        let (_broker, answer) = ask_run(&rig.door, &["/bin/true"]);
        assert_eq!(wait(&answer), Err(String::from(HELD)));
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

pub(crate) fn result(generation: usize) -> RoomForegroundResult {
    RoomForegroundResult {
        status: RoomForegroundStatus::Exited(u8::try_from(generation).unwrap()),
        stdout: Vec::new(),
        stderr: Vec::new(),
        stdout_truncated: false,
        stderr_truncated: false,
    }
}

/// A stand-in for the thread that holds a session: it answers with its
/// own number, and ends when told to or on the commands that say so.
fn stand_in(generation: usize, seat: &Seat) {
    while let Ok(message) = seat.inbox.recv() {
        match message {
            Message::Run { argv, reply, .. } => match argv.first().map(String::as_str) {
                Some("/vanish") => return,
                Some("/once") => {
                    let _ = reply.send(Ok(result(generation)));
                    return;
                }
                _ => {
                    let _ = reply.send(Ok(result(generation)));
                }
            },
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
    let sessions = Sessions::new(move |seat| {
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

fn run(sessions: &Sessions, program: &str) -> RunReply {
    let (_broker, requester) = UnixStream::pair().unwrap();
    sessions.run(&[String::from(program)], &requester)
}

#[test]
fn the_first_command_starts_a_session_and_the_next_uses_it() {
    let (sessions, started, ended) = stand_ins();
    assert_eq!(run(&sessions, "/bin/true"), Ok(result(1)));
    assert_eq!(run(&sessions, "/bin/true"), Ok(result(1)));
    assert_eq!(started.load(Atomic::SeqCst), 1);
    assert_eq!(ended.load(Atomic::SeqCst), 0);
}

#[test]
fn a_session_that_ended_is_replaced_by_the_next_command() {
    let (sessions, started, _ended) = stand_ins();
    assert_eq!(run(&sessions, "/once"), Ok(result(1)));
    assert_eq!(run(&sessions, "/bin/true"), Ok(result(2)));
    assert_eq!(run(&sessions, "/bin/true"), Ok(result(2)));
    assert_eq!(started.load(Atomic::SeqCst), 2);
}

#[test]
fn a_session_that_keeps_ending_unanswered_is_given_up_on() {
    let (sessions, started, _ended) = stand_ins();
    assert_eq!(run(&sessions, "/vanish"), Err(String::from(UNSETTLED)));
    let tried = started.load(Atomic::SeqCst);
    assert!((2..=ATTEMPTS).contains(&tried), "started {tried} sessions");
    let (sessions, started, _ended) = stand_ins();
    assert_eq!(run(&sessions, "/bin/true"), Ok(result(1)));
    assert_eq!(
        sessions.probe(|| unreachable!("a live session answers")),
        Ok(())
    );
    assert_eq!(started.load(Atomic::SeqCst), 1);
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

    assert_eq!(run(&sessions, "/bin/true"), Ok(result(1)));
    assert_eq!(
        sessions.probe(|| unreachable!("a live session answers")),
        Ok(())
    );
    assert_eq!(
        sessions.disk(RoomDiskOp::Open, |_| unreachable!("a live session answers")),
        Err(String::from("session 1 Open"))
    );

    assert_eq!(run(&sessions, "/once"), Ok(result(1)));
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
    assert_eq!(run(&sessions, "/bin/true"), Ok(result(1)));
    sessions.shut("the broker has gone");
    assert_eq!(ended.load(Atomic::SeqCst), 1, "the session wasn't seen out");
    assert_eq!(run(&sessions, "/bin/true"), Err(String::from(LEAVING)));
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
    assert_eq!(run(&idle, "/bin/true"), Err(String::from(LEAVING)));
    assert_eq!(started.load(Atomic::SeqCst), 0);
}

#[test]
fn a_session_that_cannot_start_is_refused_with_why() {
    let sessions = Sessions::new(|_| Err(io::Error::other("no threads left")));
    let said = run(&sessions, "/bin/true").unwrap_err();
    assert_eq!(said, "cannot start the Room session: no threads left");
    let said = run(&sessions, "/bin/true").unwrap_err();
    assert_eq!(said, "cannot start the Room session: no threads left");
}

/// The command a session is started for is waiting for its thread when
/// it starts, with the connection it was asked on.
#[test]
fn a_started_session_finds_its_first_command_waiting() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let saw = Arc::clone(&seen);
    let sessions = Sessions::new(move |seat: Seat| {
        let saw = Arc::clone(&saw);
        std::thread::Builder::new().spawn(move || {
            if let Ok(Message::Run {
                argv,
                mut requester,
                reply,
                ..
            }) = seat.inbox.try_recv()
            {
                saw.lock().unwrap().push(argv);
                requester.write_all(b"!").unwrap();
                let _ = reply.send(Ok(result(7)));
            }
        })
    });
    let (mut broker, requester) = UnixStream::pair().unwrap();
    broker.set_read_timeout(Some(LONG)).unwrap();
    let argv = [String::from("/bin/echo"), String::from("hello")];
    assert_eq!(sessions.run(&argv, &requester), Ok(result(7)));
    assert_eq!(*seen.lock().unwrap(), [argv.to_vec()]);
    let mut byte = [0_u8; 1];
    broker.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"!");
}
