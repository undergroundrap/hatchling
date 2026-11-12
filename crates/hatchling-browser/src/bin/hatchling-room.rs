//! The supervisor in the Room (WO-034): it runs the Room's jobs.
//!
//! The spawner owns admission, deadlines, and teardown. This endpoint runs
//! in the Room as the Room's user: its answers and the bytes it captures
//! are untrusted data. It speaks [`hatchling_core::room`]'s wire for as
//! long as the spawner keeps its Room: a job is started, its two output
//! streams are kept as a head and a tail with the whole of each in a file
//! in the session directory, and its end is said when its first process
//! has ended. Whatever that process left in its process group is killed
//! with it.
//!
//! One thread does all of it, waiting on the spawner's socket, every
//! running job's pipes and every job's first process at once, so no job
//! holds up another, a stop, or an answer.
//!
//! The Room's launcher starts it with no argument, and it keeps its files
//! in the Room's session directory. Given one, it keeps them there
//! instead: that is for proving it under the Room's filter outside a
//! Room, where there is no session directory.

use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use hatchling_core::framing::{read_frame_or_eof, write_frame};
use hatchling_core::room::{
    Ask, Captured, End, Ended, JobId, NotStarted, PIECE_BYTES, RUNNING_MAX, SESSION_DIR,
    SPILL_BYTES, Say, Stream, spill_path,
};
use rustix::event::{PollFd, PollFlags, poll};
use rustix::process::{Pid, PidfdFlags, Signal, kill_process_group, pidfd_open};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const CONNECTION: i32 = 3;

/// How many ended jobs are remembered, with their kept output and their
/// files. The oldest goes when another ends.
const KEPT_MAX: usize = 16;

/// How long a job's pipes are still read after its first process has
/// ended, when something outside its process group holds them open.
const DRAIN: Duration = Duration::from_millis(200);

