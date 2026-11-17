use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;

/// What a stand-in for the spawner was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asked {
    Run(RoomRun),
    Job(String, u32),
    Stop(String),
    Output(String, Stream, End),
    Page(String, u64, u32),
    Search(RoomSearch),
    Change(RoomChange),
    Take(String),
}

/// A stand-in for the spawner: it answers each ask from what it was told
/// to, and keeps what it was asked.
#[derive(Default)]
struct Fake {
    asked: RefCell<Vec<Asked>>,
    /// What a run is answered: a job's state, or a refusal.
    runs: RefCell<Vec<Result<RoomJobState, String>>>,
    /// How each job is when asked after, or stopped.
    jobs: RefCell<BTreeMap<String, Result<RoomJobState, String>>>,
    /// The kept pieces of what a job wrote.
    pieces: RefCell<BTreeMap<(String, Stream, End), Vec<u8>>>,
    /// What each ask for a page is answered, in order.
    pages: RefCell<Vec<Result<Page, String>>>,
    /// What each search is answered, in order.
    finds: RefCell<Vec<Result<Found, String>>>,
    /// What each change is answered, in order.
    changes: RefCell<Vec<Result<RoomChanged, String>>>,
}

impl Fake {
    fn asked(&self) -> Vec<Asked> {
        self.asked.borrow().clone()
    }

    fn answers_run(&self, state: Result<RoomJobState, String>) {
        self.runs.borrow_mut().push(state);
    }

    fn knows(&self, job: &str, state: Result<RoomJobState, String>) {
        self.jobs.borrow_mut().insert(job.to_owned(), state);
    }

    fn hands_back(&self, page: Result<Page, String>) {
        self.pages.borrow_mut().push(page);
    }

    fn finds(&self, found: Result<Found, String>) {
        self.finds.borrow_mut().push(found);
    }

    fn changes(&self, changed: Result<RoomChanged, String>) {
        self.changes.borrow_mut().push(changed);
    }

    fn keeps(&self, job: &str, stream: Stream, end: End, bytes: &[u8]) {
        self.pieces
            .borrow_mut()
            .insert((job.to_owned(), stream, end), bytes.to_vec());
    }

    fn known(&self, job: &JobId) -> Result<RoomJobAnswer, String> {
        self.jobs
            .borrow()
            .get(job.as_str())
            .cloned()
            .unwrap_or_else(|| Err(String::from(crate::room_session::NO_JOB)))
            .map(|state| RoomJobAnswer {
                started: false,
                job: RoomJob {
                    job: job.clone(),
                    state,
                },
            })
    }
}

impl Rooms for Fake {
    fn run(&self, run: &RoomRun) -> Result<RoomJobAnswer, String> {
        self.asked.borrow_mut().push(Asked::Run(run.clone()));
        let state = self.runs.borrow_mut().remove(0)?;
        Ok(RoomJobAnswer {
            started: true,
            job: RoomJob {
                job: run.job.clone(),
                state,
            },
        })
    }

    fn job(&self, job: &JobId, wait_secs: u32) -> Result<RoomJobAnswer, String> {
        self.asked
            .borrow_mut()
            .push(Asked::Job(job.as_str().to_owned(), wait_secs));
        self.known(job)
    }

    fn stop(&self, job: &JobId) -> Result<RoomJobAnswer, String> {
        self.asked
            .borrow_mut()
            .push(Asked::Stop(job.as_str().to_owned()));
        self.known(job)
    }

    fn output(&self, job: &JobId, stream: Stream, end: End) -> Result<Vec<u8>, String> {
        self.asked
            .borrow_mut()
            .push(Asked::Output(job.as_str().to_owned(), stream, end));
        self.pieces
            .borrow()
            .get(&(job.as_str().to_owned(), stream, end))
            .cloned()
            .ok_or_else(|| String::from("no such piece"))
    }

    fn page(&self, path: &str, from: u64, lines: u32) -> Result<Page, String> {
        self.asked
            .borrow_mut()
            .push(Asked::Page(path.to_owned(), from, lines));
        self.pages.borrow_mut().remove(0)
    }

    fn search(&self, search: &RoomSearch) -> Result<Found, String> {
        self.asked.borrow_mut().push(Asked::Search(search.clone()));
        self.finds.borrow_mut().remove(0)
    }

    fn change(&self, change: &RoomChange) -> Result<RoomChanged, String> {
        self.asked.borrow_mut().push(Asked::Change(change.clone()));
        self.changes.borrow_mut().remove(0)
    }

    fn take(&self, path: &str) -> Result<Vec<u8>, String> {
        self.asked.borrow_mut().push(Asked::Take(path.to_owned()));
        Err(String::from("no file here"))
    }
}

fn write(path: &str, text: &str, replace: bool) -> RoomOp {
    RoomOp::Write {
        path: path.to_owned(),
        text: text.to_owned(),
        replace,
    }
}

fn edit(path: &str, old: &str, new: &str, all: bool) -> RoomOp {
    RoomOp::Edit {
        path: path.to_owned(),
        old: old.to_owned(),
        new: new.to_owned(),
        all,
    }
}

fn search(pattern: &str, path: Option<&str>) -> RoomOp {
    RoomOp::Search {
        pattern: pattern.to_owned(),
        path: path.map(str::to_owned),
        glob: None,
        ignore_case: false,
        literal: false,
    }
}

fn read(path: &str, from: Option<u64>, lines: Option<u32>) -> RoomOp {
    RoomOp::Read {
        path: path.to_owned(),
        from,
        lines,
    }
}

fn session(fake: &Fake) -> RoomSession<'_> {
    RoomSession::over_rooms(true, Some(fake), Some(String::from("jabc")))
}

fn run(argv: &[&str]) -> RoomOp {
    RoomOp::Run {
        argv: argv.iter().map(|part| (*part).to_owned()).collect(),
        dir: None,
        stdin: None,
        wait_secs: None,
        limit_secs: None,
    }
}

fn kept(head: u32, tail: u32, dropped: u64) -> Captured {
    Captured {
        head,
        tail,
        dropped,
        spilled: u64::from(head) + dropped + u64::from(tail),
        spill_cut: false,
    }
}

