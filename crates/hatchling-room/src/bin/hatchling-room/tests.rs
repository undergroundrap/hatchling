//! The supervisor, served over a socket pair as the spawner reaches it,
//! running real programs: each test has a directory of its own to stand
//! for the Room's session directory.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hatchling_core::framing::read_frame;
use hatchling_core::room::{
    Edited, JOBS_DIR, Kind, LIST_FOLDER_NAMES, LIST_PAGE, Listed, NoEdit, NoList, NoTake, NoWrite,
    TAKE_BYTES, Took, Wrote,
};

use super::*;

/// A supervisor at work, and the spawner's end of it.
struct Bench {
    peer: UnixStream,
    task: Option<JoinHandle<bool>>,
    dir: PathBuf,
    /// What it said that hasn't been taken yet.
    said: VecDeque<Say>,
    asked: usize,
}

impl Bench {
    fn new(name: &str) -> Self {
        Self::searching_with(name, None)
    }

    /// A supervisor whose search program is the shell script `search`,
    /// when there is one, kept in its own directory.
    fn searching_with(name: &str, search: Option<&str>) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-room-supervisor-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let mut place = Place::under(&dir);
        if let Some(script) = search {
            let program = dir.join("search-program");
            search::tests::program_at(&program, script);
            place.search = program;
        }
        let (peer, endpoint) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let task = std::thread::spawn(move || serve(&endpoint, &place).is_ok());
        assert_eq!(read_frame(&peer).unwrap(), b"ready");
        Self {
            peer,
            task: Some(task),
            dir,
            said: VecDeque::new(),
            asked: 0,
        }
    }

    fn nonce(&mut self) -> String {
        self.asked += 1;
        format!("nonce-{}", self.asked)
    }

    /// The next thing the supervisor says that `wanted` takes.
    fn next<T>(&mut self, wanted: impl Fn(&Say) -> Option<T>) -> T {
        if let Some(at) = self.said.iter().position(|said| wanted(said).is_some()) {
            return self.said.remove(at).as_ref().and_then(&wanted).unwrap();
        }
        loop {
            let said: Say = serde_json::from_slice(&read_frame(&self.peer).unwrap()).unwrap();
            if let Some(taken) = wanted(&said) {
                return taken;
            }
            self.said.push_back(said);
        }
    }

    /// Asks, and takes the answer with the ask's nonce.
    fn ask(&mut self, ask: &Ask) -> Say {
        write_frame(&self.peer, &serde_json::to_vec(ask).unwrap()).unwrap();
        let nonce = ask.nonce().to_owned();
        self.next(|said| (said.nonce() == Some(nonce.as_str())).then(|| said.clone()))
    }

    fn start_in(&mut self, job: &str, argv: &[&str], dir: &str, stdin: &[u8]) -> Say {
        let ask = Ask::Start {
            nonce: self.nonce(),
            job: job.parse().unwrap(),
            argv: argv.iter().map(|part| (*part).to_owned()).collect(),
            dir: dir.to_owned(),
            stdin: stdin.to_vec(),
        };
        self.ask(&ask)
    }

    /// Starts a job in this bench's directory, and expects it started.
    fn start(&mut self, job: &str, argv: &[&str]) {
        let dir = self.dir.to_string_lossy().into_owned();
        let said = self.start_in(job, argv, &dir, b"");
        assert!(matches!(said, Say::Started { .. }), "{said:?}");
    }

    /// Asks for a job in this bench's directory, and says why it wasn't
    /// started: `None` when it was.
    fn refused(&mut self, job: &str, argv: &[&str]) -> Option<NotStarted> {
        let dir = self.dir.to_string_lossy().into_owned();
        if let Say::NotStarted { why, .. } = self.start_in(job, argv, &dir, b"") {
            return Some(why);
        }
        None
    }

    /// Runs `script` with the shell as a job, and expects it started.
    fn shell(&mut self, job: &str, script: &str) {
        self.start(job, &["sh", "-c", script]);
    }

    fn stop(&mut self, job: &str) -> Say {
        let ask = Ask::Stop {
            nonce: self.nonce(),
            job: job.parse().unwrap(),
        };
        self.ask(&ask)
    }

    /// A kept piece, or `None` where the supervisor has no such job.
    fn piece(&mut self, job: &str, stream: Stream, end: End) -> Option<Vec<u8>> {
        let ask = Ask::Piece {
            nonce: self.nonce(),
            job: job.parse().unwrap(),
            stream,
            end,
        };
        let said = self.ask(&ask);
        if let Say::Piece { bytes, .. } = said {
            return Some(bytes);
        }
        assert!(matches!(said, Say::NoJob { .. }), "{said:?}");
        None
    }

    /// A page of `path`, or why there is none.
    fn page(&mut self, path: &Path, from: u64, lines: u32) -> std::result::Result<Page, NoPage> {
        let ask = Ask::Page {
            nonce: self.nonce(),
            path: path.to_string_lossy().into_owned(),
            from,
            lines,
        };
        let said = self.ask(&ask);
        if let Say::Page { page, .. } = said {
            return Ok(page);
        }
        assert!(matches!(said, Say::NoPage { .. }), "{said:?}");
        let Say::NoPage { why, .. } = said else {
            return Err(NoPage::Failed);
        };
        Err(why)
    }

    /// A piece of the file at the Room's `path` from byte `at`, or why
    /// none: the bench's workspace is `workspace` in its directory.
    fn take(&mut self, path: &str, at: u64) -> std::result::Result<Took, NoTake> {
        let ask = Ask::Take {
            nonce: self.nonce(),
            path: path.to_owned(),
            at,
        };
        let said = self.ask(&ask);
        if let Say::Took { took, .. } = said {
            return Ok(took);
        }
        let Say::NoTake { why, .. } = said else {
            unreachable!("{said:?} isn't what a take is answered with");
        };
        Err(why)
    }

    /// What a search of `path` for `pattern` found, or why nothing.
    fn search(&mut self, pattern: &str, path: &Path) -> std::result::Result<Found, NoSearch> {
        let ask = Ask::Search {
            nonce: self.nonce(),
            pattern: pattern.to_owned(),
            path: path.to_string_lossy().into_owned(),
            glob: Some(String::from("*.txt")),
            ignore_case: true,
            literal: false,
        };
        let said = self.ask(&ask);
        if let Say::Found { found, .. } = said {
            return Ok(found);
        }
        assert!(matches!(said, Say::NoSearch { .. }), "{said:?}");
        let Say::NoSearch { why, .. } = said else {
            return Err(NoSearch::Failed);
        };
        Err(why)
    }

    /// The end of `job`, once it's said: how, and what each stream kept.
    fn ended(&mut self, job: &str) -> (Ended, Captured, Captured) {
        self.next(|said| {
            if let Say::Ended {
                job: id,
                how,
                stdout,
                stderr,
            } = said
            {
                if id.as_str() == job {
                    return Some((*how, *stdout, *stderr));
                }
            }
            None
        })
    }

    /// The whole of a short stream of an ended job.
    fn output(&mut self, job: &str, stream: Stream) -> String {
        let mut bytes = self.piece(job, stream, End::Head).unwrap();
        bytes.extend(self.piece(job, stream, End::Tail).unwrap());
        String::from_utf8(bytes).unwrap()
    }

    fn file(&self, job: &str, kind: &str) -> PathBuf {
        self.dir.join("jobs").join(format!("{job}.{kind}"))
    }

    /// Waits for the supervisor to end, for longer than any job these
    /// tests leave running, and says whether it ended well. One that
    /// doesn't end is left behind: a test that fails must still end.
    fn ended_well(&mut self) -> bool {
        let Some(task) = self.task.take() else {
            return false;
        };
        let until = Instant::now() + Duration::from_secs(25);
        while !task.is_finished() {
            if Instant::now() > until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        task.join().unwrap_or(false)
    }

    /// Closes the spawner's end, and says whether the supervisor ended
    /// well.
    fn close(mut self) -> bool {
        let _ = self.peer.shutdown(std::net::Shutdown::Both);
        self.ended_well()
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = self.peer.shutdown(std::net::Shutdown::Both);
        let _ = self.ended_well();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Whether the process `pid` is running: there, and not waiting to be
/// reaped.
fn runs(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()))
        .is_ok_and(|stat| !stat.rsplit(") ").next().unwrap_or("Z").starts_with('Z'))
}

