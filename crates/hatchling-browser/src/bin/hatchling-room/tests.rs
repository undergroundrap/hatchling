//! The supervisor, served over a socket pair as the spawner reaches it,
//! running real programs: each test has a directory of its own to stand
//! for the Room's session directory.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hatchling_core::framing::read_frame;
use hatchling_core::room::JOBS_DIR;

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
        let dir = std::env::temp_dir().join(format!(
            "hatchling-room-supervisor-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let place = Place::under(&dir);
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