const fn ended(code: u8, stdout: Captured, stderr: Captured) -> RoomJobState {
    RoomJobState::Ended {
        how: Ended::Exited(code),
        stopped: None,
        ran_ms: 12,
        stdout,
        stderr,
    }
}

/// A command that ends is told as it ended: its exit, what it wrote, and
/// where all of that is. It is asked of the Room as the session gave it,
/// under a name the broker made, waiting the ordinary wait.
#[test]
fn a_command_that_ends_is_told_with_what_it_wrote() {
    let fake = Fake::default();
    fake.answers_run(Ok(ended(3, kept(6, 0, 0), kept(0, 0, 0))));
    fake.keeps("jabc-1", Stream::Stdout, End::Head, b"hello\n");
    let mut room = session(&fake);
    let told = room.handle(&RoomOp::Run {
        argv: vec![String::from("cat"), String::from("a b")],
        dir: Some(String::from("/session")),
        stdin: Some(String::from("in")),
        wait_secs: None,
        limit_secs: Some(30),
    });
    assert_eq!(
        told,
        RoomResult::Ended {
            job: String::from("jabc-1"),
            exit: Some(3),
            signal: None,
            stopped: None,
            ran_ms: 12,
            stdout: RoomText {
                head: String::from("hello\n"),
                tail: String::new(),
                left_out: 0,
                total: 6,
                file: String::from("/session/jobs/jabc-1.stdout"),
                file_cut: false,
            },
            stderr: RoomText {
                file: String::from("/session/jobs/jabc-1.stderr"),
                ..RoomText::default()
            },
        }
    );
    // A stream that kept nothing is not asked for.
    assert_eq!(
        fake.asked(),
        [
            Asked::Run(RoomRun {
                job: "jabc-1".parse().unwrap(),
                argv: vec![String::from("cat"), String::from("a b")],
                dir: Some(String::from("/session")),
                stdin: b"in".to_vec(),
                limit_secs: Some(30),
                wait_secs: ROOM_WAIT_SECS,
            }),
            Asked::Output(String::from("jabc-1"), Stream::Stdout, End::Head),
        ]
    );
    // It is over: nothing is stopped when the session ends.
    drop(room);
    assert_eq!(fake.asked().len(), 2);
}

/// A signal, and a stop, are told as they were.
#[test]
fn how_a_command_was_ended_is_told() {
    let fake = Fake::default();
    fake.answers_run(Ok(RoomJobState::Ended {
        how: Ended::Signaled(9),
        stopped: Some(RoomJobStopped::Limit),
        ran_ms: 120_000,
        stdout: kept(0, 0, 0),
        stderr: kept(0, 0, 0),
    }));
    let mut room = session(&fake);
    let RoomResult::Ended {
        exit,
        signal,
        stopped,
        ..
    } = room.handle(&run(&["sleep", "600"]))
    else {
        unreachable!("it ended")
    };
    assert_eq!(
        (exit, signal, stopped),
        (None, Some(9), Some(RoomStopped::Limit))
    );
    fake.knows(
        "jabc-1",
        Ok(RoomJobState::Ended {
            how: Ended::Signaled(9),
            stopped: Some(RoomJobStopped::Asked),
            ran_ms: 5,
            stdout: kept(0, 0, 0),
            stderr: kept(0, 0, 0),
        }),
    );
    let RoomResult::Ended { stopped, .. } = room.handle(&RoomOp::Stop {
        job: String::from("jabc-1"),
    }) else {
        unreachable!("it was stopped")
    };
    assert_eq!(stopped, Some(RoomStopped::Asked));
}

/// A command still running is told so by its name; the session asks
/// after it and stops it by that name, and each command has its own.
#[test]
fn a_running_command_is_asked_after_and_stopped_by_its_name() {
    let fake = Fake::default();
    fake.answers_run(Ok(RoomJobState::Running { ran_ms: 10_000 }));
    fake.answers_run(Ok(RoomJobState::Running { ran_ms: 1 }));
    let mut room = session(&fake);
    assert_eq!(
        room.handle(&run(&["sleep", "30"])),
        RoomResult::Running {
            job: String::from("jabc-1"),
            ran_ms: 10_000,
        }
    );
    assert_eq!(
        room.handle(&run(&["sleep", "40"])),
        RoomResult::Running {
            job: String::from("jabc-2"),
            ran_ms: 1,
        }
    );
    fake.knows("jabc-1", Ok(RoomJobState::Running { ran_ms: 20_000 }));
    assert_eq!(
        room.handle(&RoomOp::Job {
            job: String::from("jabc-1"),
            wait_secs: Some(600),
        }),
        RoomResult::Running {
            job: String::from("jabc-1"),
            ran_ms: 20_000,
        }
    );
    fake.knows("jabc-2", Ok(ended(0, kept(0, 0, 0), kept(0, 0, 0))));
    assert!(matches!(
        room.handle(&RoomOp::Job {
            job: String::from("jabc-2"),
            wait_secs: None,
        }),
        RoomResult::Ended { exit: Some(0), .. }
    ));
    let asked = fake.asked();
    // No wait is longer than the longest, and none named is the ordinary.
    assert_eq!(
        asked.get(2..),
        Some(
            [
                Asked::Job(String::from("jabc-1"), ROOM_WAIT_MAX_SECS),
                Asked::Job(String::from("jabc-2"), ROOM_WAIT_SECS),
            ]
            .as_slice()
        )
    );
    // The session ends: the one still running is stopped, and only it.
    drop(room);
    assert_eq!(
        fake.asked().get(4..),
        Some([Asked::Stop(String::from("jabc-1"))].as_slice())
    );
}

/// A session asks after and stops only what it ran: any other name is
/// unknown to it, and the Room is never asked.
#[test]
fn a_session_reaches_only_its_own_commands() {
    let fake = Fake::default();
    fake.knows("jxyz-1", Ok(RoomJobState::Running { ran_ms: 1 }));
    let mut room = session(&fake);
    for name in ["jxyz-1", "jabc-1", "", "../x", "not a name"] {
        for op in [
            RoomOp::Job {
                job: name.to_owned(),
                wait_secs: None,
            },
            RoomOp::Stop {
                job: name.to_owned(),
            },
        ] {
            assert_eq!(
                room.handle(&op),
                RoomResult::Unknown {
                    job: name.to_owned()
                },
                "{name:?}"
            );
        }
    }
    assert_eq!(fake.asked(), []);
}