/// Waits until the process `pid` no longer runs.
fn gone(pid: &str) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while runs(pid) {
        if Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// In the Room its files are in the session directory, where the wire
/// says a job's output is; given another directory, they are laid out
/// the same way there.
#[test]
fn its_files_are_under_the_session_directory() {
    let room = Place::room();
    assert_eq!(room.jobs, Path::new(JOBS_DIR));
    assert_eq!(room.jobs, Path::new("/session/jobs"));
    assert_eq!(room.home, Path::new("/session/home"));
    assert_eq!(room.scratch, Path::new("/session"));
    assert_eq!(room.search, Path::new("/usr/bin/rg"));
    let job: JobId = "j".parse().unwrap();
    assert_eq!(
        room.spill(&job, Stream::Stderr),
        Path::new(&spill_path(&job, Stream::Stderr))
    );
    assert_eq!(room.input(&job), Path::new("/session/jobs/j.stdin"));
    let other = Place::under(Path::new("/elsewhere"));
    assert_eq!(other.jobs, Path::new("/elsewhere/jobs"));
    assert_eq!(other.home, Path::new("/elsewhere/home"));
    assert_eq!(other.scratch, Path::new("/elsewhere"));
}

/// A search is answered with what the search program printed for the
/// arguments this supervisor makes of the ask, and a path with nothing at
/// it is said before any program is run.
#[test]
fn a_search_is_answered_with_what_the_search_program_found() {
    // The stand-in prints each argument it was given, as a line found.
    let mut bench =
        Bench::searching_with("search", Some("for a; do printf '/w:1:%s\\n' \"$a\"; done"));
    let here = bench.dir.clone();
    let found = bench.search("to do", &here).unwrap();
    let text = String::from_utf8(found.bytes).unwrap();
    let told: Vec<&str> = text.lines().collect();
    let tail: Vec<&str> = told.iter().rev().take(5).rev().copied().collect();
    assert_eq!(
        tail,
        [
            "/w:1:--ignore-case",
            "/w:1:--glob=*.txt",
            "/w:1:--regexp=to do",
            "/w:1:--",
            format!("/w:1:{}", here.display()).as_str(),
        ]
    );
    assert_eq!(told.first().copied(), Some("/w:1:--no-config"));
    assert!(!found.more);
    assert_eq!(
        bench.search("to do", &here.join("no-such-place")),
        Err(NoSearch::NoSuchPath)
    );
    assert!(bench.close());
}

/// A search the program refuses, and one whose program isn't there, are
/// each said as they are; and the supervisor goes on answering.
#[test]
fn a_search_that_fails_is_said_and_the_supervisor_goes_on() {
    let mut refusing = Bench::searching_with("search-refused", Some("exit 2"));
    let here = refusing.dir.clone();
    assert_eq!(refusing.search("(", &here), Err(NoSearch::Refused));
    assert_eq!(refusing.search("(", &here), Err(NoSearch::Refused));
    assert!(refusing.close());
    // The Room's own program isn't on a machine that isn't a Room's
    // image: where it isn't, the search can't be run, and that is said.
    let mut bare = Bench::new("search-bare");
    let here = bare.dir.clone();
    if !Path::new(search::SEARCH_PROGRAM).exists() {
        assert_eq!(bare.search("x", &here), Err(NoSearch::Failed));
    }
    assert!(bare.close());
}

/// A job started before a search goes on running through it, and its end
/// and what it wrote are said after the search is answered.
#[test]
fn a_job_runs_on_through_a_search() {
    let mut bench = Bench::searching_with("search-job", Some("sleep 1; printf '/w:1:x\\n'"));
    let here = bench.dir.clone();
    bench.shell("j1", "echo started; sleep 0.2; echo done");
    let found = bench.search("x", &here).unwrap();
    assert_eq!(found.bytes, b"/w:1:x\n");
    let (how, _, _) = bench.ended("j1");
    assert_eq!(how, Ended::Exited(0));
    assert_eq!(bench.output("j1", Stream::Stdout), "started\ndone\n");
    assert!(bench.close());
}

/// A file is written and edited by the supervisor itself, over its
/// socket: what it says is what is in the file, a page of it reads back
/// what was written, and what can't be done is said and changes nothing.
#[test]
fn a_file_is_written_and_edited_and_read_back() {
    let mut bench = Bench::new("write");
    let path = bench
        .dir
        .join("notes/todo.md")
        .to_string_lossy()
        .into_owned();
    let write = |bench: &mut Bench, bytes: &[u8], replace: bool| {
        let ask = Ask::Write {
            nonce: bench.nonce(),
            path: path.clone(),
            bytes: bytes.to_vec(),
            replace,
        };
        bench.ask(&ask)
    };
    let edit = |bench: &mut Bench, old: &str, new: &str, all: bool| {
        let ask = Ask::Edit {
            nonce: bench.nonce(),
            path: path.clone(),
            old: old.as_bytes().to_vec(),
            new: new.as_bytes().to_vec(),
            all,
        };
        bench.ask(&ask)
    };
    let said = write(&mut bench, b"TODO: buy milk\nTODO: call home\n", false);
    assert!(
        matches!(said, Say::Wrote { wrote, .. } if wrote == Wrote { size: 31, replaced: false }),
        "{said:?}"
    );
    let said = write(&mut bench, b"again\n", false);
    assert!(
        matches!(
            said,
            Say::NoWrite {
                why: NoWrite::Exists,
                ..
            }
        ),
        "{said:?}"
    );
    let said = edit(&mut bench, "TODO", "DONE", false);
    assert!(
        matches!(&said, Say::NoEdit { why: NoEdit::Ambiguous { places: 2, lines }, .. } if *lines == [1, 2]),
        "{said:?}"
    );
    let said = edit(&mut bench, "TODO: buy milk", "DONE: bought milk", false);
    assert!(
        matches!(said, Say::Edited { edited, .. } if edited == Edited { replaced: 1, line: 1, size: 34 }),
        "{said:?}"
    );
    let said = edit(&mut bench, "nowhere", "x", true);
    assert!(
        matches!(
            said,
            Say::NoEdit {
                why: NoEdit::NotFound,
                ..
            }
        ),
        "{said:?}"
    );
    let ask = Ask::Page {
        nonce: bench.nonce(),
        path: path.clone(),
        from: 1,
        lines: 10,
    };
    let said = bench.ask(&ask);
    assert!(
        matches!(&said, Say::Page { page, .. } if page.bytes == b"DONE: bought milk\nTODO: call home\n"),
        "{said:?}"
    );
    let said = write(&mut bench, b"all new\n", true);
    assert!(
        matches!(said, Say::Wrote { wrote, .. } if wrote == Wrote { size: 8, replaced: true }),
        "{said:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"all new\n");
    assert!(bench.close());
}

/// With nothing running and nothing asked, it waits for as long as it
/// takes the spawner to say something.
#[test]
fn it_waits_until_there_is_something_to_do() {
    let place = Place::under(Path::new("/nonexistent"));
    let room = Room {
        place: &place,
        running: BTreeMap::new(),
        kept: VecDeque::new(),
    };
    let (peer, stream) = UnixStream::pair().unwrap();
    let began = Instant::now();
    let late = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        write_frame(&peer, b"late").unwrap();
        peer
    });
    let (spawner, jobs) = room.wait(&stream).unwrap();
    assert!(spawner, "it gave up waiting with nothing to do");
    assert!(jobs.is_empty());
    assert!(began.elapsed() >= Duration::from_millis(300));
    drop(late.join().unwrap());
}

#[test]
fn it_says_it_is_ready_and_ends_well_when_the_spawner_closes() {
    let bench = Bench::new("ready");
    assert!(bench.close());
}

#[test]
fn a_job_runs_and_its_end_and_what_it_wrote_are_said() {
    let mut bench = Bench::new("runs");
    bench.shell("first", "echo out; echo err >&2; exit 7");
    let (how, stdout, stderr) = bench.ended("first");
    assert_eq!(how, Ended::Exited(7));
    let four = Captured {
        head: 4,
        spilled: 4,
        ..Captured::default()
    };
    assert_eq!((stdout, stderr), (four, four));
    stdout.check().unwrap();
    assert_eq!(bench.output("first", Stream::Stdout), "out\n");
    assert_eq!(bench.output("first", Stream::Stderr), "err\n");
    assert_eq!(
        std::fs::read(bench.file("first", "stdout")).unwrap(),
        b"out\n"
    );
    assert_eq!(
        std::fs::read(bench.file("first", "stderr")).unwrap(),
        b"err\n"
    );
    // One job after another, each with its own end.
    bench.start("second", &["true"]);
    assert_eq!(bench.ended("second").0, Ended::Exited(0));
    bench.shell("third", "kill -TERM $$");
    assert_eq!(bench.ended("third").0, Ended::Signaled(15));
    assert!(bench.close());
}

#[test]
fn a_job_is_given_its_directory_a_fixed_environment_and_its_input() {
    let mut bench = Bench::new("given");
    let dir = bench.dir.to_string_lossy().into_owned();
    bench.start("where", &["pwd"]);
    bench.ended("where");
    assert_eq!(bench.output("where", Stream::Stdout), format!("{dir}\n"));
    // Nothing of the supervisor's own environment reaches a job.
    bench.start("env", &["/usr/bin/env"]);
    bench.ended("env");
    assert_eq!(
        bench.output("env", Stream::Stdout),
        format!("HOME={dir}/home\nLANG=C.UTF-8\nPATH=/usr/local/bin:/usr/bin:/bin\nTMPDIR={dir}\n")
    );
    assert!(bench.dir.join("home").is_dir());
    // Its input is what it was given, and the file it came from goes
    // when the job has ended.
    let said = bench.start_in("fed", &["cat"], &dir, b"fed\xff\n");
    assert!(matches!(said, Say::Started { .. }), "{said:?}");
    let (how, stdout, _) = bench.ended("fed");
    assert_eq!((how, stdout.head), (Ended::Exited(0), 5));
    assert_eq!(
        bench.piece("fed", Stream::Stdout, End::Head).unwrap(),
        b"fed\xff\n"
    );
    assert!(!bench.file("fed", "stdin").exists());
    // With no input, it reads the end of nothing.
    bench.start("unfed", &["cat"]);
    let (how, stdout, _) = bench.ended("unfed");
    assert_eq!((how, stdout), (Ended::Exited(0), Captured::default()));
    assert!(bench.close());
}

#[test]
fn a_long_stream_keeps_its_head_its_tail_and_the_count_between() {
    let mut bench = Bench::new("long");
    let mut whole = String::new();
    for line in 1..=20_000 {
        whole.push_str(&line.to_string());
        whole.push('\n');
    }
    let whole = whole.as_bytes();
    bench.start("seq", &["seq", "1", "20000"]);
    let (how, stdout, stderr) = bench.ended("seq");
    assert_eq!(how, Ended::Exited(0));
    stdout.check().unwrap();
    let piece = u32::try_from(PIECE_BYTES).unwrap();
    let length = u64::try_from(whole.len()).unwrap();
    assert_eq!(
        stdout,
        Captured {
            head: piece,
            tail: piece,
            dropped: length - 2 * u64::from(piece),
            spilled: length,
            spill_cut: false,
        }
    );
    assert_eq!(stderr, Captured::default());
    assert_eq!(
        bench.piece("seq", Stream::Stdout, End::Head).unwrap(),
        whole.get(..PIECE_BYTES).unwrap()
    );
    assert_eq!(
        bench.piece("seq", Stream::Stdout, End::Tail).unwrap(),
        whole.get(whole.len() - PIECE_BYTES..).unwrap()
    );
    assert_eq!(std::fs::read(bench.file("seq", "stdout")).unwrap(), whole);
    // One byte past a full head is a tail of one byte, and nothing lost.
    let count = (PIECE_BYTES + 1).to_string();
    bench.start("edge", &["head", "-c", &count, "/dev/zero"]);
    let (_, stdout, _) = bench.ended("edge");
    assert_eq!((stdout.head, stdout.tail, stdout.dropped), (piece, 1, 0));
    stdout.check().unwrap();
    assert!(bench.close());
}