fn main() -> ExitCode {
    let result = hatchling_sys::net::inherited_unix_stream(CONNECTION)
        .map_err(Box::<dyn Error>::from)
        .and_then(|stream| {
            let place = std::env::args_os()
                .nth(1)
                .map_or_else(Place::room, |session| Place::under(Path::new(&session)));
            serve(&stream, &place)
        });
    if result.is_ok() {
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(io::stderr(), "hatchling-room: supervisor failed");
    ExitCode::FAILURE
}

/// Where the supervisor keeps its files, and what a job is told of them.
struct Place {
    /// What jobs wrote, and were given to read.
    jobs: PathBuf,
    /// A job's home: gone with the session, so nothing a job leaves
    /// there is read by a later session's jobs.
    home: PathBuf,
    /// A job's place for temporary files.
    scratch: PathBuf,
}

impl Place {
    /// The Room's own: under its session directory.
    fn room() -> Self {
        Self::under(Path::new(SESSION_DIR))
    }

    /// Under the session directory `session`.
    fn under(session: &Path) -> Self {
        Self {
            jobs: session.join("jobs"),
            home: session.join("home"),
            scratch: session.to_path_buf(),
        }
    }

    /// The file that holds what `job` wrote to `stream`: the name
    /// [`spill_path`] gives it, in this place's directory.
    fn spill(&self, job: &JobId, stream: Stream) -> PathBuf {
        let path = spill_path(job, stream);
        self.jobs
            .join(Path::new(&path).file_name().unwrap_or_default())
    }

    /// The file a job's standard input is read from.
    fn input(&self, job: &JobId) -> PathBuf {
        self.jobs.join(format!("{job}.stdin"))
    }

    /// Removes every file of `job`'s.
    fn forget(&self, job: &JobId) {
        for stream in Stream::BOTH {
            let _ = std::fs::remove_file(self.spill(job, stream));
        }
        let _ = std::fs::remove_file(self.input(job));
    }
}

/// A file only the Room's user may read, made anew.
fn private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// One stream of a job, as it is captured.
struct Capture {
    pipe: Option<File>,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: u64,
    spill: Option<File>,
    spilled: u64,
}

impl Capture {
    /// A stream read from `pipe`, with the file it is written to. With
    /// none (a full session directory can't hold another), the head and
    /// the tail are kept all the same, and the file is said to be cut.
    const fn new(pipe: File, spill: Option<File>) -> Self {
        Self {
            pipe: Some(pipe),
            head: Vec::new(),
            tail: VecDeque::new(),
            dropped: 0,
            spill,
            spilled: 0,
        }
    }

    /// Keeps `bytes`: in the file while it has room, in the head while
    /// that has room, and the rest as the newest of the tail.
    fn take(&mut self, bytes: &[u8]) {
        if let Some(file) = self.spill.as_mut() {
            let room = usize::try_from(SPILL_BYTES.saturating_sub(self.spilled)).unwrap_or(0);
            let part = bytes.get(..bytes.len().min(room)).unwrap_or_default();
            if file.write_all(part).is_ok() {
                self.spilled = self
                    .spilled
                    .saturating_add(u64::try_from(part.len()).unwrap_or(0));
            } else {
                // A file that can't be written holds what it held.
                self.spill = None;
            }
        }
        let keep = bytes.len().min(PIECE_BYTES.saturating_sub(self.head.len()));
        let (first, rest) = bytes.split_at(keep);
        self.head.extend_from_slice(first);
        self.tail.extend(rest);
        let over = self.tail.len().saturating_sub(PIECE_BYTES);
        self.tail.drain(..over);
        self.dropped = self
            .dropped
            .saturating_add(u64::try_from(over).unwrap_or(0));
    }

    /// Reads once from the pipe: what it gives is kept, and its end
    /// closes it.
    fn read(&mut self) -> io::Result<()> {
        let Some(pipe) = self.pipe.as_mut() else {
            return Ok(());
        };
        let mut buffer = [0_u8; PIECE_BYTES];
        match pipe.read(&mut buffer) {
            Ok(0) => self.pipe = None,
            Ok(count) => self.take(buffer.get(..count).unwrap_or_default()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    /// What was kept, in the wire's counts.
    fn captured(&self) -> Captured {
        let head = u32::try_from(self.head.len()).unwrap_or(u32::MAX);
        let tail = u32::try_from(self.tail.len()).unwrap_or(u32::MAX);
        let total = u64::from(head)
            .saturating_add(self.dropped)
            .saturating_add(u64::from(tail));
        Captured {
            head,
            tail,
            dropped: self.dropped,
            spilled: self.spilled,
            spill_cut: self.spilled != total,
        }
    }
}

/// A job that was started and hasn't been said to have ended.
struct Job {
    child: Child,
    /// Readable once the first process has ended.
    pidfd: OwnedFd,
    /// Standard output, then standard error.
    streams: [Capture; 2],
    /// How the first process ended, and until when its pipes are read.
    end: Option<(Ended, Instant)>,
}

impl Job {
    /// Kills every process in the job's group. Only while the first
    /// process hasn't been reaped: until then its pid is its own, and
    /// names its group.
    fn kill(&self) {
        if self.end.is_none() {
            let _ = kill_process_group(Pid::from_child(&self.child), Signal::Kill);
        }
    }

    /// Takes the first process's end, once its pidfd says it has one.
    /// What it left in its group is killed before it is reaped.
    fn exit(&mut self) -> io::Result<()> {
        self.kill();
        let Some(status) = self.child.try_wait()? else {
            return Ok(());
        };
        let how = match (status.code(), status.signal()) {
            (Some(code), _) => Ended::Exited(u8::try_from(code).unwrap_or(u8::MAX)),
            (_, Some(signal)) => Ended::Signaled(u8::try_from(signal).unwrap_or(u8::MAX)),
            _ => return Err(io::Error::other("unrecognized child exit status")),
        };
        self.end = Some((how, Instant::now() + DRAIN));
        Ok(())
    }

    /// How it ended, once it has and nothing more will be read of it.
    fn ended(&self, now: Instant) -> Option<Ended> {
        let (how, until) = self.end?;
        let open = self.streams.iter().any(|stream| stream.pipe.is_some());
        (!open || now >= until).then_some(how)
    }

    /// Ends it whatever it is doing, and waits for its first process.
    fn clear(&mut self) {
        self.kill();
        let _ = self.child.wait();
    }
}

/// What is kept of an ended job.
struct Done {
    /// Standard output's head and tail, then standard error's.
    pieces: [[Vec<u8>; 2]; 2],
}

/// What a watched descriptor is.
enum Watch {
    Pipe(JobId, usize),
    Exit(JobId),
}

/// The supervisor's jobs.
struct Room<'a> {
    place: &'a Place,
    running: BTreeMap<JobId, Job>,
    kept: VecDeque<(JobId, Done)>,
}

fn serve(stream: &UnixStream, place: &Place) -> Result<()> {
    let mut room = Room {
        place,
        running: BTreeMap::new(),
        kept: VecDeque::new(),
    };
    let result = room.run(stream);
    for job in room.running.values_mut() {
        job.clear();
    }
    result
}

impl Room<'_> {
    /// Serves the spawner until it closes its end.
    fn run(&mut self, stream: &UnixStream) -> Result<()> {
        write_frame(stream, b"ready")?;
        loop {
            for said in self.finish(Instant::now()) {
                write_frame(stream, &serde_json::to_vec(&said)?)?;
            }
            let (spawner, ready) = self.wait(stream)?;
            for watch in ready {
                self.watched(&watch)?;
            }
            if spawner {
                let Some(frame) = read_frame_or_eof(stream)? else {
                    return Ok(());
                };
                let ask: Ask = serde_json::from_slice(&frame)?;
                let said = self.answer(ask);
                write_frame(stream, &serde_json::to_vec(&said)?)?;
            }
        }
    }

    /// Waits until the spawner has sent something, a job has written or
    /// ended, or a job's pipes have been read for long enough. Says
    /// whether the spawner's socket is ready, and which jobs' descriptors.
    fn wait(&self, stream: &UnixStream) -> Result<(bool, Vec<Watch>)> {
        let mut watched = Vec::new();
        let mut fds = vec![PollFd::new(stream, PollFlags::IN)];
        let mut due: Option<Instant> = None;
        for (id, job) in &self.running {
            for (index, capture) in job.streams.iter().enumerate() {
                if let Some(pipe) = &capture.pipe {
                    fds.push(PollFd::new(pipe, PollFlags::IN));
                    watched.push(Watch::Pipe(id.clone(), index));
                }
            }
            match job.end {
                None => {
                    fds.push(PollFd::new(&job.pidfd, PollFlags::IN));
                    watched.push(Watch::Exit(id.clone()));
                }
                Some((_, until)) => due = Some(due.map_or(until, |due| due.min(until))),
            }
        }
        let timeout = due.map_or(-1, |due| {
            let left = due.saturating_duration_since(Instant::now()).as_millis();
            i32::try_from(left).unwrap_or(i32::MAX).saturating_add(1)
        });
        match poll(&mut fds, timeout) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => return Ok((false, Vec::new())),
            Err(error) => return Err(error.into()),
        }
        let mut ready = fds.iter().map(|fd| !fd.revents().is_empty());
        let spawner = ready.next().unwrap_or(false);
        let ready: Vec<bool> = ready.collect();
        drop(fds);
        let ready = watched
            .into_iter()
            .zip(ready)
            .filter_map(|(watch, ready)| ready.then_some(watch))
            .collect();
        Ok((spawner, ready))
    }

    /// Reads a job's pipe, or takes its first process's end.
    fn watched(&mut self, watch: &Watch) -> Result<()> {
        match watch {
            Watch::Pipe(id, index) => {
                let capture = self
                    .running
                    .get_mut(id)
                    .and_then(|job| job.streams.get_mut(*index));
                if let Some(capture) = capture {
                    capture.read()?;
                }
            }
            Watch::Exit(id) => {
                if let Some(job) = self.running.get_mut(id) {
                    job.exit()?;
                }
            }
        }
        Ok(())
    }

    /// Moves every job that has ended to the kept ones, and says each
    /// one's end.
    fn finish(&mut self, now: Instant) -> Vec<Say> {
        let ended: Vec<(JobId, Ended)> = self
            .running
            .iter()
            .filter_map(|(id, job)| Some((id.clone(), job.ended(now)?)))
            .collect();
        let mut said = Vec::new();
        for (id, how) in ended {
            let Some(job) = self.running.remove(&id) else {
                continue;
            };
            let [stdout, stderr] = job.streams;
            said.push(Say::Ended {
                job: id.clone(),
                how,
                stdout: stdout.captured(),
                stderr: stderr.captured(),
            });
            let pieces =
                [stdout, stderr].map(|capture| [capture.head, capture.tail.into_iter().collect()]);
            let _ = std::fs::remove_file(self.place.input(&id));
            self.kept.push_back((id, Done { pieces }));
            // One ended, so at most one is over the count.
            if self.kept.len() > KEPT_MAX {
                if let Some((oldest, _)) = self.kept.pop_front() {
                    self.place.forget(&oldest);
                }
            }
        }
        said
    }

    /// Does what the spawner asks, and answers it.
    fn answer(&mut self, ask: Ask) -> Say {
        match ask {
            Ask::Start {
                nonce,
                job,
                argv,
                dir,
                stdin,
            } => match self.start(&job, &argv, &dir, &stdin) {
                Ok(started) => {
                    self.running.insert(job, started);
                    Say::Started { nonce }
                }
                Err(why) => Say::NotStarted { nonce, why },
            },
            Ask::Stop { nonce, job } => match self.running.get(&job) {
                Some(running) => {
                    running.kill();
                    Say::Stopping { nonce }
                }
                None => Say::NoJob { nonce },
            },
            Ask::Piece {
                nonce,
                job,
                stream,
                end,
            } => {
                let piece = self
                    .kept
                    .iter()
                    .find(|(id, _)| *id == job)
                    .map(|(_, done)| {
                        let [stdout, stderr] = &done.pieces;
                        let [head, tail] = match stream {
                            Stream::Stdout => stdout,
                            Stream::Stderr => stderr,
                        };
                        match end {
                            End::Head => head.clone(),
                            End::Tail => tail.clone(),
                        }
                    });
                match piece {
                    Some(bytes) => Say::Piece { nonce, bytes },
                    None => Say::NoJob { nonce },
                }
            }
        }
    }

    /// Starts a job: its files in the session directory, then its first
    /// process, in a process group of its own, with nothing of the
    /// supervisor's environment or descriptors.
    fn start(
        &self,
        job: &JobId,
        argv: &[String],
        dir: &str,
        stdin: &[u8],
    ) -> std::result::Result<Job, NotStarted> {
        if self.running.contains_key(job) || self.kept.iter().any(|(id, _)| id == job) {
            return Err(NotStarted::Known);
        }
        if self.running.len() >= RUNNING_MAX {
            return Err(NotStarted::Full);
        }
        let (program, arguments) = argv.split_first().ok_or(NotStarted::NoSuchProgram)?;
        if !Path::new(dir).is_dir() {
            return Err(NotStarted::NoSuchDir);
        }
        // The files of what a job writes are kept if they can be: a
        // session directory that is full still runs the command that
        // would clear it. What a job is given to read must be whole.
        let _ = std::fs::create_dir_all(&self.place.jobs);
        let _ = std::fs::create_dir_all(&self.place.home);
        let given = || -> io::Result<Stdio> {
            if stdin.is_empty() {
                return Ok(Stdio::null());
            }
            let path = self.place.input(job);
            private(&path)?.write_all(stdin)?;
            Ok(Stdio::from(File::open(&path)?))
        };
        let input = given().map_err(|_| {
            self.place.forget(job);
            NotStarted::NoSession
        })?;
        let stdout = private(&self.place.spill(job, Stream::Stdout)).ok();
        let stderr = private(&self.place.spill(job, Stream::Stderr)).ok();
        let spawned = Command::new(program)
            .args(arguments)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.place.home)
            .env("TMPDIR", &self.place.scratch)
            .env("LANG", "C.UTF-8")
            .process_group(0)
            .stdin(input)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(mut child) = spawned else {
            self.place.forget(job);
            return Err(NotStarted::NoSuchProgram);
        };
        let watched = pidfd_open(Pid::from_child(&child), PidfdFlags::empty());
        let pipes = (child.stdout.take(), child.stderr.take());
        let (Ok(pidfd), (Some(out), Some(err))) = (watched, pipes) else {
            // A job that can't be watched isn't one: it's ended here.
            let _ = kill_process_group(Pid::from_child(&child), Signal::Kill);
            let _ = child.wait();
            self.place.forget(job);
            return Err(NotStarted::NoSession);
        };
        Ok(Job {
            child,
            pidfd,
            streams: [
                Capture::new(File::from(OwnedFd::from(out)), stdout),
                Capture::new(File::from(OwnedFd::from(err)), stderr),
            ],
            end: None,
        })
    }
}

#[cfg(test)]
#[path = "hatchling-room/tests.rs"]
mod tests;