/// A session without its person there, and one with no Room, run
/// nothing; a command that can't be asked is refused before the Room is.
#[test]
fn what_may_not_be_asked_is_never_asked() {
    let fake = Fake::default();
    let mut unattended = RoomSession::over_rooms(false, Some(&fake), Some(String::from("j")));
    for op in [
        run(&["true"]),
        RoomOp::Job {
            job: String::from("j-1"),
            wait_secs: None,
        },
        RoomOp::Stop {
            job: String::from("j-1"),
        },
        read("notes.txt", None, None),
    ] {
        assert_eq!(
            unattended.handle(&op),
            RoomResult::Refused {
                reason: String::from(UNATTENDED)
            }
        );
    }
    let mut nowhere = RoomSession::over_rooms(true, None, Some(String::from("j")));
    assert_eq!(
        nowhere.handle(&run(&["true"])),
        RoomResult::Failed {
            reason: String::from(NO_ROOM),
            job: None,
        }
    );
    // With nothing to name a command by, there is no Room either.
    let mut nameless = RoomSession::over_rooms(true, Some(&fake), None);
    assert_eq!(
        nameless.handle(&run(&["true"])),
        RoomResult::Failed {
            reason: String::from(NO_ROOM),
            job: None,
        }
    );
    let mut room = session(&fake);
    for (op, why) in [
        (run(&[]), "empty"),
        (run(&["a/b"]), "a relative path"),
        (
            RoomOp::Run {
                argv: vec![String::from("true")],
                dir: Some(String::from("relative")),
                stdin: None,
                wait_secs: None,
                limit_secs: None,
            },
            "a relative directory",
        ),
        (
            RoomOp::Run {
                argv: vec![String::from("true")],
                dir: None,
                stdin: None,
                wait_secs: None,
                limit_secs: Some(601),
            },
            "too long a limit",
        ),
    ] {
        assert!(
            matches!(room.handle(&op), RoomResult::Refused { .. }),
            "{why}"
        );
    }
    drop((unattended, nowhere, nameless, room));
    assert_eq!(fake.asked(), []);
}

/// A run the spawner doesn't answer with a job: what became of it is
/// asked before anything is said. Known not started, it is forgotten.
/// Started all the same, it is told as it is. Not known, its name is
/// given, to ask after before anything is run again.
#[test]
fn a_run_with_no_answer_is_asked_after_before_it_is_told() {
    let fake = Fake::default();
    let mut room = session(&fake);
    // Not started: the Room says there is no such job.
    fake.answers_run(Err(String::from("the Room couldn't start that program")));
    assert_eq!(
        room.handle(&run(&["no-such"])),
        RoomResult::Failed {
            reason: String::from("the Room couldn't start that program"),
            job: None,
        }
    );
    assert_eq!(
        room.handle(&RoomOp::Job {
            job: String::from("jabc-1"),
            wait_secs: None,
        }),
        RoomResult::Unknown {
            job: String::from("jabc-1")
        },
        "a command that never started isn't the session's"
    );
    // Started all the same: the answer was lost, the job is there.
    fake.answers_run(Err(String::from("the spawner cannot be reached")));
    fake.knows("jabc-2", Ok(RoomJobState::Running { ran_ms: 7 }));
    assert_eq!(
        room.handle(&run(&["sleep", "5"])),
        RoomResult::Running {
            job: String::from("jabc-2"),
            ran_ms: 7,
        }
    );
    // Not known: the spawner can't be asked that either.
    fake.answers_run(Err(String::from("the spawner cannot be reached")));
    fake.knows("jabc-3", Err(String::from("still cannot be reached")));
    assert_eq!(
        room.handle(&run(&["touch", "x"])),
        RoomResult::Failed {
            reason: String::from("the spawner cannot be reached"),
            job: Some(String::from("jabc-3")),
        }
    );
    // It stays the session's: asked after by name, and stopped at the end.
    assert_eq!(
        room.handle(&RoomOp::Job {
            job: String::from("jabc-3"),
            wait_secs: Some(0),
        }),
        RoomResult::Failed {
            reason: String::from("still cannot be reached"),
            job: Some(String::from("jabc-3")),
        }
    );
    assert_eq!(
        room.handle(&RoomOp::Stop {
            job: String::from("jabc-3"),
        }),
        RoomResult::Failed {
            reason: String::from("still cannot be reached"),
            job: Some(String::from("jabc-3")),
        }
    );
    let before = fake.asked().len();
    drop(room);
    assert_eq!(
        fake.asked().get(before..),
        Some(
            [
                Asked::Stop(String::from("jabc-2")),
                Asked::Stop(String::from("jabc-3")),
            ]
            .as_slice()
        )
    );
}

/// A command lost with its Room is told so, and is over.
#[test]
fn a_command_lost_with_its_room_is_over() {
    let fake = Fake::default();
    fake.answers_run(Ok(RoomJobState::Lost {
        why: String::from("the room disk was closed"),
    }));
    let mut room = session(&fake);
    assert_eq!(
        room.handle(&run(&["sleep", "5"])),
        RoomResult::Lost {
            job: String::from("jabc-1"),
            why: String::from("the room disk was closed"),
        }
    );
    drop(room);
    assert_eq!(fake.asked().len(), 1, "nothing is stopped");
}