#[test]
fn a_streams_file_stops_at_its_cap_and_says_so() {
    let mut bench = Bench::new("cap");
    let over = SPILL_BYTES + 1000;
    bench.start("big", &["head", "-c", &over.to_string(), "/dev/zero"]);
    let (how, stdout, _) = bench.ended("big");
    assert_eq!(how, Ended::Exited(0));
    stdout.check().unwrap();
    assert_eq!(stdout.total(), over);
    assert_eq!((stdout.spilled, stdout.spill_cut), (SPILL_BYTES, true));
    assert_eq!(
        std::fs::metadata(bench.file("big", "stdout"))
            .unwrap()
            .len(),
        SPILL_BYTES
    );
    // Exactly the cap is whole.
    bench.start(
        "exact",
        &["head", "-c", &SPILL_BYTES.to_string(), "/dev/zero"],
    );
    let (_, stdout, _) = bench.ended("exact");
    assert_eq!((stdout.spilled, stdout.spill_cut), (SPILL_BYTES, false));
    assert!(bench.close());
}

#[test]
fn only_so_many_jobs_run_at_once() {
    let mut bench = Bench::new("slots");
    let names = ["a", "b", "c", "d"];
    assert_eq!(names.len(), RUNNING_MAX);
    for name in names {
        bench.start(name, &["sleep", "20"]);
    }
    assert_eq!(bench.refused("e", &["true"]), Some(NotStarted::Full));
    // A piece of a job that still runs isn't there to give.
    assert_eq!(bench.piece("a", Stream::Stdout, End::Head), None);
    for name in names {
        assert!(matches!(bench.stop(name), Say::Stopping { .. }));
        assert_eq!(bench.ended(name).0, Ended::Signaled(9));
    }
    // With room again, the fifth name starts.
    bench.start("e", &["true"]);
    assert_eq!(bench.ended("e").0, Ended::Exited(0));
    assert!(bench.close());
}

#[test]
fn a_name_is_used_once_for_as_long_as_its_job_is_kept() {
    let mut bench = Bench::new("names");
    bench.start("a", &["sleep", "20"]);
    assert_eq!(bench.refused("a", &["true"]), Some(NotStarted::Known));
    assert!(matches!(bench.stop("a"), Say::Stopping { .. }));
    assert_eq!(bench.ended("a").0, Ended::Signaled(9));
    // An ended job keeps its name, and can't be stopped again.
    assert_eq!(bench.refused("a", &["true"]), Some(NotStarted::Known));
    assert!(matches!(bench.stop("a"), Say::NoJob { .. }));
    assert!(matches!(bench.stop("never"), Say::NoJob { .. }));
    assert!(bench.file("a", "stdout").exists());
    assert!(bench.close());
}

#[test]
fn a_job_that_cannot_start_says_why_and_leaves_nothing() {
    let mut bench = Bench::new("refused");
    let dir = bench.dir.to_string_lossy().into_owned();
    let cases: [(&[&str], &str, NotStarted); 4] = [
        (&["/nonexistent/program"], &dir, NotStarted::NoSuchProgram),
        (
            &["no-such-program-anywhere"],
            &dir,
            NotStarted::NoSuchProgram,
        ),
        (&["true"], "/nonexistent/dir", NotStarted::NoSuchDir),
        (&["true"], "/etc/hostname", NotStarted::NoSuchDir),
    ];
    for (argv, dir, why) in cases {
        let said = bench.start_in("refused", argv, dir, b"input");
        assert!(
            matches!(&said, Say::NotStarted { why: said, .. } if *said == why),
            "{argv:?} in {dir}: {said:?}"
        );
        for kind in ["stdout", "stderr", "stdin"] {
            assert!(!bench.file("refused", kind).exists(), "{kind} was left");
        }
    }
    // The name is free: nothing was started under it.
    bench.start("refused", &["true"]);
    assert_eq!(bench.ended("refused").0, Ended::Exited(0));
    assert!(bench.close());
}

/// A session directory that can't hold a job's files still runs the job:
/// what it writes is kept in memory, and its file is said to be cut.
/// What it is given to read must be whole, or it isn't started.
#[test]
fn a_job_runs_where_its_output_files_cannot_be_kept() {
    let mut bench = Bench::new("fileless");
    // A file where the jobs' directory would be: nothing can be made in it.
    std::fs::write(bench.dir.join("jobs"), b"in the way").unwrap();
    bench.shell("said", "echo kept");
    let (how, stdout, stderr) = bench.ended("said");
    assert_eq!(how, Ended::Exited(0));
    stdout.check().unwrap();
    assert_eq!(
        (stdout.head, stdout.spilled, stdout.spill_cut),
        (5, 0, true)
    );
    assert_eq!(stderr, Captured::default());
    assert_eq!(bench.output("said", Stream::Stdout), "kept\n");
    let dir = bench.dir.to_string_lossy().into_owned();
    let unfed = bench.start_in("fed", &["cat"], &dir, b"input");
    assert!(
        matches!(
            unfed,
            Say::NotStarted {
                why: NotStarted::NoSession,
                ..
            }
        ),
        "{unfed:?}"
    );
    assert!(bench.close());
}