/// Long output is shown as its start and its end, with the count of what
/// is between; the pieces are the spawner's, and one that can't be had
/// shows nothing and is counted as left out.
#[test]
fn long_output_is_shown_as_its_start_and_its_end() {
    let fake = Fake::default();
    let head: Vec<u8> = (0..16_384_u32)
        .map(|i| b'a' + u8::try_from(i % 26).unwrap())
        .collect();
    let tail: Vec<u8> = (0..16_384_u32)
        .map(|i| b'A' + u8::try_from(i % 26).unwrap())
        .collect();
    fake.answers_run(Ok(ended(0, kept(16_384, 16_384, 1_000_000), kept(5, 0, 0))));
    fake.keeps("jabc-1", Stream::Stdout, End::Head, &head);
    fake.keeps("jabc-1", Stream::Stdout, End::Tail, &tail);
    let mut room = session(&fake);
    let RoomResult::Ended { stdout, stderr, .. } = room.handle(&run(&["seq", "1", "200000"]))
    else {
        unreachable!("it ended")
    };
    assert_eq!(stdout.head.as_bytes(), head.get(..ROOM_HEAD_BYTES).unwrap());
    assert_eq!(
        stdout.tail.as_bytes(),
        tail.get(16_384 - ROOM_TAIL_BYTES..).unwrap()
    );
    assert_eq!(stdout.total, 1_032_768);
    assert_eq!(stdout.left_out, 1_032_768 - 1024 - 3072);
    // The standard error's piece couldn't be had.
    assert_eq!(
        stderr,
        RoomText {
            head: String::new(),
            tail: String::new(),
            left_out: 5,
            total: 5,
            file: String::from("/session/jobs/jabc-1.stderr"),
            file_cut: false,
        }
    );
}

/// What is shown of a stream, at each of its edges.
#[test]
fn what_is_shown_of_a_stream() {
    let most = ROOM_HEAD_BYTES + ROOM_TAIL_BYTES;
    let bytes = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|i| b'0' + u8::try_from(i % 10).unwrap())
            .collect()
    };
    let count = |n: usize| u32::try_from(n).unwrap();
    // No longer than what is shown: all of it, in the start.
    for n in [0, 1, most - 1, most] {
        let all = bytes(n);
        assert_eq!(
            shown(&all, &[], &kept(count(n), 0, 0)),
            (words(&all), String::new(), 0),
            "{n}"
        );
    }
    // One byte more: the start and the end, and the one between them.
    let all = bytes(most + 1);
    let (head, tail, left_out) = shown(&all, &[], &kept(count(most + 1), 0, 0));
    assert_eq!(head.as_bytes(), all.get(..ROOM_HEAD_BYTES).unwrap());
    assert_eq!(tail.as_bytes(), all.get(ROOM_HEAD_BYTES + 1..).unwrap());
    assert_eq!(left_out, 1);
    // A short tail piece: the end reaches back into the head piece.
    let (first, last) = (bytes(16_384), b"END".to_vec());
    let (head, tail, left_out) = shown(&first, &last, &kept(16_384, 3, 0));
    assert_eq!(head.len(), ROOM_HEAD_BYTES);
    assert_eq!(tail.len(), ROOM_TAIL_BYTES);
    assert!(tail.ends_with("END"), "{tail}");
    assert_eq!(
        tail.as_bytes().get(..ROOM_TAIL_BYTES - 3),
        first.get(16_384 - (ROOM_TAIL_BYTES - 3)..)
    );
    assert_eq!(left_out, 16_387 - 1024 - 3072);
    // A gap between the pieces: the end is the tail piece's alone.
    let (head, tail, left_out) = shown(&first, &last, &kept(16_384, 3, 10));
    assert_eq!((head.len(), tail.as_str()), (ROOM_HEAD_BYTES, "END"));
    assert_eq!(left_out, 16_397 - 1024 - 3);
    // A cut never splits a character: the start ends before one it would
    // split, and the end starts after.
    let mut wide = vec![b'x'; ROOM_HEAD_BYTES - 1];
    wide.extend("\u{20ac}".as_bytes());
    wide.extend(vec![b'y'; most]);
    let (head, tail, _) = shown(&wide, &[], &kept(count(wide.len()), 0, 0));
    assert_eq!(head, "x".repeat(ROOM_HEAD_BYTES - 1));
    assert_eq!(tail, "y".repeat(ROOM_TAIL_BYTES));
    let mut wide = vec![b'x'; most];
    wide.extend("\u{20ac}".as_bytes());
    wide.extend(vec![b'y'; ROOM_TAIL_BYTES - 2]);
    let (_, tail, _) = shown(&wide, &[], &kept(count(wide.len()), 0, 0));
    assert_eq!(tail, "y".repeat(ROOM_TAIL_BYTES - 2));
    // A character of four bytes, cut at each place inside it: the start
    // ends before it, and the end starts after it.
    for within in 1..=3 {
        let mut wide = vec![b'x'; ROOM_HEAD_BYTES - within];
        wide.extend("\u{1f600}".as_bytes());
        wide.extend(vec![b'y'; most]);
        let (head, _, _) = shown(&wide, &[], &kept(count(wide.len()), 0, 0));
        assert_eq!(head, "x".repeat(ROOM_HEAD_BYTES - within), "{within}");
        let mut wide = vec![b'x'; most];
        wide.extend("\u{1f600}".as_bytes());
        wide.extend(vec![b'y'; ROOM_TAIL_BYTES - within]);
        let (_, tail, _) = shown(&wide, &[], &kept(count(wide.len()), 0, 0));
        assert_eq!(tail, "y".repeat(ROOM_TAIL_BYTES - within), "{within}");
    }
    // A cut that falls between two characters takes neither apart.
    let mut wide = vec![b'x'; ROOM_HEAD_BYTES - 4];
    wide.extend("\u{1f600}".as_bytes());
    wide.extend(vec![b'y'; most - 4]);
    wide.extend("\u{1f600}".as_bytes());
    wide.extend(vec![b'z'; ROOM_TAIL_BYTES - 4]);
    let (head, tail, _) = shown(&wide, &[], &kept(count(wide.len()), 0, 0));
    assert!(head.ends_with('\u{1f600}'), "{head}");
    assert!(tail.starts_with('\u{1f600}'), "{tail}");
    // Bytes that are no character's are cut where they are, and shown as
    // they can be: a stream of them is never shown as nothing.
    let noise = vec![0x80_u8; most * 2];
    let (head, tail, left_out) = shown(&noise, &[], &kept(count(noise.len()), 0, 0));
    assert_eq!(head, "\u{fffd}".repeat(ROOM_HEAD_BYTES));
    assert_eq!(tail, "\u{fffd}".repeat(ROOM_TAIL_BYTES));
    assert_eq!(left_out, length(&noise) - 4096);
    // What isn't text is shown as it can be.
    assert_eq!(shown(b"a\xffb", &[], &kept(3, 0, 0)).0, "a\u{fffd}b");
}

/// A run is offered its computer only in a chat with the Room open, and
/// whether the Room is open isn't asked for any other run.
#[test]
fn a_computer_is_offered_only_to_a_chat_with_the_room_open() {
    assert!(offered(true, || true));
    assert!(!offered(true, || false));
    let asked = std::cell::Cell::new(false);
    assert!(!offered(false, || {
        asked.set(true);
        true
    }));
    assert!(
        !asked.get(),
        "the spawner is asked nothing for a run with no person there"
    );
}

/// There is a Room to offer where there is a spawner and its room disk
/// is open with nothing turning the room off; with no spawner the disk
/// isn't asked after.
#[test]
fn a_room_is_open_with_a_spawner_and_an_open_disk() {
    use hatchling_core::ui::RoomDiskOff;
    let disk = |state, off| RoomDiskView {
        state,
        off,
        cap: None,
        used: None,
        free: None,
        on_disk: None,
        host_free: None,
    };
    assert!(is_open(true, || disk(RoomDiskCondition::Open, None)));
    assert!(!is_open(true, || disk(
        RoomDiskCondition::Open,
        Some(RoomDiskOff::NotRead)
    )));
    for state in [
        RoomDiskCondition::Absent,
        RoomDiskCondition::Closed,
        RoomDiskCondition::Mapped,
        RoomDiskCondition::Off,
    ] {
        assert!(!is_open(true, || disk(state, None)), "{state:?}");
    }
    let asked = std::cell::Cell::new(false);
    assert!(!is_open(false, || {
        asked.set(true);
        disk(RoomDiskCondition::Open, None)
    }));
    assert!(!asked.get(), "with no spawner, nothing is asked of one");
    // This process has no spawner: it has no Room to offer.
    assert!(!open());
}

/// The wait a session names is never longer than the longest.
#[test]
fn a_wait_is_bounded() {
    assert_eq!(wait(None), ROOM_WAIT_SECS);
    assert_eq!(wait(Some(0)), 0);
    assert_eq!(wait(Some(ROOM_WAIT_MAX_SECS)), ROOM_WAIT_MAX_SECS);
    assert_eq!(wait(Some(ROOM_WAIT_MAX_SECS + 1)), ROOM_WAIT_MAX_SECS);
    assert_eq!((ROOM_WAIT_SECS, ROOM_WAIT_MAX_SECS), (10, 60));
    assert_eq!((ROOM_HEAD_BYTES, ROOM_TAIL_BYTES), (1024, 3072));
}

/// A session names so many commands and no more: a name is never used
/// twice.
#[test]
fn a_name_is_never_given_twice() {
    let fake = Fake::default();
    let mut room = session(&fake);
    room.named = u32::MAX - 1;
    assert_eq!(room.name().unwrap().as_str(), format!("jabc-{}", u32::MAX));
    assert_eq!(room.name(), None);
    assert_eq!(
        room.handle(&run(&["true"])),
        RoomResult::Refused {
            reason: String::from("this chat has run as many commands as it may")
        }
    );
    assert_eq!(fake.asked(), []);
}

/// The most a reply can hold fits one frame as it is written, whatever
/// the command wrote: a reply is never replaced for its size, which
/// would tell the agent nothing ran when something did.
#[test]
fn the_longest_reply_fits_one_frame() {
    // A control character is six bytes as JSON, and a byte that isn't
    // text is three as text: nothing a stream holds is longer written.
    for byte in [0_u8, 0xff, b'"'] {
        let piece = vec![byte; 16_384];
        let (head, tail, left_out) = shown(&piece, &piece, &kept(16_384, 16_384, u64::MAX / 4));
        let stream = RoomText {
            head,
            tail,
            left_out,
            total: u64::MAX,
            file: format!("/session/jobs/{}.stdout", "j".repeat(64)),
            file_cut: true,
        };
        let reply = hatchling_core::SessionResponse::Room(RoomResult::Ended {
            job: "j".repeat(64),
            exit: None,
            signal: Some(64),
            stopped: Some(RoomStopped::Limit),
            ran_ms: u64::MAX,
            stdout: stream.clone(),
            stderr: stream,
        });
        let written = serde_json::to_vec(&reply).unwrap().len();
        assert!(
            written <= hatchling_core::framing::MAX_FRAME_BYTES,
            "{written} bytes for a stream of {byte:#x}"
        );
        // And it is close: showing more would need the frame to be larger.
        if byte == 0 {
            assert!(
                written > hatchling_core::framing::MAX_FRAME_BYTES / 2,
                "{written}"
            );
        }
    }
}

/// A file is read as the session names it: under the workspace unless
/// its path is absolute, from the first line and the ordinary count
/// unless it says. What comes back is told as it came, with the path the
/// Room was asked for; bytes that aren't text are shown as the
/// replacement character. A read is no command: it is given no name.
#[test]
fn a_file_is_read_a_page_at_a_time() {
    let fake = Fake::default();
    let mut room = session(&fake);
    fake.hands_back(Ok(Page {
        bytes: b"one\ntwo\n".to_vec(),
        more: true,
        cut: false,
        size: 40,
    }));
    assert_eq!(
        room.handle(&read("notes/today.txt", None, None)),
        RoomResult::Page {
            path: String::from("/workspace/notes/today.txt"),
            from: 1,
            text: String::from("one\ntwo\n"),
            more: true,
            cut: false,
            size: 40,
        }
    );
    fake.hands_back(Ok(Page {
        bytes: b"caf\xe9 \xff".to_vec(),
        more: true,
        cut: true,
        size: 9000,
    }));
    assert_eq!(
        room.handle(&read("/etc/os-release", Some(7), Some(3))),
        RoomResult::Page {
            path: String::from("/etc/os-release"),
            from: 7,
            text: String::from("caf\u{fffd} \u{fffd}"),
            more: true,
            cut: true,
            size: 9000,
        }
    );
    // The most lines there are to ask for, and a file with nothing in it.
    fake.hands_back(Ok(Page::default()));
    assert_eq!(
        room.handle(&read("empty", Some(1), Some(2000))),
        RoomResult::Page {
            path: String::from("/workspace/empty"),
            from: 1,
            text: String::new(),
            more: false,
            cut: false,
            size: 0,
        }
    );
    assert_eq!(
        fake.asked(),
        [
            Asked::Page(String::from("/workspace/notes/today.txt"), 1, 200),
            Asked::Page(String::from("/etc/os-release"), 7, 3),
            Asked::Page(String::from("/workspace/empty"), 1, 2000),
        ]
    );
    // The next command has the first name: a read took none.
    fake.answers_run(Ok(ended(0, Captured::default(), Captured::default())));
    assert!(matches!(
        room.handle(&run(&["true"])),
        RoomResult::Ended { job, .. } if job == "jabc-1"
    ));
    drop(room);
    assert_eq!(fake.asked().len(), 4, "a read was stopped as a command is");
}