#[test]
fn a_stop_kills_everything_in_the_jobs_group() {
    let mut bench = Bench::new("stop");
    bench.shell("tree", "sleep 20 & echo $! > child; echo $$ > parent; wait");
    let read = |name: &str| {
        let path = bench.dir.join(name);
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let pid = std::fs::read_to_string(&path).unwrap_or_default();
            if pid.ends_with('\n') || Instant::now() > until {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let (parent, child) = (read("parent"), read("child"));
    assert!(runs(&parent) && runs(&child), "the job isn't running");
    assert!(matches!(bench.stop("tree"), Say::Stopping { .. }));
    assert_eq!(bench.ended("tree").0, Ended::Signaled(9));
    assert!(gone(&parent), "the job's first process outlived its stop");
    assert!(gone(&child), "the job's child outlived its stop");
    assert!(bench.close());
}

#[test]
fn what_a_job_leaves_running_in_its_group_ends_with_it() {
    let mut bench = Bench::new("left");
    let began = Instant::now();
    bench.shell("leaver", "sleep 20 & echo $!");
    let (how, stdout, _) = bench.ended("leaver");
    assert_eq!(how, Ended::Exited(0));
    assert!(stdout.head > 1);
    let child = bench.output("leaver", Stream::Stdout);
    assert!(gone(&child), "what the job left running outlived it");
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "the job's end waited for what it left"
    );
    assert!(bench.close());
}

#[test]
fn a_pipe_held_outside_the_jobs_group_does_not_hold_its_end() {
    let mut bench = Bench::new("held");
    let began = Instant::now();
    // In a session of its own, the sleeper isn't in the job's group: it
    // lives on, holding the job's output open. The job ends once the
    // sleeper has left its session (the sixth field of a process's stat).
    let session = "cut -d' ' -f6 /proc/$1/stat";
    bench.shell(
        "holder",
        &format!(
            "sid() {{ {session}; }}; setsid sleep 20 & while [ \"$(sid $!)\" = \"$(sid $$)\" ]; do :; done; echo said"
        ),
    );
    let (how, stdout, _) = bench.ended("holder");
    assert_eq!(how, Ended::Exited(0));
    assert_eq!(stdout.head, 5);
    assert_eq!(bench.output("holder", Stream::Stdout), "said\n");
    let took = began.elapsed();
    assert!(
        took >= DRAIN,
        "its pipes weren't read for as long as they're given: {took:?}"
    );
    assert!(
        took < Duration::from_secs(10),
        "the job's end waited on a pipe held elsewhere: {took:?}"
    );
    assert!(bench.close());
}

#[test]
fn only_so_many_ended_jobs_are_kept() {
    let mut bench = Bench::new("kept");
    for number in 0..=KEPT_MAX {
        let name = format!("job-{number}");
        bench.shell(&name, "echo kept");
        assert_eq!(bench.ended(&name).0, Ended::Exited(0));
    }
    // The oldest went, with its files; the next oldest is still there.
    assert_eq!(bench.piece("job-0", Stream::Stdout, End::Head), None);
    assert!(!bench.file("job-0", "stdout").exists());
    assert_eq!(
        bench.piece("job-1", Stream::Stdout, End::Head).unwrap(),
        b"kept\n"
    );
    assert!(bench.file("job-1", "stdout").exists());
    assert!(!bench.file("job-0", "stderr").exists());
    // The name that went is free again.
    bench.start("job-0", &["true"]);
    assert_eq!(bench.ended("job-0").0, Ended::Exited(0));
    assert!(bench.close());
}

#[test]
fn it_ends_its_jobs_when_the_spawner_goes_or_says_what_it_cannot_read() {
    let cases: [(&str, Option<&[u8]>); 2] =
        [("closed", None), ("garbage", Some(b"{\"ask\":\"run\"}"))];
    for (name, garbage) in cases {
        let mut bench = Bench::new(name);
        bench.shell("runner", "echo $$ > pid; exec sleep 20");
        let path = bench.dir.join("pid");
        let until = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            let pid = std::fs::read_to_string(&path).unwrap_or_default();
            if pid.ends_with('\n') || Instant::now() > until {
                break pid;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(runs(&pid), "the job isn't running");
        if let Some(garbage) = garbage {
            write_frame(&bench.peer, garbage).unwrap();
        }
        // A clean close ends it well; what it can't read ends it badly.
        assert_eq!(bench.close(), garbage.is_none(), "{name}");
        assert!(gone(&pid), "{name}: a job outlived its supervisor");
    }
}

#[test]
fn a_stream_is_kept_piece_by_piece() {
    // A capture fed by hand: a file stands for the pipe, and isn't read.
    let dir = std::env::temp_dir().join(format!("hatchling-room-capture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    let spill = dir.join("spill");
    let mut capture = Capture::new(
        File::open("/dev/null").unwrap(),
        Some(private(&spill).unwrap()),
    );
    assert_eq!(capture.captured(), Captured::default());
    capture.take(&vec![1; PIECE_BYTES - 1]);
    capture.take(&[2, 3]);
    assert_eq!(capture.head.len(), PIECE_BYTES);
    assert_eq!(capture.head.last(), Some(&2));
    assert_eq!(capture.tail, [3]);
    capture.take(&vec![4; PIECE_BYTES]);
    assert_eq!(capture.tail.len(), PIECE_BYTES);
    assert_eq!(
        capture.tail.front(),
        Some(&4),
        "the oldest of the tail stayed"
    );
    assert_eq!(capture.dropped, 1);
    let kept = capture.captured();
    kept.check().unwrap();
    let length = u64::try_from(2 * PIECE_BYTES + 1).unwrap();
    assert_eq!(
        (kept.total(), kept.spilled, kept.spill_cut),
        (length, length, false)
    );
    assert_eq!(std::fs::metadata(&spill).unwrap().len(), length);
    // A file that can't be written is left as it was, and said cut.
    let mut capture = Capture::new(
        File::open("/dev/null").unwrap(),
        Some(File::open("/dev/null").unwrap()),
    );
    capture.take(b"lost");
    capture.take(b"too");
    let kept = capture.captured();
    kept.check().unwrap();
    assert_eq!((kept.head, kept.spilled, kept.spill_cut), (7, 0, true));
    // With no file at all, the same; and nothing written is whole.
    let mut capture = Capture::new(File::open("/dev/null").unwrap(), None);
    assert_eq!(capture.captured(), Captured::default());
    capture.take(b"kept");
    let kept = capture.captured();
    kept.check().unwrap();
    assert_eq!((kept.head, kept.spilled, kept.spill_cut), (4, 0, true));
    // The reading end: bytes, then the end of the pipe closes it.
    let fed = dir.join("fed");
    std::fs::write(&fed, b"abc").unwrap();
    let mut capture = Capture::new(File::open(&fed).unwrap(), Some(private(&spill).unwrap()));
    capture.read().unwrap();
    assert_eq!(
        (capture.head.as_slice(), capture.pipe.is_some()),
        (&b"abc"[..], true)
    );
    capture.read().unwrap();
    assert!(capture.pipe.is_none());
    capture.read().unwrap();
    assert_eq!(capture.captured().head, 3);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What `paged` makes of `text`, read `chunk` bytes at a time.
fn pages(
    text: &[u8],
    chunk: usize,
    from: u64,
    lines: u32,
    scan: u64,
) -> std::result::Result<Page, NoPage> {
    struct Slow<'a>(&'a [u8], usize);
    impl io::Read for Slow<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.0.len().min(self.1).min(buffer.len());
            let (now, later) = self.0.split_at(count);
            buffer.get_mut(..count).unwrap().copy_from_slice(now);
            self.0 = later;
            Ok(count)
        }
    }
    paged(Slow(text, chunk), from, lines, scan)
}

/// A page of `bytes`, with more after it or none, that isn't cut.
fn lines_of(bytes: &[u8], more: bool) -> Page {
    Page {
        bytes: bytes.to_vec(),
        more,
        cut: false,
        size: 0,
    }
}

#[test]
fn a_page_is_whole_lines_from_the_one_asked_for() {
    let text = b"one\ntwo\nthree\nfour\n";
    // However it comes off the disk: all at once, or a byte at a time.
    for chunk in [1, 2, 3, 5, 64] {
        let page = |from, lines| pages(text, chunk, from, lines, u64::MAX);
        assert_eq!(page(1, 4), Ok(lines_of(text, false)), "{chunk}");
        assert_eq!(page(1, 9), Ok(lines_of(text, false)), "{chunk}");
        assert_eq!(page(1, 1), Ok(lines_of(b"one\n", true)), "{chunk}");
        assert_eq!(page(2, 2), Ok(lines_of(b"two\nthree\n", true)), "{chunk}");
        assert_eq!(page(3, 2), Ok(lines_of(b"three\nfour\n", false)), "{chunk}");
        assert_eq!(page(4, 1), Ok(lines_of(b"four\n", false)), "{chunk}");
        // The first line past the last isn't an empty page.
        assert_eq!(page(5, 1), Err(NoPage::PastEnd(4)), "{chunk}");
        assert_eq!(page(90, 1), Err(NoPage::PastEnd(4)), "{chunk}");
    }
    // A last line with no line end is a line.
    for chunk in [1, 4, 64] {
        let page = |from, lines| pages(b"one\ntwo", chunk, from, lines, u64::MAX);
        assert_eq!(page(1, 2), Ok(lines_of(b"one\ntwo", false)));
        assert_eq!(page(2, 1), Ok(lines_of(b"two", false)));
        assert_eq!(page(2, 5), Ok(lines_of(b"two", false)));
        assert_eq!(page(3, 1), Err(NoPage::PastEnd(2)));
        assert_eq!(page(1, 1), Ok(lines_of(b"one\n", true)));
    }
    // Lines with nothing in them are lines.
    assert_eq!(
        pages(b"\n\n\n", 1, 2, 1, u64::MAX),
        Ok(lines_of(b"\n", true))
    );
    assert_eq!(
        pages(b"\n\n\n", 2, 3, 5, u64::MAX),
        Ok(lines_of(b"\n", false))
    );
    assert_eq!(pages(b"\n\n\n", 2, 4, 5, u64::MAX), Err(NoPage::PastEnd(3)));
    // A file with nothing in it has a first page with nothing in it.
    assert_eq!(pages(b"", 8, 1, 5, u64::MAX), Ok(lines_of(b"", false)));
    assert_eq!(pages(b"", 8, 2, 5, u64::MAX), Err(NoPage::PastEnd(0)));
    // Line 0 is read as the first, and no lines as none.
    assert_eq!(pages(b"", 8, 0, 5, u64::MAX), Ok(lines_of(b"", false)));
    assert_eq!(
        pages(b"a\nb\n", 8, 0, 1, u64::MAX),
        Ok(lines_of(b"a\n", true))
    );
    assert_eq!(pages(b"a\nb\n", 8, 1, 0, u64::MAX), Ok(lines_of(b"", true)));
}

#[test]
fn a_page_stops_at_the_last_whole_line_that_fits() {
    let line = |fill: u8, length: usize| {
        let mut line = vec![fill; length - 1];
        line.push(b'\n');
        line
    };
    // Two lines that fill a page to the byte, and a third.
    let half = PAGE_BYTES / 2;
    let text = [line(b'a', half), line(b'b', half), line(b'c', 10)].concat();
    for chunk in [1000, 64 * 1024] {
        let page = pages(&text, chunk, 1, 9, u64::MAX).unwrap();
        assert_eq!(page.bytes, text.get(..PAGE_BYTES).unwrap(), "{chunk}");
        assert!(page.more && !page.cut);
        let next = pages(&text, chunk, 3, 9, u64::MAX).unwrap();
        assert_eq!(next.bytes, line(b'c', 10));
        assert!(!next.more && !next.cut);
    }
    // One byte more in the second line, and it waits for the next page.
    let text = [line(b'a', half), line(b'b', half + 1), line(b'c', 10)].concat();
    for chunk in [1000, 64 * 1024] {
        let page = pages(&text, chunk, 1, 9, u64::MAX).unwrap();
        assert_eq!(page.bytes, line(b'a', half), "{chunk}");
        assert!(page.more && !page.cut);
        let next = pages(&text, chunk, 2, 9, u64::MAX).unwrap();
        assert_eq!(next.bytes, text.get(half..).unwrap());
        assert!(!next.more && !next.cut);
    }
    // A page of exactly a page's lines, with nothing after it.
    let text = [line(b'a', half), line(b'b', half)].concat();
    let page = pages(&text, 1000, 1, 9, u64::MAX).unwrap();
    assert_eq!(page.bytes, text);
    assert!(!page.more && !page.cut);
}

#[test]
fn a_first_line_too_long_for_a_page_is_cut_and_said_to_be() {
    let long = [vec![b'x'; PAGE_BYTES + 5], b"\nnext\n".to_vec()].concat();
    for chunk in [1000, PAGE_BYTES, 64 * 1024] {
        let page = pages(&long, chunk, 1, 9, u64::MAX).unwrap();
        assert_eq!(page.bytes, vec![b'x'; PAGE_BYTES], "{chunk}");
        assert!(page.more && page.cut);
        assert!(page.check().is_ok());
        // The line after it is found past all of it.
        let next = pages(&long, chunk, 2, 9, u64::MAX).unwrap();
        assert_eq!(next.bytes, b"next\n");
        assert!(!next.more && !next.cut);
    }
    // A line as long as a page, with its line end or at the file's end,
    // is whole.
    let exact = [vec![b'x'; PAGE_BYTES - 1], b"\n".to_vec()].concat();
    let page = pages(&exact, 1000, 1, 9, u64::MAX).unwrap();
    assert_eq!(page.bytes, exact);
    assert!(!page.more && !page.cut);
    let bare = vec![b'x'; PAGE_BYTES];
    let page = pages(&bare, 1000, 1, 9, u64::MAX).unwrap();
    assert_eq!(page.bytes, bare);
    assert!(!page.more && !page.cut);
    // One byte longer, and it is cut.
    let over = vec![b'x'; PAGE_BYTES + 1];
    let page = pages(&over, 1000, 1, 9, u64::MAX).unwrap();
    assert_eq!(page.bytes, bare);
    assert!(page.more && page.cut);
    // A long line after a short one waits for its own page.
    let after = [b"short\n".to_vec(), over].concat();
    let page = pages(&after, 1000, 1, 9, u64::MAX).unwrap();
    assert_eq!(page.bytes, b"short\n");
    assert!(page.more && !page.cut);
}

#[test]
fn a_page_is_looked_for_only_so_far_into_a_file() {
    let text = b"aa\nbb\ncc\ndd\n";
    // Within what is read, a page is as it would be.
    assert_eq!(pages(text, 4, 1, 2, 12), Ok(lines_of(b"aa\nbb\n", true)));
    assert_eq!(pages(text, 4, 4, 1, 12), Ok(lines_of(b"dd\n", false)));
    // Whole lines found by the end of what is read are the page, and
    // whether the file goes on is looked at past it.
    assert_eq!(pages(text, 4, 1, 9, 6), Ok(lines_of(b"aa\nbb\n", true)));
    assert_eq!(pages(text, 4, 1, 9, 7), Ok(lines_of(b"aa\nbb\n", true)));
    assert_eq!(pages(text, 4, 1, 9, 12), Ok(lines_of(text, false)));
    assert_eq!(pages(text, 4, 4, 9, 12), Ok(lines_of(b"dd\n", false)));
    // A line that isn't whole by then isn't one, and a page with no
    // whole line isn't a page.
    assert_eq!(pages(text, 4, 1, 9, 2), Err(NoPage::TooFar));
    assert_eq!(pages(text, 4, 3, 1, 8), Err(NoPage::TooFar));
    assert_eq!(pages(text, 4, 3, 1, 6), Err(NoPage::TooFar));
    assert_eq!(pages(text, 4, 4, 1, 9), Err(NoPage::TooFar));
    // Nothing at all may be read, of a file that has something in it.
    assert_eq!(pages(text, 4, 1, 1, 0), Err(NoPage::TooFar));
    assert_eq!(pages(b"", 4, 1, 1, 0), Ok(lines_of(b"", false)));
    // A file that ends where the look does is read to its end: its last
    // line is whole, with a line end or none, and a line past it is past
    // its end.
    assert_eq!(pages(b"aa\nbb", 4, 1, 9, 5), Ok(lines_of(b"aa\nbb", false)));
    assert_eq!(pages(b"aa\nbb", 4, 2, 9, 5), Ok(lines_of(b"bb", false)));
    assert_eq!(pages(b"aa\nbb", 4, 1, 1, 5), Ok(lines_of(b"aa\n", true)));
    assert_eq!(pages(b"aa\n", 4, 2, 9, 3), Err(NoPage::PastEnd(1)));
    // One byte longer, and the line that isn't whole by then isn't one.
    assert_eq!(pages(b"aa\nbbb", 4, 1, 9, 5), Ok(lines_of(b"aa\n", true)));
    assert_eq!(pages(b"aa\nbbb", 4, 2, 9, 5), Err(NoPage::TooFar));
}

#[test]
fn a_page_with_a_byte_no_text_has_is_not_one() {
    assert_eq!(pages(b"a\0b\n", 8, 1, 1, u64::MAX), Err(NoPage::NotText));
    // Only the lines asked for are looked at.
    let text = b"text\nbin\0ary\nmore\n";
    assert_eq!(
        pages(text, 8, 1, 1, u64::MAX),
        Ok(lines_of(b"text\n", true))
    );
    assert_eq!(pages(text, 8, 2, 1, u64::MAX), Err(NoPage::NotText));
    assert_eq!(
        pages(text, 8, 3, 1, u64::MAX),
        Ok(lines_of(b"more\n", false))
    );
    // Bytes that aren't UTF-8 are handed back as they are.
    assert_eq!(
        pages(b"caf\xe9\n", 8, 1, 1, u64::MAX),
        Ok(lines_of(b"caf\xe9\n", false))
    );
}

#[test]
fn a_read_that_fails_is_not_the_end_of_the_file() {
    struct Broken(u8);
    impl io::Read for Broken {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.0 += 1;
            match self.0 {
                1 => Err(io::ErrorKind::Interrupted.into()),
                2 => {
                    buffer.get_mut(..2).unwrap().copy_from_slice(b"a\n");
                    Ok(2)
                }
                _ => Err(io::Error::other("the disk went")),
            }
        }
    }
    // What interrupts a read is read again; what fails it fails the page.
    assert_eq!(paged(Broken(0), 1, 1, u64::MAX), Err(NoPage::Failed));
    assert_eq!(paged(Broken(0), 2, 1, u64::MAX), Err(NoPage::Failed));
    let mut two = [0_u8; 2];
    assert_eq!(read_some(&mut Broken(0), &mut two), Ok(2));
    assert_eq!(&two, b"a\n");
}

#[test]
fn the_supervisor_hands_back_a_page_of_a_file_and_says_why_not() {
    use std::os::unix::fs::PermissionsExt as _;

    let mut bench = Bench::new("page");
    let notes = bench.dir.join("notes.txt");
    std::fs::write(&notes, b"one\ntwo\nthree\n").unwrap();
    let page = bench.page(&notes, 2, 1).unwrap();
    assert_eq!(page.bytes, b"two\n");
    assert!(page.more && !page.cut);
    assert_eq!(page.size, 14);
    assert!(page.check().is_ok());
    let last = bench.page(&notes, 3, 50).unwrap();
    assert_eq!(last.bytes, b"three\n");
    assert!(!last.more);
    assert_eq!(bench.page(&notes, 4, 1), Err(NoPage::PastEnd(3)));
    // What isn't there, and what is in the way of it.
    assert_eq!(
        bench.page(&bench.dir.join("none.txt"), 1, 1),
        Err(NoPage::NoSuchFile)
    );
    assert_eq!(
        bench.page(&notes.join("under"), 1, 1),
        Err(NoPage::NoSuchFile)
    );
    // What isn't a file: a directory, and a pipe nobody writes to, which
    // is not waited for.
    let dir = bench.dir.clone();
    assert_eq!(bench.page(&dir, 1, 1), Err(NoPage::NotAFile));
    let pipe = bench.dir.join("pipe");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &pipe,
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    assert_eq!(bench.page(&pipe, 1, 1), Err(NoPage::NotAFile));
    // What can't be opened for some other reason.
    let looped = bench.dir.join("loop");
    std::os::unix::fs::symlink(&looped, &looped).unwrap();
    assert_eq!(bench.page(&looped, 1, 1), Err(NoPage::Failed));
    // A file its user may not read, where there is such a thing: root
    // reads what it likes.
    let closed = bench.dir.join("closed.txt");
    std::fs::write(&closed, b"kept\n").unwrap();
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    if rustix::process::geteuid().is_root() {
        assert!(bench.page(&closed, 1, 1).is_ok());
    } else {
        assert_eq!(bench.page(&closed, 1, 1), Err(NoPage::Denied));
    }
    // A file with no text in it.
    let binary = bench.dir.join("binary");
    std::fs::write(&binary, [0_u8, 1, 2, b'\n']).unwrap();
    assert_eq!(bench.page(&binary, 1, 1), Err(NoPage::NotText));
    // A page starts no job, and is still answered while one runs.
    bench.shell("sleeper", "sleep 30");
    assert_eq!(bench.page(&notes, 1, 1).unwrap().bytes, b"one\n");
    assert!(matches!(bench.stop("sleeper"), Say::Stopping { .. }));
    bench.ended("sleeper");
}

/// A bench with a workspace of its own, made, and the Room's path for
/// `name` in it.
fn workspace(bench: &Bench) -> PathBuf {
    let workspace = bench.dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    workspace
}

/// The Room's path for `name` in the workspace.
fn in_room(name: &str) -> String {
    format!("{WORKSPACE}/{name}")
}

#[test]
fn a_file_is_handed_back_whole_piece_by_piece() {
    let mut bench = Bench::new("take");
    let workspace = workspace(&bench);
    let whole: Vec<u8> = (0..2 * TAKE_BYTES + 5)
        .map(|at| u8::try_from(at % 251).unwrap())
        .collect();
    std::fs::create_dir(workspace.join("out")).unwrap();
    std::fs::write(workspace.join("out/made.bin"), &whole).unwrap();
    let path = in_room("out/made.bin");
    let size = u64::try_from(whole.len()).unwrap();
    let mut taken = Vec::new();
    let mut first: Option<Took> = None;
    while u64::try_from(taken.len()).unwrap() < size {
        let at = u64::try_from(taken.len()).unwrap();
        let took = bench.take(&path, at).unwrap();
        assert!(took.check(at).is_ok());
        assert_eq!(took.size, size);
        if let Some(first) = &first {
            assert_eq!((took.ino, took.changed), (first.ino, first.changed));
        }
        taken.extend_from_slice(&took.bytes);
        first.get_or_insert(took);
    }
    assert_eq!(taken, whole);
    let lengths: Vec<usize> = [0, TAKE_BYTES, 2 * TAKE_BYTES]
        .map(|at| {
            bench
                .take(&path, u64::try_from(at).unwrap())
                .unwrap()
                .bytes
                .len()
        })
        .to_vec();
    assert_eq!(lengths, [TAKE_BYTES, TAKE_BYTES, 5]);
    // An empty file is one piece of nothing, and a path inside the
    // workspace that steps back into it is still beneath it.
    std::fs::write(workspace.join("empty"), b"").unwrap();
    let empty = bench.take(&in_room("empty"), 0).unwrap();
    assert_eq!((empty.bytes.len(), empty.size), (0, 0));
    assert!(empty.check(0).is_ok());
    let round = bench.take(&in_room("out/../empty"), 0).unwrap();
    assert_eq!(round.ino, empty.ino);
    // A piece from past where the file ends: it is shorter than it was.
    assert_eq!(bench.take(&path, size + 1), Err(NoTake::Changed));
    // Taking starts no job, and is still answered while one runs.
    bench.shell("sleeper", "sleep 30");
    assert_eq!(bench.take(&path, 0).unwrap().bytes.len(), TAKE_BYTES);
    assert!(matches!(bench.stop("sleeper"), Say::Stopping { .. }));
    bench.ended("sleeper");
}

#[test]
fn nothing_is_handed_back_through_a_link_or_from_outside_the_workspace() {
    use std::os::unix::fs::PermissionsExt as _;

    let mut bench = Bench::new("take-refused");
    let workspace = workspace(&bench);
    std::fs::write(bench.dir.join("outside.txt"), b"not the workspace's\n").unwrap();
    std::fs::write(workspace.join("notes.txt"), b"notes\n").unwrap();
    std::fs::create_dir(workspace.join("dir")).unwrap();
    // What isn't there, and what is in the way of it.
    assert_eq!(bench.take(&in_room("none.txt"), 0), Err(NoTake::NoSuchFile));
    assert_eq!(
        bench.take(&in_room("notes.txt/under"), 0),
        Err(NoTake::NoSuchFile)
    );
    // What isn't a file: a directory, and a pipe nobody writes to, which
    // is not waited for.
    assert_eq!(bench.take(&in_room("dir"), 0), Err(NoTake::NotAFile));
    rustix::fs::mknodat(
        rustix::fs::CWD,
        workspace.join("pipe"),
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    assert_eq!(bench.take(&in_room("pipe"), 0), Err(NoTake::NotAFile));
    // A link is never followed: not the last step, not one before it,
    // not one to a file inside the workspace, not one to itself.
    let link =
        |name: &str, to: &Path| std::os::unix::fs::symlink(to, workspace.join(name)).unwrap();
    link("to-outside", &bench.dir.join("outside.txt"));
    link("to-notes", &workspace.join("notes.txt"));
    link("to-dir", &workspace.join("dir"));
    link("loop", &workspace.join("loop"));
    std::fs::write(workspace.join("dir/inner.txt"), b"inner\n").unwrap();
    for linked in ["to-outside", "to-notes", "to-dir/inner.txt", "loop"] {
        assert_eq!(
            bench.take(&in_room(linked), 0),
            Err(NoTake::Link),
            "{linked}"
        );
    }
    // Nothing out of the workspace: not by stepping back past it, and not
    // by a path that isn't under it at all.
    for outside in [
        in_room("../outside.txt"),
        in_room("dir/../../outside.txt"),
        String::from("/session/jobs/a.stdout"),
        String::from("/workspaces/notes.txt"),
        String::from(WORKSPACE),
    ] {
        assert_eq!(bench.take(&outside, 0), Err(NoTake::Outside), "{outside}");
    }
    // A file its user may not read, where there is such a thing: root
    // reads what it likes.
    let closed = workspace.join("closed.txt");
    std::fs::write(&closed, b"kept\n").unwrap();
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    if rustix::process::geteuid().is_root() {
        assert!(bench.take(&in_room("closed.txt"), 0).is_ok());
    } else {
        assert_eq!(bench.take(&in_room("closed.txt"), 0), Err(NoTake::Denied));
    }
    // A workspace that isn't there: nothing is opened.
    std::fs::remove_dir_all(&workspace).unwrap();
    assert_eq!(bench.take(&in_room("notes.txt"), 0), Err(NoTake::Failed));
    // None of it ended the supervisor.
    assert_eq!(
        bench
            .page(&bench.dir.join("outside.txt"), 1, 1)
            .unwrap()
            .bytes,
        b"not the workspace's\n"
    );
}

#[test]
fn why_a_file_is_not_opened_is_said_from_the_kernels_word() {
    for (errno, why) in [
        (Errno::NOENT, NoTake::NoSuchFile),
        (Errno::NOTDIR, NoTake::NoSuchFile),
        (Errno::ACCESS, NoTake::Denied),
        (Errno::PERM, NoTake::Denied),
        (Errno::LOOP, NoTake::Link),
        (Errno::XDEV, NoTake::Outside),
        (Errno::NXIO, NoTake::NotAFile),
        (Errno::OPNOTSUPP, NoTake::NotAFile),
        (Errno::IO, NoTake::Failed),
        (Errno::NOMEM, NoTake::Failed),
    ] {
        assert_eq!(take::not_opened(errno), why, "{errno:?}");
    }
}

/// A file as a stand-in: what it is said to be at its first look and at
/// every one after, its bytes, the most one read gives, and what reads say
/// in place of bytes before they give any.
struct Stood {
    seen: [take::Seen; 2],
    looked: std::cell::Cell<usize>,
    /// From which look on a look fails.
    blind_from: Option<usize>,
    bytes: Vec<u8>,
    most: usize,
    errors: std::cell::RefCell<Vec<io::ErrorKind>>,
    read: std::cell::Cell<usize>,
}

impl Stood {
    /// A file of `bytes`, said to be as long as they are, read whole.
    fn of(bytes: Vec<u8>) -> Self {
        let seen = take::Seen {
            file: true,
            size: u64::try_from(bytes.len()).unwrap(),
            ino: 5,
            changed: 6,
        };
        Self {
            seen: [seen; 2],
            looked: std::cell::Cell::new(0),
            blind_from: None,
            bytes,
            most: usize::MAX,
            errors: std::cell::RefCell::new(Vec::new()),
            read: std::cell::Cell::new(0),
        }
    }
}

impl take::Readable for Stood {
    fn seen(&self) -> io::Result<take::Seen> {
        let look = self.looked.get();
        self.looked.set(look + 1);
        if self.blind_from.is_some_and(|from| look >= from) {
            return Err(io::Error::other("the file went"));
        }
        Ok(*self.seen.get(look.min(1)).unwrap())
    }

    fn read_from(&self, bytes: &mut [u8], at: u64) -> io::Result<usize> {
        self.read.set(self.read.get() + 1);
        if let Some(kind) = self.errors.borrow_mut().pop() {
            return Err(io::Error::from(kind));
        }
        let from = usize::try_from(at).unwrap().min(self.bytes.len());
        let count = (self.bytes.len() - from).min(bytes.len()).min(self.most);
        bytes
            .get_mut(..count)
            .unwrap()
            .copy_from_slice(self.bytes.get(from..from + count).unwrap());
        Ok(count)
    }
}

/// The bytes 0 to `len`, each its place mod 251.
fn counted(len: usize) -> Vec<u8> {
    (0..len).map(|at| u8::try_from(at % 251).unwrap()).collect()
}

#[test]
fn a_piece_is_read_whole_from_where_it_was_asked_however_its_reads_come() {
    let len = 2 * TAKE_BYTES + 9;
    let whole = counted(len);
    let mut stood = Stood::of(whole.clone());
    // Reads that give a few bytes at a time, and one interrupted.
    stood.most = 7;
    stood.errors = std::cell::RefCell::new(vec![io::ErrorKind::Interrupted]);
    let first = take::piece(&stood, 0).unwrap();
    assert_eq!(first.bytes, whole.get(..TAKE_BYTES).unwrap());
    assert_eq!(
        (first.size, first.ino, first.changed),
        (u64::try_from(len).unwrap(), 5, 6)
    );
    assert_eq!(stood.read.get(), TAKE_BYTES.div_ceil(7) + 1);
    let at = u64::try_from(2 * TAKE_BYTES).unwrap();
    let last = take::piece(&Stood::of(whole.clone()), at).unwrap();
    assert_eq!(last.bytes, whole.get(2 * TAKE_BYTES..).unwrap());
    assert!(last.check(at).is_ok());
    // From the very end: nothing, and the file isn't read.
    let end = Stood::of(whole.clone());
    let none = take::piece(&end, u64::try_from(len).unwrap()).unwrap();
    assert!(none.bytes.is_empty());
    assert_eq!(end.read.get(), 0);
    // From past it: the file is shorter than it was.
    let past = u64::try_from(len + 1).unwrap();
    assert_eq!(take::piece(&Stood::of(whole), past), Err(NoTake::Changed));
}

#[test]
fn a_piece_of_a_file_that_changed_or_isnt_one_is_not_handed_back() {
    // Not a file: nothing of it is read.
    let mut other = Stood::of(counted(9));
    other.seen = other.seen.map(|seen| take::Seen {
        file: false,
        ..seen
    });
    assert_eq!(take::piece(&other, 0), Err(NoTake::NotAFile));
    assert_eq!(other.read.get(), 0);
    // Another file, another length, or a change, after the piece is read.
    let changes: [fn(&mut take::Seen); 3] = [
        |seen| seen.ino += 1,
        |seen| seen.size += 1,
        |seen| seen.changed += 1,
    ];
    for change in changes {
        let mut stood = Stood::of(counted(9));
        if let Some(after) = stood.seen.get_mut(1) {
            change(after);
        }
        assert_eq!(take::piece(&stood, 0), Err(NoTake::Changed));
    }
    // Shorter than it was said to be while it was read.
    let mut short = Stood::of(counted(9));
    short.bytes.truncate(4);
    assert_eq!(take::piece(&short, 0), Err(NoTake::Changed));
    // A read that fails, and a look that does, before or after.
    let failing = Stood::of(counted(9));
    failing
        .errors
        .borrow_mut()
        .push(io::ErrorKind::PermissionDenied);
    assert_eq!(take::piece(&failing, 0), Err(NoTake::Failed));
    for from in [0, 1] {
        let mut blind = Stood::of(counted(9));
        blind.blind_from = Some(from);
        assert_eq!(
            take::piece(&blind, 0),
            Err(NoTake::Failed),
            "from look {from}"
        );
    }
}

/// A real file says which file it is, as it is: its change time, a
/// plausible time, moves when the file is written where it lies at the
/// same length, and its inode when another file is put in its place.
#[test]
fn a_real_file_says_when_it_changed_and_which_file_it_is() {
    use std::io::{Seek as _, SeekFrom};

    let mut bench = Bench::new("take-identity");
    let workspace = workspace(&bench);
    let path = workspace.join("made.txt");
    std::fs::write(&path, b"first words\n").unwrap();
    let first = bench.take(&in_room("made.txt"), 0).unwrap();
    // After 2017, and before 2100, in nanoseconds since the epoch.
    assert!((1_500_000_000_000_000_000..4_102_444_800_000_000_000).contains(&first.changed));
    std::thread::sleep(Duration::from_millis(50));
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"other words\n").unwrap();
    drop(file);
    let rewritten = bench.take(&in_room("made.txt"), 0).unwrap();
    assert_eq!(rewritten.bytes, b"other words\n");
    assert_eq!((rewritten.size, rewritten.ino), (first.size, first.ino));
    assert!(
        rewritten.changed > first.changed,
        "a write left its change time"
    );
    let beside = workspace.join("beside.txt");
    std::fs::write(&beside, b"third words\n").unwrap();
    std::fs::rename(&beside, &path).unwrap();
    let replaced = bench.take(&in_room("made.txt"), 0).unwrap();
    assert_eq!(replaced.size, first.size);
    assert_ne!(replaced.ino, first.ino, "another file has the same inode");
}

impl Bench {
    /// A page of the folder at the Room's `path` after `after`, or why
    /// none.
    fn list(&mut self, path: &str, after: &[u8]) -> std::result::Result<Listed, NoList> {
        let ask = Ask::List {
            nonce: self.nonce(),
            path: path.to_owned(),
            after: after.to_vec(),
        };
        let said = self.ask(&ask);
        if let Say::Listed { listed, .. } = said {
            return Ok(listed);
        }
        let Say::NoList { why, .. } = said else {
            unreachable!("{said:?} isn't what a listing is answered with");
        };
        Err(why)
    }

    /// Every name of the folder at the Room's `path`, page by page, and
    /// how many pages it took.
    fn list_whole(&mut self, path: &str) -> (Vec<hatchling_core::room::Entry>, usize) {
        let mut entries: Vec<hatchling_core::room::Entry> = Vec::new();
        let mut pages = 0;
        loop {
            let after = entries
                .last()
                .map(|entry| entry.name.clone())
                .unwrap_or_default();
            let page = self.list(path, &after).unwrap();
            assert!(page.check(&after).is_ok(), "{page:?}");
            pages += 1;
            let done = page.entries.len() < LIST_PAGE;
            let total = page.total;
            entries.extend(page.entries);
            if done || entries.len() == usize::try_from(total).unwrap() {
                return (entries, pages);
            }
        }
    }
}

/// A folder in the workspace is listed page by page, in the order of its
/// names' bytes, each with what it is, following no link.
#[test]
fn a_folder_is_listed_page_by_page_with_what_each_name_is() {
    use std::os::unix::fs::MetadataExt as _;
    let mut bench = Bench::new("list");
    let workspace = workspace(&bench);
    let site = workspace.join("site");
    std::fs::create_dir(&site).unwrap();
    std::fs::write(site.join("index.html"), b"<p>hi</p>\n").unwrap();
    std::fs::write(site.join("Empty"), b"").unwrap();
    std::fs::create_dir(site.join("css")).unwrap();
    std::fs::write(site.join(".gitignore"), b"target\n").unwrap();
    std::os::unix::fs::symlink(bench.dir.join("outside.txt"), site.join("away")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        site.join("pipe"),
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    let page = bench.list(&in_room("site"), b"").unwrap();
    assert!(page.check(b"").is_ok());
    let meta = |name: &str| std::fs::symlink_metadata(site.join(name)).unwrap();
    assert_eq!(
        (page.ino, page.total),
        (std::fs::metadata(&site).unwrap().ino(), 6)
    );
    let said: Vec<(&[u8], Kind, u64, u64)> = page
        .entries
        .iter()
        .map(|entry| (entry.name.as_slice(), entry.kind, entry.ino, entry.size))
        .collect();
    assert_eq!(
        said,
        [
            (&b".gitignore"[..], Kind::File, meta(".gitignore").ino(), 7),
            (b"Empty", Kind::File, meta("Empty").ino(), 0),
            (b"away", Kind::Link, 0, 0),
            (b"css", Kind::Folder, meta("css").ino(), 0),
            (b"index.html", Kind::File, meta("index.html").ino(), 10),
            (b"pipe", Kind::Other, 0, 0),
        ]
    );
    // The page goes on after a name, there or gone, and after the last
    // there is nothing.
    let rest = bench.list(&in_room("site"), b"css").unwrap();
    assert_eq!(rest.entries.len(), 2);
    assert_eq!(rest.entries.first().unwrap().name, b"index.html");
    let after_gone = bench.list(&in_room("site"), b"b-gone").unwrap();
    assert_eq!(after_gone.entries.first().unwrap().name, b"css");
    assert!(
        bench
            .list(&in_room("site"), b"zz")
            .unwrap()
            .entries
            .is_empty()
    );
    // An empty folder is a page of nothing.
    std::fs::create_dir(site.join("css/empty")).unwrap();
    let empty = bench.list(&in_room("site/css/empty"), b"").unwrap();
    assert_eq!((empty.total, empty.entries.len()), (0, 0));
    // Listing starts no job, and is still answered while one runs.
    bench.shell("sleeper", "sleep 30");
    assert_eq!(bench.list(&in_room("site"), b"").unwrap().total, 6);
    assert!(matches!(bench.stop("sleeper"), Say::Stopping { .. }));
    bench.ended("sleeper");
}

/// Names are bytes, in the order of their bytes: any a folder can hold,
/// whatever the text, up to the longest.
#[test]
fn a_folders_names_are_its_bytes_in_their_order() {
    use std::os::unix::ffi::OsStrExt as _;
    let mut bench = Bench::new("list-names");
    let workspace = workspace(&bench);
    let names = workspace.join("names");
    std::fs::create_dir(&names).unwrap();
    let longest = vec![b'n'; 255];
    let odd: [&[u8]; 6] = [b"B", b"a", b"\xff\xfe", b"line\nbreak", b" lead", &longest];
    for name in odd {
        std::fs::write(names.join(std::ffi::OsStr::from_bytes(name)), b"x").unwrap();
    }
    let page = bench.list(&in_room("names"), b"").unwrap();
    let mut sorted: Vec<&[u8]> = odd.to_vec();
    sorted.sort_unstable();
    let said: Vec<&[u8]> = page
        .entries
        .iter()
        .map(|entry| entry.name.as_slice())
        .collect();
    assert_eq!(said, sorted);
    assert_eq!(said.first(), Some(&&b" lead"[..]));
    assert!(
        said.iter().position(|name| *name == b"B") < said.iter().position(|name| *name == b"a")
    );
}

/// A folder of many names comes back in whole pages, the last short, and
/// one of more names than may be listed is refused.
#[test]
fn a_big_folder_comes_back_in_whole_pages_up_to_the_bound() {
    let mut bench = Bench::new("list-many");
    let workspace = workspace(&bench);
    let make = |folder: &str, count: usize| {
        let at = workspace.join(folder);
        std::fs::create_dir(&at).unwrap();
        for number in 0..count {
            std::fs::write(at.join(format!("{number:05}")), b"").unwrap();
        }
    };
    let most = usize::try_from(LIST_FOLDER_NAMES).unwrap();
    make("some", 2 * LIST_PAGE + 2);
    make("whole", 2 * LIST_PAGE);
    make("most", most);
    make("over", most + 1);
    let (some, pages) = bench.list_whole(&in_room("some"));
    assert_eq!((some.len(), pages), (2 * LIST_PAGE + 2, 3));
    assert!(
        some.windows(2)
            .all(|pair| pair.first().map(|one| &one.name) < pair.get(1).map(|two| &two.name))
    );
    let first = bench.list(&in_room("some"), b"").unwrap();
    assert_eq!(first.entries.len(), LIST_PAGE);
    assert_eq!(first.total, u32::try_from(2 * LIST_PAGE + 2).unwrap());
    let (whole, pages) = bench.list_whole(&in_room("whole"));
    assert_eq!((whole.len(), pages), (2 * LIST_PAGE, 2));
    let (all, _) = bench.list_whole(&in_room("most"));
    assert_eq!(all.len(), most);
    assert_eq!(bench.list(&in_room("over"), b""), Err(NoList::TooMany));
}

/// No folder is listed through a link, out of the workspace, or that
/// isn't one, and none of it ends the supervisor.
#[test]
fn a_folder_is_listed_only_beneath_the_workspace_through_no_link() {
    use std::os::unix::fs::PermissionsExt as _;
    let mut bench = Bench::new("list-refused");
    let workspace = workspace(&bench);
    std::fs::create_dir(bench.dir.join("outside")).unwrap();
    std::fs::create_dir_all(workspace.join("dir/inner")).unwrap();
    std::fs::write(workspace.join("notes.txt"), b"notes\n").unwrap();
    assert_eq!(bench.list(&in_room("none"), b""), Err(NoList::NoSuchFolder));
    for not_one in ["notes.txt", "notes.txt/under"] {
        assert_eq!(
            bench.list(&in_room(not_one), b""),
            Err(NoList::NotAFolder),
            "{not_one}"
        );
    }
    let link =
        |name: &str, to: &Path| std::os::unix::fs::symlink(to, workspace.join(name)).unwrap();
    link("to-outside", &bench.dir.join("outside"));
    link("to-dir", &workspace.join("dir"));
    link("loop", &workspace.join("loop"));
    for linked in ["to-outside", "to-dir", "to-dir/inner", "loop"] {
        assert_eq!(
            bench.list(&in_room(linked), b""),
            Err(NoList::Link),
            "{linked}"
        );
    }
    for outside in [
        in_room(".."),
        in_room("dir/../.."),
        String::from("/session"),
        String::from("/workspaces/dir"),
        String::from("/workspaces"),
    ] {
        assert_eq!(bench.list(&outside, b""), Err(NoList::Outside), "{outside}");
    }
    // The workspace itself is a folder beneath itself.
    let root = bench.list(WORKSPACE, b"").unwrap();
    let names: Vec<&[u8]> = root
        .entries
        .iter()
        .map(|entry| entry.name.as_slice())
        .collect();
    assert_eq!(
        names,
        [&b"dir"[..], b"loop", b"notes.txt", b"to-dir", b"to-outside"]
    );
    // A folder whose names can be read but not looked at, where there is
    // such a thing: root looks at what it likes.
    let closed = workspace.join("closed");
    std::fs::create_dir(&closed).unwrap();
    std::fs::write(closed.join("a"), b"a").unwrap();
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o444)).unwrap();
    let said = bench.list(&in_room("closed"), b"");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).unwrap();
    if rustix::process::geteuid().is_root() {
        assert!(said.is_ok());
    } else {
        assert_eq!(said, Err(NoList::Denied));
    }
    // A workspace that isn't there: nothing is opened.
    std::fs::remove_dir_all(&workspace).unwrap();
    assert_eq!(bench.list(&in_room("dir"), b""), Err(NoList::Failed));
    // None of it ended the supervisor.
    std::fs::create_dir_all(workspace.join("back")).unwrap();
    assert_eq!(bench.list(&in_room("back"), b"").unwrap().total, 0);
}