/// Why there is no page is told in the Room's words, and a read that may
/// not be asked for never is.
#[test]
fn a_file_that_cannot_be_read_says_why() {
    let fake = Fake::default();
    let mut room = session(&fake);
    fake.hands_back(Err(String::from("the Room has no such file")));
    assert_eq!(
        room.handle(&read("absent", None, None)),
        RoomResult::NoPage {
            reason: String::from("the Room has no such file")
        }
    );
    assert_eq!(fake.asked().len(), 1);
    let long = "f".repeat(hatchling_core::room::PATH_BYTES);
    for (op, why) in [
        (read("notes.txt", Some(0), None), "counted from 1"),
        (read("notes.txt", None, Some(0)), "2000 lines"),
        (read("notes.txt", None, Some(2001)), "2000 lines"),
        (read("a\0b", None, None), "path"),
        (read(&long, None, None), "path"),
    ] {
        let RoomResult::Refused { reason } = room.handle(&op) else {
            unreachable!("{op:?} was asked for");
        };
        assert!(reason.contains(why), "{reason}");
    }
    assert_eq!(fake.asked().len(), 1, "what may not be asked for was");
    let mut nowhere = RoomSession::over_rooms(true, None, Some(String::from("j")));
    assert_eq!(
        nowhere.handle(&read("notes.txt", None, None)),
        RoomResult::NoPage {
            reason: String::from(NO_ROOM)
        }
    );
}

#[test]
fn a_path_that_is_not_absolute_is_under_the_workspace() {
    assert_eq!(placed("notes.txt"), "/workspace/notes.txt");
    assert_eq!(placed("a/b.txt"), "/workspace/a/b.txt");
    assert_eq!(
        placed("/session/jobs/j-1.stdout"),
        "/session/jobs/j-1.stdout"
    );
    assert_eq!(placed("/"), "/");
    assert_eq!(placed(""), "/workspace/");
}

/// The longest page there can be, of the bytes that are longest written,
/// under the longest path, is a reply that fits one frame: a page is
/// never replaced for its size.
#[test]
fn the_longest_page_fits_one_frame() {
    for byte in [1_u8, 0xff, b'"'] {
        let fake = Fake::default();
        let mut room = session(&fake);
        fake.hands_back(Ok(Page {
            bytes: vec![byte; hatchling_core::room::PAGE_BYTES],
            more: true,
            cut: true,
            size: u64::MAX,
        }));
        let path = format!("/{}", "\u{1}".repeat(hatchling_core::room::PATH_BYTES - 1));
        let result = room.handle(&read(&path, Some(u64::MAX), Some(2000)));
        assert!(matches!(result, RoomResult::Page { .. }), "{result:?}");
        let reply = hatchling_core::SessionResponse::Room(result);
        let written = serde_json::to_vec(&reply).unwrap().len();
        assert!(
            written <= hatchling_core::framing::MAX_FRAME_BYTES,
            "{written} bytes for a page of {byte:#x}"
        );
    }
}

/// Files are searched as the session names them: in the workspace unless
/// it names a path, which is under the workspace unless it is absolute,
/// with the glob and the switches it gave. What was found is told as it
/// came, with where the Room was asked to look; bytes that aren't text
/// are shown as the replacement character. A search is no command: it is
/// given no name.
#[test]
fn files_are_searched_as_the_session_names_them() {
    let fake = Fake::default();
    let mut room = session(&fake);
    fake.finds(Ok(Found {
        bytes: b"/workspace/a.txt:3:to do\n".to_vec(),
        more: true,
    }));
    assert_eq!(
        room.handle(&search("to do", None)),
        RoomResult::Found {
            path: String::from("/workspace"),
            text: String::from("/workspace/a.txt:3:to do\n"),
            more: true,
        }
    );
    fake.finds(Ok(Found {
        bytes: b"/workspace/notes/b:1:caf\xe9\n".to_vec(),
        more: false,
    }));
    let every = RoomOp::Search {
        pattern: String::from("CAF"),
        path: Some(String::from("notes")),
        glob: Some(String::from("*.md")),
        ignore_case: true,
        literal: false,
    };
    assert_eq!(
        room.handle(&every),
        RoomResult::Found {
            path: String::from("/workspace/notes"),
            text: String::from("/workspace/notes/b:1:caf\u{fffd}\n"),
            more: false,
        }
    );
    // Nothing matched, somewhere that isn't the workspace, for the very
    // words: each switch is its own.
    fake.finds(Ok(Found::default()));
    let words = RoomOp::Search {
        pattern: String::from("x"),
        path: Some(String::from("/etc")),
        glob: None,
        ignore_case: false,
        literal: true,
    };
    assert_eq!(
        room.handle(&words),
        RoomResult::Found {
            path: String::from("/etc"),
            text: String::new(),
            more: false,
        }
    );
    let asked_for = |pattern: &str, path: &str| RoomSearch {
        pattern: pattern.to_owned(),
        path: path.to_owned(),
        glob: None,
        ignore_case: false,
        literal: false,
    };
    assert_eq!(
        fake.asked(),
        [
            Asked::Search(asked_for("to do", "/workspace")),
            Asked::Search(RoomSearch {
                glob: Some(String::from("*.md")),
                ignore_case: true,
                ..asked_for("CAF", "/workspace/notes")
            }),
            Asked::Search(RoomSearch {
                literal: true,
                ..asked_for("x", "/etc")
            }),
        ]
    );
    // The next command has the first name: a search took none.
    fake.answers_run(Ok(ended(0, Captured::default(), Captured::default())));
    assert!(matches!(
        room.handle(&run(&["true"])),
        RoomResult::Ended { job, .. } if job == "jabc-1"
    ));
    drop(room);
    assert_eq!(
        fake.asked().len(),
        4,
        "a search was stopped as a command is"
    );
}

/// Why nothing was searched is told in the Room's words; a search that
/// may not be asked for never is; one with no Room to ask, or made for a
/// run with nobody there, is said as that.
#[test]
fn a_search_that_cannot_be_made_says_why() {
    let fake = Fake::default();
    let mut room = session(&fake);
    fake.finds(Err(String::from("the Room has nothing at that path")));
    assert_eq!(
        room.handle(&search("x", Some("absent"))),
        RoomResult::NoSearch {
            reason: String::from("the Room has nothing at that path")
        }
    );
    assert_eq!(fake.asked().len(), 1);
    let long = "g".repeat(hatchling_core::room::GLOB_BYTES + 1);
    let with_glob = |glob: &str| RoomOp::Search {
        pattern: String::from("x"),
        path: None,
        glob: Some(glob.to_owned()),
        ignore_case: false,
        literal: false,
    };
    for (op, why) in [
        (search("", None), "pattern"),
        (search("a\nb", None), "pattern"),
        (search("x", Some("a\0b")), "path"),
        (with_glob(""), "glob"),
        (with_glob(&long), "glob"),
    ] {
        let RoomResult::Refused { reason } = room.handle(&op) else {
            unreachable!("{op:?} was asked for");
        };
        assert!(reason.contains(why), "{reason}");
    }
    assert_eq!(fake.asked().len(), 1, "what may not be asked for was");
    let mut nowhere = RoomSession::over_rooms(true, None, Some(String::from("j")));
    assert_eq!(
        nowhere.handle(&search("x", None)),
        RoomResult::NoSearch {
            reason: String::from(NO_ROOM)
        }
    );
    let mut alone = RoomSession::over_rooms(false, Some(&fake), Some(String::from("j")));
    assert_eq!(alone.handle(&search("x", None)), refused(UNATTENDED));
    assert_eq!(fake.asked().len(), 1, "a run with nobody there searched");
}

/// The most a search can find, of the bytes that are longest written,
/// under the longest path, is a reply that fits one frame: what was found
/// is never replaced for its size.
#[test]
fn the_most_a_search_finds_fits_one_frame() {
    for byte in [1_u8, 0xff, b'"'] {
        let fake = Fake::default();
        let mut room = session(&fake);
        fake.finds(Ok(Found {
            bytes: vec![byte; hatchling_core::room::FOUND_BYTES],
            more: true,
        }));
        let path = format!("/{}", "\u{1}".repeat(hatchling_core::room::PATH_BYTES - 1));
        let result = room.handle(&search("x", Some(&path)));
        assert!(matches!(result, RoomResult::Found { .. }), "{result:?}");
        let reply = hatchling_core::SessionResponse::Room(result);
        let written = serde_json::to_vec(&reply).unwrap().len();
        assert!(
            written <= hatchling_core::framing::MAX_FRAME_BYTES,
            "{written} bytes for what a search found of {byte:#x}"
        );
    }
}

/// Files are written and edited as the session names them: under the
/// workspace unless the path is absolute, with the text and the switch it
/// gave. What the change came to is told as the Room said it, with the
/// file as the Room was asked for it. A change is no command: it is given
/// no name.
#[test]
fn files_are_written_and_edited_as_the_session_names_them() {
    use hatchling_core::room::{Edited, Wrote};
    let fake = Fake::default();
    let mut room = session(&fake);
    fake.changes(Ok(RoomChanged::Wrote(Wrote {
        size: 3,
        replaced: false,
    })));
    assert_eq!(
        room.handle(&write("notes/a.md", "hi\n", false)),
        RoomResult::Wrote {
            path: String::from("/workspace/notes/a.md"),
            bytes: 3,
            replaced: false,
        }
    );
    fake.changes(Ok(RoomChanged::Wrote(Wrote {
        size: 5,
        replaced: true,
    })));
    assert_eq!(
        room.handle(&write("/session/b", "caf\u{e9}", true)),
        RoomResult::Wrote {
            path: String::from("/session/b"),
            bytes: 5,
            replaced: true,
        }
    );
    fake.changes(Ok(RoomChanged::Edited(Edited {
        replaced: 2,
        line: 7,
        size: 40,
    })));
    assert_eq!(
        room.handle(&edit("notes/a.md", "hi", "", true)),
        RoomResult::Edited {
            path: String::from("/workspace/notes/a.md"),
            replaced: 2,
            line: 7,
            bytes: 40,
        }
    );
    fake.changes(Ok(RoomChanged::Edited(Edited {
        replaced: 1,
        line: 1,
        size: 2,
    })));
    assert!(matches!(
        room.handle(&edit("/etc/x", "a", "b", false)),
        RoomResult::Edited { replaced: 1, .. }
    ));
    assert_eq!(
        fake.asked(),
        [
            Asked::Change(RoomChange::Write {
                path: String::from("/workspace/notes/a.md"),
                bytes: b"hi\n".to_vec(),
                replace: false,
            }),
            Asked::Change(RoomChange::Write {
                path: String::from("/session/b"),
                bytes: "caf\u{e9}".as_bytes().to_vec(),
                replace: true,
            }),
            Asked::Change(RoomChange::Edit {
                path: String::from("/workspace/notes/a.md"),
                old: b"hi".to_vec(),
                new: Vec::new(),
                all: true,
            }),
            Asked::Change(RoomChange::Edit {
                path: String::from("/etc/x"),
                old: b"a".to_vec(),
                new: b"b".to_vec(),
                all: false,
            }),
        ]
    );
    // The next command has the first name: a change took none.
    fake.answers_run(Ok(ended(0, Captured::default(), Captured::default())));
    assert!(matches!(
        room.handle(&run(&["true"])),
        RoomResult::Ended { job, .. } if job == "jabc-1"
    ));
    drop(room);
    assert_eq!(
        fake.asked().len(),
        5,
        "a change was stopped as a command is"
    );
}