#[test]
fn why_a_folder_is_not_listed_is_said_from_the_kernels_word() {
    for (errno, why) in [
        (Errno::NOENT, NoList::NoSuchFolder),
        (Errno::NOTDIR, NoList::NotAFolder),
        (Errno::ACCESS, NoList::Denied),
        (Errno::PERM, NoList::Denied),
        (Errno::LOOP, NoList::Link),
        (Errno::XDEV, NoList::Outside),
        (Errno::IO, NoList::Failed),
        (Errno::AGAIN, NoList::Failed),
    ] {
        assert_eq!(list::not_opened(errno), why, "{errno:?}");
    }
    for (errno, why) in [
        (Errno::NOENT, NoList::Changed),
        (Errno::ACCESS, NoList::Denied),
        (Errno::PERM, NoList::Denied),
        (Errno::NOTDIR, NoList::Failed),
        (Errno::IO, NoList::Failed),
    ] {
        assert_eq!(list::not_read(errno), why, "{errno:?}");
    }
}

/// What the kernel says of a folder or a name is taken only when it said
/// all that was asked, and a mode is what it says.
#[test]
fn what_the_kernel_says_of_a_name_is_taken_only_whole() {
    use rustix::fs::StatxFlags;
    let seen = StatxFlags::INO
        .union(StatxFlags::CTIME)
        .union(StatxFlags::MNT_ID)
        .bits();
    assert_eq!(
        list::seen_from(seen, 3, 2, 5, 9),
        Ok(list::Seen {
            ino: 3,
            changed: 2_000_000_005,
            mount: 9
        })
    );
    for missing in [StatxFlags::INO, StatxFlags::CTIME, StatxFlags::MNT_ID] {
        assert_eq!(
            list::seen_from(seen & !missing.bits(), 3, 2, 5, 9),
            Err(NoList::Failed),
            "{missing:?}"
        );
    }
    let looked = StatxFlags::TYPE
        .union(StatxFlags::INO)
        .union(StatxFlags::SIZE)
        .union(StatxFlags::MNT_ID)
        .bits();
    assert_eq!(
        list::looked_from(looked | StatxFlags::BLOCKS.bits(), 0o100_644, 4, 10, 9),
        Ok(list::Looked {
            kind: Kind::File,
            ino: 4,
            size: 10,
            mount: 9
        })
    );
    for missing in [
        StatxFlags::TYPE,
        StatxFlags::INO,
        StatxFlags::SIZE,
        StatxFlags::MNT_ID,
    ] {
        assert_eq!(
            list::looked_from(looked & !missing.bits(), 0o100_644, 4, 10, 9),
            Err(NoList::Failed),
            "{missing:?}"
        );
    }
    for (mode, kind) in [
        (0o100_644, Kind::File),
        (0o040_755, Kind::Folder),
        (0o120_777, Kind::Link),
        (0o010_600, Kind::Other),
        (0o020_600, Kind::Other),
        (0o060_600, Kind::Other),
        (0o140_600, Kind::Other),
    ] {
        assert_eq!(list::kind_of(mode), kind, "{mode:o}");
    }
    assert_eq!(list::nanos(-1, 5), 5);
    assert_eq!(list::nanos(i64::MAX, 5), u64::MAX);
}