/// Why no change was made is told in the Room's words; a change that may
/// not be asked for never is; one with no Room to ask, or made for a run
/// with nobody there, is said as that.
#[test]
fn a_change_that_cannot_be_made_says_why() {
    let fake = Fake::default();
    let mut room = session(&fake);
    let there = "the Room has a file at that path already, and it wasn't replaced";
    fake.changes(Err(String::from(there)));
    assert_eq!(
        room.handle(&write("notes/a.md", "x", false)),
        RoomResult::NoChange {
            reason: String::from(there)
        }
    );
    fake.changes(Err(String::from("the Room has no such file")));
    assert_eq!(
        room.handle(&edit("absent", "x", "y", false)),
        RoomResult::NoChange {
            reason: String::from("the Room has no such file")
        }
    );
    assert_eq!(fake.asked().len(), 2);
    let much = "x".repeat(hatchling_core::room::WRITE_BYTES + 1);
    let long = "y".repeat(hatchling_core::room::EDIT_BYTES + 1);
    for (op, why) in [
        (write("a\0b", "x", false), "invalid or too long"),
        (write("notes/", "x", false), "a directory"),
        (write("a", &much, true), "32768"),
        (edit("a", "", "y", false), "replaces 1 to"),
        (edit("a", &long, "y", false), "replaces 1 to"),
        (edit("a", "x", &long, false), "at most 8192"),
        (edit("a", "same", "same", true), "the text it replaces"),
    ] {
        let RoomResult::Refused { reason } = room.handle(&op) else {
            unreachable!("{op:?} was asked for");
        };
        assert!(reason.contains(why), "{reason}");
    }
    assert_eq!(fake.asked().len(), 2, "what may not be asked for was");
    for op in [write("a", "x", false), edit("a", "x", "y", false)] {
        let mut nowhere = RoomSession::over_rooms(true, None, Some(String::from("j")));
        assert_eq!(
            nowhere.handle(&op),
            RoomResult::NoChange {
                reason: String::from(NO_ROOM)
            }
        );
        let mut alone = RoomSession::over_rooms(false, Some(&fake), Some(String::from("j")));
        assert_eq!(alone.handle(&op), refused(UNATTENDED));
    }
    assert_eq!(
        fake.asked().len(),
        2,
        "a run with nobody there changed a file"
    );
    // Only a write and an edit ask for a change to a file.
    assert_eq!(change_of(&run(&["true"])), None);
    assert_eq!(change_of(&search("x", None)), None);
    assert!(matches!(
        changed(&fake, &search("x", None)),
        RoomResult::Refused { reason } if reason.contains("no change")
    ));
    assert_eq!(fake.asked().len(), 2);
}

/// What is said of a change to the file with the longest path fits one
/// frame.
#[test]
fn what_is_said_of_a_change_fits_one_frame() {
    use hatchling_core::room::{Edited, Wrote};
    let path = format!("/{}", "\u{1}".repeat(hatchling_core::room::PATH_BYTES - 1));
    for (op, made) in [
        (
            write(&path, "x", true),
            RoomChanged::Wrote(Wrote {
                size: u64::MAX,
                replaced: true,
            }),
        ),
        (
            edit(&path, "x", "y", true),
            RoomChanged::Edited(Edited {
                replaced: u32::MAX,
                line: u64::MAX,
                size: u64::MAX,
            }),
        ),
    ] {
        let fake = Fake::default();
        let mut room = session(&fake);
        fake.changes(Ok(made));
        let result = room.handle(&op);
        assert!(
            matches!(result, RoomResult::Wrote { .. } | RoomResult::Edited { .. }),
            "{result:?}"
        );
        assert!(hatchling_core::fits_one_frame(
            &hatchling_core::SessionResponse::Room(result)
        ));
    }
}

/// A file is handed over only by a session with its person there, a Room
/// to read it from, and a way to hand it over: the broker's own run's.
/// Each refusal is said as no file handed over, and the Room isn't asked.
#[test]
fn a_file_is_handed_over_only_where_it_may_be() {
    let fake = Fake::default();
    let op = RoomOp::HandOver {
        path: String::from("report.pdf"),
    };
    let mut alone = RoomSession::over_rooms(false, Some(&fake), Some(String::from("j")));
    assert_eq!(alone.handle(&op), refused(UNATTENDED));
    let mut nowhere = RoomSession::over_rooms(true, None, Some(String::from("j")));
    assert_eq!(
        nowhere.handle(&op),
        RoomResult::NotHandedOver {
            reason: String::from(NO_ROOM)
        }
    );
    let mut unhanded = RoomSession::over_rooms(true, Some(&fake), Some(String::from("j")));
    assert_eq!(
        unhanded.handle(&op),
        RoomResult::NotHandedOver {
            reason: String::from(crate::hand_over::OFF_HERE)
        }
    );
    assert!(fake.asked().is_empty(), "the Room was asked for a file");
    assert_eq!(change_of(&op), None, "a file handed over is no change");
    // A session that hands files over asks its Handing, which says what
    // it says: here, that the session is no chat's run.
    let scratch = crate::api_auth::tests::scratch("room-ops-handing");
    let mut config = crate::api_auth::tests::owner_config(None);
    config.scheduler.library = scratch.0.join("library");
    let key = hatchling_vault::MasterKey::generate().unwrap();
    let cell: hatchling_core::CellId = "cell-handing".parse().unwrap();
    let mut handing = RoomSession::over_rooms(true, Some(&fake), Some(String::from("j"))).handing(
        crate::hand_over::Handing::new(&config, &key, false, &cell, "run-1"),
    );
    assert_eq!(
        handing.handle(&op),
        RoomResult::NotHandedOver {
            reason: String::from(crate::hand_over::NOT_THE_CHAT)
        }
    );
    assert!(fake.asked().is_empty(), "the Room was asked for a file");
}