/// A folder as a stand-in: what it is said to be at each look, its names
/// at each read of them, and what each name is.
struct Held {
    seen: Vec<std::result::Result<list::Seen, NoList>>,
    looked_at: std::cell::Cell<usize>,
    names: Vec<std::result::Result<Vec<Vec<u8>>, NoList>>,
    read: usize,
    looks: BTreeMap<Vec<u8>, std::result::Result<list::Looked, NoList>>,
    asked: std::cell::RefCell<Vec<Vec<u8>>>,
}

const HELD_SEEN: list::Seen = list::Seen {
    ino: 2,
    changed: 3,
    mount: 1,
};

impl Held {
    /// A folder of files named `names`, the same at every look.
    fn of(names: &[&[u8]]) -> Self {
        let names: Vec<Vec<u8>> = names.iter().map(|name| name.to_vec()).collect();
        let looks = names
            .iter()
            .enumerate()
            .map(|(at, name)| {
                let looked = list::Looked {
                    kind: Kind::File,
                    ino: 100 + u64::try_from(at).unwrap(),
                    size: 1,
                    mount: 1,
                };
                (name.clone(), Ok(looked))
            })
            .collect();
        Self {
            seen: vec![Ok(HELD_SEEN)],
            looked_at: std::cell::Cell::new(0),
            names: vec![Ok(names)],
            read: 0,
            looks,
            asked: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl list::Folder for Held {
    fn seen(&self) -> std::result::Result<list::Seen, NoList> {
        let look = self.looked_at.get();
        self.looked_at.set(look + 1);
        *self.seen.get(look).or_else(|| self.seen.last()).unwrap()
    }

    fn names(&mut self) -> std::result::Result<Vec<Vec<u8>>, NoList> {
        let read = self.read;
        self.read += 1;
        self.names
            .get(read)
            .or_else(|| self.names.last())
            .unwrap()
            .clone()
    }

    fn look(&self, name: &[u8]) -> std::result::Result<list::Looked, NoList> {
        self.asked.borrow_mut().push(name.to_vec());
        *self.looks.get(name).unwrap()
    }
}

/// A page is the names after the one asked from, at most a page of them,
/// each looked at once, and the folder as it was before the page.
#[test]
fn a_page_is_the_names_after_the_one_asked_from() {
    let names: Vec<Vec<u8>> = (0..2 * LIST_PAGE + 2)
        .rev()
        .map(|at| format!("{at:04}").into_bytes())
        .collect();
    let held: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
    let mut folder = Held::of(&held);
    let page = list::page(&mut folder, b"").unwrap();
    assert_eq!((page.ino, page.changed, page.total), (2, 3, 130));
    assert_eq!(page.entries.len(), LIST_PAGE);
    assert_eq!(page.entries.first().unwrap().name, b"0000");
    assert_eq!(
        folder.asked.borrow().len(),
        LIST_PAGE,
        "a name looked at past the page"
    );
    let mut folder = Held::of(&held);
    let page = list::page(&mut folder, b"0063").unwrap();
    assert_eq!(page.entries.first().unwrap().name, b"0064");
    assert_eq!(page.entries.len(), LIST_PAGE);
    let mut folder = Held::of(&held);
    let page = list::page(&mut folder, b"0127").unwrap();
    let last: Vec<&[u8]> = page
        .entries
        .iter()
        .map(|entry| entry.name.as_slice())
        .collect();
    assert_eq!(last, [b"0128", b"0129"]);
    assert_eq!(folder.asked.borrow().len(), 2);
}

/// What is on another mount is said to be a mount, whatever it is, with
/// no inode or length; a folder or a link has no length, and only a file
/// or a folder an inode.
#[test]
fn a_name_on_another_mount_is_a_mount_whatever_it_is() {
    let mut folder = Held::of(&[
        b"file",
        b"folder",
        b"inbox",
        b"link",
        b"mounted-file",
        b"pipe",
    ]);
    let set = |folder: &mut Held, name: &[u8], kind: Kind, mount: u64| {
        folder.looks.insert(
            name.to_vec(),
            Ok(list::Looked {
                kind,
                ino: 7,
                size: 9,
                mount,
            }),
        );
    };
    set(&mut folder, b"file", Kind::File, 1);
    set(&mut folder, b"folder", Kind::Folder, 1);
    set(&mut folder, b"inbox", Kind::Folder, 2);
    set(&mut folder, b"link", Kind::Link, 1);
    set(&mut folder, b"mounted-file", Kind::File, 3);
    set(&mut folder, b"pipe", Kind::Other, 1);
    let page = list::page(&mut folder, b"").unwrap();
    assert!(page.check(b"").is_ok());
    let said: Vec<(Kind, u64, u64)> = page
        .entries
        .iter()
        .map(|entry| (entry.kind, entry.ino, entry.size))
        .collect();
    assert_eq!(
        said,
        [
            (Kind::File, 7, 9),
            (Kind::Folder, 7, 0),
            (Kind::Mount, 0, 0),
            (Kind::Link, 0, 0),
            (Kind::Mount, 0, 0),
            (Kind::Other, 0, 0),
        ]
    );
}

/// A folder that changed while its page was read, by its names, its
/// inode, its change time or its mount, is said to have changed, and so is
/// one that names a name twice; what can't be a name isn't handed back.
#[test]
fn a_folder_that_changed_while_it_was_read_has_no_page() {
    let refused_as_changed =
        |folder: &mut Held| assert_eq!(list::page(folder, b""), Err(NoList::Changed));
    let changes: [fn(&mut list::Seen); 3] = [
        |seen| seen.ino += 1,
        |seen| seen.changed += 1,
        |seen| seen.mount += 1,
    ];
    for change in changes {
        let mut folder = Held::of(&[b"a", b"b"]);
        let mut after = HELD_SEEN;
        change(&mut after);
        folder.seen = vec![Ok(HELD_SEEN), Ok(after)];
        refused_as_changed(&mut folder);
    }
    // Its names read again, another added, one gone, or one swapped.
    for again in [
        vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
        vec![b"a".to_vec()],
        vec![b"a".to_vec(), b"c".to_vec()],
    ] {
        let mut folder = Held::of(&[b"a", b"b"]);
        folder.names.push(Ok(again));
        refused_as_changed(&mut folder);
    }
    // The same names read again, in another order, is the same folder.
    let mut folder = Held::of(&[b"a", b"b"]);
    folder.names.push(Ok(vec![b"b".to_vec(), b"a".to_vec()]));
    assert!(list::page(&mut folder, b"").is_ok());
    // A name read twice in one read.
    let mut folder = Held::of(&[b"a", b"b"]);
    folder.names = vec![Ok(vec![b"b".to_vec(), b"a".to_vec(), b"b".to_vec()])];
    refused_as_changed(&mut folder);
    // A name gone before it is looked at, and one the user may not look
    // at, are said as they are.
    let mut folder = Held::of(&[b"a", b"b"]);
    folder.looks.insert(b"b".to_vec(), Err(NoList::Changed));
    refused_as_changed(&mut folder);
    let mut folder = Held::of(&[b"a", b"b"]);
    folder.looks.insert(b"a".to_vec(), Err(NoList::Denied));
    assert_eq!(list::page(&mut folder, b""), Err(NoList::Denied));
    // What can't be a name, before or after the page asked for.
    for bad in [&b"a/b"[..], b"", b".", &[b'n'; 256]] {
        let mut folder = Held::of(&[b"m", bad]);
        assert_eq!(
            list::page(&mut folder, b"z"),
            Err(NoList::Failed),
            "{bad:?}"
        );
    }
    // Too many names, at the first read or the second, and a look at the
    // folder that fails, before the page or after it.
    for at in [0, 1] {
        let mut folder = Held::of(&[b"a"]);
        folder.names.insert(at, Err(NoList::TooMany));
        assert_eq!(list::page(&mut folder, b""), Err(NoList::TooMany), "{at}");
        let mut folder = Held::of(&[b"a"]);
        folder.seen.insert(at, Err(NoList::Failed));
        assert_eq!(list::page(&mut folder, b""), Err(NoList::Failed), "{at}");
    }
}
