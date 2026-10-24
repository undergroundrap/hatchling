//! The spawner (WO-017, step 3): the process that starts every cell's
//! helper, and the one that will keep the privilege to once the broker
//! drops it.
//!
//! [`start`] runs in the broker's start, after the egress authority's,
//! while the broker still has one thread. It starts the broker's own binary
//! again as `spawner`, joined to the broker by a socket pair that carries
//! only connections, as the authority's does: for each helper, the broker
//! makes a socket pair and sends the spawner one end.
//!
//! On a connection the broker asks one thing:
//! - **A helper** (`agent-run`, `skill-cell`, `browser-cell`), with its
//!   cell spec, its run's private directory, and its three stdio
//!   descriptors ([`Spawner::helper`]). The spawner checks the spec is
//!   one that helper starts, writes it to the run's directory as the
//!   helper's spec file, starts the helper on those descriptors, and says
//!   its pid. It reports how the helper ended, and kills it when the broker
//!   asks, or goes.
//! - **A change to a harness's work trees** (`harness-tree` give or
//!   reclaim): the spawner runs it, its id maps included, and says how it
//!   went ([`Spawner::trees`]).
//!
//! **Lifetimes.** A helper watches its parent and ends with it: the
//! spawner, which ends when the broker does (its pair with the broker
//! closes), so a broker that's gone, however it went, takes its helpers
//! and their cells with it, as before.
//!
//! The broker still runs as root until slice 3: this slice moves every
//! start behind the spawner first. Served in-process with no spawner
//! running, as with no keeper, the broker starts its helpers itself
//! ([`start_helper`]).

use std::fs::File;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_isolation::{
    CellIdentity, Error as IsolationError, Exit, IsolationBackend as _, Mapping, PlatformError,
    Supervised,
};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::cell::{CellConfig, Process};
use crate::harness_tree::TreeOp;
use crate::run_launcher::{BROKER_PID_ENV, broker_pid, forget_spec, spec_args};

/// The spawner's subcommand: `hatchling-broker spawner`, started only by
/// the broker.
pub const SPAWNER_SUBCOMMAND: &str = "spawner";

/// The setting that names the spawner's end of its pair with the broker.
const SPAWNER_FD: &str = crate::environment::HATCHLING_SPAWNER_FD.name;

/// How long a killed helper has to be reaped before its wait gives up.
const REAP_AFTER_KILL: Duration = Duration::from_secs(5);

/// How long the broker waits for the spawner to say a helper started.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the broker waits for a change to a harness's trees: past the
/// spawner's own bound on its tree helper.
const TREES_TIMEOUT: Duration =
    Duration::from_secs(crate::harness_tree::HELPER_TIMEOUT.as_secs() + 30);

/// A helper the spawner starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HelperKind {
    /// A scheduled run's, a subagent's or a chat's cell
    /// ([`crate::AGENT_RUN_SUBCOMMAND`]).
    AgentRun,
    /// A skill's or a harness's cell
    /// ([`crate::skill_run::SKILL_CELL_SUBCOMMAND`]).
    SkillCell,
    /// A browser's cell ([`crate::browser_cell::BROWSER_CELL_SUBCOMMAND`]).
    BrowserCell,
}

impl HelperKind {
    /// Its subcommand.
    #[must_use]
    pub const fn subcommand(self) -> &'static str {
        match self {
            Self::AgentRun => crate::AGENT_RUN_SUBCOMMAND,
            Self::SkillCell => crate::skill_run::SKILL_CELL_SUBCOMMAND,
            Self::BrowserCell => crate::browser_cell::BROWSER_CELL_SUBCOMMAND,
        }
    }

    /// Whether `config` is a cell this helper starts: what each helper
    /// checks of its own spec, checked first by the spawner.
    const fn starts(self, config: &CellConfig) -> bool {
        match self {
            Self::AgentRun => config.scheduled.is_some() && !config.is_skill_cell(),
            Self::SkillCell => config.skill_code.is_some(),
            Self::BrowserCell => config.browser.is_some(),
        }
    }
}

/// What the broker asks on a connection.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case", deny_unknown_fields)]
enum Request {
    /// Start `helper` for `spec`, in `run_dir`. Its stdin, stdout and
    /// stderr follow, as three descriptors.
    Helper {
        helper: HelperKind,
        spec: String,
        run_dir: PathBuf,
    },
    /// Kill the helper this connection started.
    Kill,
    /// Give a harness's work trees to its cell's identity, or take them
    /// back.
    Trees {
        give: bool,
        dirs: Vec<PathBuf>,
        principal: u32,
        group: u32,
    },
}

/// What the spawner answers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
enum Reply {
    /// The helper started, as `pid`.
    Started { pid: i32 },
    /// The helper ended, with this raw wait status.
    Exited { status: i32 },
    /// Refused, or failed, and why.
    Refused { reason: String },
    /// The trees were changed.
    Done,
}

/// The broker's end of the spawner.
#[derive(Debug)]
pub struct Spawner {
    /// Carries the connections' ends, one at a time.
    control: Mutex<UnixStream>,
    /// Set once a connection couldn't be handed over: none is again.
    lost: AtomicBool,
    /// Kept so the spawner isn't left a zombie; it leaves when the
    /// broker does.
    _child: Option<Child>,
}

/// The broker's spawner, once started.
static SPAWNER: OnceLock<Spawner> = OnceLock::new();

/// The spawner this broker started, if it started one.
pub(crate) fn spawner() -> Option<&'static Spawner> {
    SPAWNER.get()
}

/// Starts the spawner.
///
/// Call it while the broker has one thread: the spawner's end of its
/// pair is the descriptor its exec keeps.
///
/// # Errors
///
/// As [`Spawner::start_with`], and [`Error::Spawner`] when the broker's
/// binary can't be found or a spawner is already running.
pub fn start() -> Result<(), Error> {
    let program = std::env::current_exe().map_err(|e| Error::Spawner {
        reason: format!("cannot locate the broker binary: {e}"),
    })?;
    let spawner = Spawner::start_with(Command::new(program))?;
    SPAWNER.set(spawner).map_err(|_| Error::Spawner {
        reason: String::from("a spawner is already running"),
    })
}

impl Spawner {
    /// Starts the spawner as `program`, the broker's binary with its
    /// environment, given its subcommand and its end of a new pair.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Spawner`] when it can't be started.
    pub fn start_with(mut program: Command) -> Result<Self, Error> {
        let failed = |reason: String| Error::Spawner { reason };
        let (ours, theirs) =
            UnixStream::pair().map_err(|e| failed(format!("cannot make the socket pair: {e}")))?;
        hatchling_sys::keep_across_exec(theirs.as_fd())
            .map_err(|e| failed(format!("cannot hand the spawner its socket: {e}")))?;
        let child = program
            .arg(SPAWNER_SUBCOMMAND)
            .env(SPAWNER_FD, theirs.as_raw_fd().to_string())
            .spawn()
            .map_err(|e| failed(format!("cannot start: {e}")))?;
        drop(theirs);
        Ok(Self {
            _child: Some(child),
            ..Self::over(ours)
        })
    }

    /// The broker's end, `control`, of a spawner already running.
    #[must_use]
    pub const fn over(control: UnixStream) -> Self {
        Self {
            control: Mutex::new(control),
            lost: AtomicBool::new(false),
            _child: None,
        }
    }

    /// A new connection to the spawner, served on a thread of its own.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Spawner`] when it can't be made or handed over.
    fn connect(&self) -> Result<UnixStream, Error> {
        let (ours, theirs) = UnixStream::pair().map_err(gone)?;
        let control = self.control.lock().map_err(|_| Error::Spawner {
            reason: String::from("an earlier connection to it panicked"),
        })?;
        if self.lost.load(Ordering::SeqCst) {
            return Err(Error::Spawner {
                reason: String::from("lost: a connection couldn't be handed over"),
            });
        }
        let sent = hatchling_sys::net::send_fd(control.as_fd(), theirs.as_fd());
        drop(control);
        if let Err(error) = sent {
            self.lost.store(true, Ordering::SeqCst);
            return Err(gone(error));
        }
        drop(theirs);
        Ok(ours)
    }

    /// Starts `helper` for `spec`, in `run_dir`, on `stdio` (its stdin,
    /// stdout and stderr): the spawner checks the spec, and the helper it
    /// starts is supervised over the connection.
    ///
    /// # Errors
    ///
    /// Why it wasn't started: the spawner refused the spec, or can't be
    /// reached.
    pub fn helper(
        &self,
        helper: HelperKind,
        spec: &str,
        run_dir: &Path,
        stdio: [OwnedFd; 3],
    ) -> Result<SpawnedProcess, String> {
        let stream = self.connect().map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(START_TIMEOUT))
            .map_err(|error| format!("cannot bound the spawner's answer: {error}"))?;
        let request = Request::Helper {
            helper,
            spec: spec.to_owned(),
            run_dir: run_dir.to_path_buf(),
        };
        send(&stream, &request).map_err(|error| error.to_string())?;
        for fd in &stdio {
            hatchling_sys::net::send_fd(stream.as_fd(), fd.as_fd())
                .map_err(|error| gone(error).to_string())?;
        }
        drop(stdio);
        let pid = match receive(&stream).map_err(|error| error.to_string())? {
            Reply::Started { pid } => pid,
            Reply::Refused { reason } => return Err(reason),
            Reply::Exited { .. } | Reply::Done => {
                return Err(String::from("the spawner answered out of step"));
            }
        };
        stream
            .set_read_timeout(None)
            .map_err(|error| format!("cannot wait on the spawner: {error}"))?;
        SpawnedProcess::watching(stream, pid)
    }

    /// Gives `dirs` to the cell `cell`, or takes them back
    /// ([`crate::harness_tree::change_trees`]), in the spawner.
    ///
    /// # Errors
    ///
    /// Why the trees weren't changed.
    pub(crate) fn trees(
        &self,
        op: TreeOp,
        dirs: &[&Path],
        cell: CellIdentity,
    ) -> Result<(), String> {
        let stream = self.connect().map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(TREES_TIMEOUT))
            .map_err(|error| format!("cannot bound the spawner's answer: {error}"))?;
        let request = Request::Trees {
            give: op == TreeOp::Give,
            dirs: dirs.iter().map(|dir| dir.to_path_buf()).collect(),
            principal: cell.principal,
            group: cell.group,
        };
        send(&stream, &request).map_err(|error| error.to_string())?;
        match receive(&stream).map_err(|error| error.to_string())? {
            Reply::Done => Ok(()),
            Reply::Refused { reason } => Err(reason),
            Reply::Started { .. } | Reply::Exited { .. } => {
                Err(String::from("the spawner answered out of step"))
            }
        }
    }
}

/// How a spawned helper ended, once the spawner says: `Err` when the
/// spawner went first. Its watch fills it, and wakes its waiters.
type Ended = Arc<(Mutex<Option<Result<Exit, String>>>, Condvar)>;

/// A helper the spawner started: supervised over its connection. Dropping
/// it kills the helper.
#[derive(Debug)]
pub struct SpawnedProcess {
    /// The helper's pid, for logs.
    pid: i32,
    /// Where the broker asks for a kill.
    stream: Mutex<UnixStream>,
    /// How the helper ended, once the spawner says.
    ended: Ended,
}

impl SpawnedProcess {
    /// Watches the helper `pid` the spawner started on `stream`: a thread
    /// of its own reads how it ended.
    fn watching(stream: UnixStream, pid: i32) -> Result<Self, String> {
        let reader = stream
            .try_clone()
            .map_err(|error| format!("cannot watch helper {pid}: {error}"))?;
        let ended: Ended = Arc::new((Mutex::new(None), Condvar::new()));
        let told = Arc::clone(&ended);
        std::thread::spawn(move || {
            let outcome = match receive(&reader) {
                Ok(Reply::Exited { status }) => Ok(Exit {
                    code: hatchling_sys::child_exit_code(status),
                    status,
                }),
                Ok(Reply::Started { .. } | Reply::Refused { .. } | Reply::Done) => {
                    Err(String::from("the spawner answered out of step"))
                }
                Err(error) => Err(error.to_string()),
            };
            let (slot, changed) = &*told;
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(outcome);
            }
            changed.notify_all();
        });
        Ok(Self {
            pid,
            stream: Mutex::new(stream),
            ended,
        })
    }

    /// The helper's pid, for logs only.
    #[must_use]
    pub const fn pid(&self) -> i32 {
        self.pid
    }

    /// How the helper ended, once known.
    fn outcome(&self) -> Result<Option<Exit>, IsolationError> {
        let (slot, _) = &*self.ended;
        let slot = slot.lock().map_err(|_| wait_error("its watch panicked"))?;
        match slot.as_ref() {
            None => Ok(None),
            Some(Ok(exit)) => Ok(Some(*exit)),
            Some(Err(reason)) => Err(wait_error(reason)),
        }
    }

    /// Waits up to `timeout` for how the helper ended.
    fn ended_within(&self, timeout: Duration) -> Result<Option<Exit>, IsolationError> {
        let (slot, changed) = &*self.ended;
        let ended = changed
            .wait_timeout_while(
                slot.lock().map_err(|_| wait_error("its watch panicked"))?,
                timeout,
                |slot| slot.is_none(),
            )
            .map_err(|_| wait_error("its watch panicked"))?
            .0
            .clone();
        match ended {
            None => Ok(None),
            Some(Ok(exit)) => Ok(Some(exit)),
            Some(Err(reason)) => Err(wait_error(&reason)),
        }
    }
}

impl Supervised for SpawnedProcess {
    fn try_wait(&self) -> Result<Option<Exit>, IsolationError> {
        self.outcome()
    }

    fn wait_deadline(&self, timeout: Duration) -> Result<Exit, IsolationError> {
        if let Some(exit) = self.ended_within(timeout)? {
            return Ok(exit);
        }
        let _ = self.terminate();
        let _ = self.ended_within(REAP_AFTER_KILL);
        Err(wait_error(
            "the helper ran past its deadline, and was killed",
        ))
    }

    fn terminate(&self) -> Result<(), IsolationError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| wait_error("an earlier kill panicked"))?;
        send(&stream, &Request::Kill).map_err(|error| wait_error(&error.to_string()))
    }
}

impl Drop for SpawnedProcess {
    fn drop(&mut self) {
        let _ = self.terminate();
        if let Ok(stream) = self.stream.lock() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn wait_error(reason: &str) -> IsolationError {
    IsolationError::Wait(PlatformError::new(std::io::Error::other(reason.to_owned())))
}

/// A helper's stdin, as its launcher wants it.
#[derive(Debug)]
pub(crate) enum Input {
    /// A descriptor of the launcher's own, handed over.
    Fd(OwnedFd),
    /// A pipe the launcher writes.
    Piped,
}

/// A helper started, wherever: its supervision, and the launcher's ends
/// of the pipes it asked for.
#[derive(Debug)]
pub(crate) struct Started {
    pub process: Process,
    pub stdin: Option<File>,
    pub stdout: Option<File>,
    pub stderr: File,
}

/// Starts `helper` for `spec`, in its run's private `run_dir`: through
/// the spawner when one runs, else as `program` (the broker's own binary),
/// here. Its stdin is `input`; its stdout is piped back when `stdout`,
/// else discarded; its stderr is always piped back.
///
/// # Errors
///
/// Why it wasn't started.
pub(crate) fn start_helper(
    program: &Path,
    helper: HelperKind,
    spec: &str,
    run_dir: &Path,
    input: Input,
    stdout: bool,
) -> Result<Started, String> {
    let pipe = || hatchling_sys::pipe().map_err(|error| format!("cannot make a pipe: {error}"));
    let (their_in, our_in) = match input {
        Input::Fd(fd) => (fd, None),
        Input::Piped => {
            let (read, write) = pipe()?;
            (read, Some(File::from(write)))
        }
    };
    let (our_out, their_out) = if stdout {
        let (read, write) = pipe()?;
        (Some(File::from(read)), write)
    } else {
        let null = File::options()
            .write(true)
            .open("/dev/null")
            .map_err(|error| format!("cannot open /dev/null: {error}"))?;
        (None, OwnedFd::from(null))
    };
    let (our_err, their_err) = pipe()?;
    let process = if let Some(spawner) = spawner() {
        Process::Spawned(spawner.helper(helper, spec, run_dir, [their_in, their_out, their_err])?)
    } else {
        let mut command = Command::new(program);
        command
            .args(spec_args(helper.subcommand(), run_dir, spec)?)
            .env("TMPDIR", run_dir)
            .env(BROKER_PID_ENV, broker_pid())
            .stdin(Stdio::from(their_in))
            .stdout(Stdio::from(their_out))
            .stderr(Stdio::from(their_err));
        let mut std_child =
            crate::skill_run::retry_while_busy(|| command.spawn(), std::thread::sleep).map_err(
                |error| {
                    forget_spec(run_dir);
                    format!("cannot start {}: {error}", program.display())
                },
            )?;
        // The command holds the helper's ends until it's dropped: then
        // the helper's exit is each pipe's end.
        drop(command);
        let process = hatchling_isolation::platform()
            .supervise(&mut std_child)
            .map_err(|error| format!("cannot supervise the helper: {error}"))?;
        Process::Local(process)
    };
    Ok(Started {
        process,
        stdin: our_in,
        stdout: our_out,
        stderr: File::from(our_err),
    })
}

/// Writes `message` as one frame.
fn send(stream: &UnixStream, message: &impl Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(message).map_err(|e| Error::Spawner {
        reason: format!("cannot write a message: {e}"),
    })?;
    write_frame(stream, &bytes).map_err(gone)
}

/// Reads one reply.
fn receive(stream: &UnixStream) -> Result<Reply, Error> {
    let frame = read_frame(stream).map_err(gone)?;
    serde_json::from_slice(&frame).map_err(|e| Error::Spawner {
        reason: format!("an answer that doesn't parse: {e}"),
    })
}

fn gone(error: impl std::fmt::Display) -> Error {
    Error::Spawner {
        reason: format!("cannot be reached: {error}"),
    }
}

/// The spawner: `hatchling-broker spawner`.
#[must_use]
pub fn spawner_main() -> ExitCode {
    let broker = match std::env::var(SPAWNER_FD)
        .ok()
        .and_then(|fd| fd.parse::<i32>().ok())
        .map(hatchling_sys::net::inherited_unix_stream)
    {
        Some(Ok(broker)) => broker,
        Some(Err(error)) => return refuse_to_run(&error.to_string()),
        None => return refuse_to_run("only the broker starts it"),
    };
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => return refuse_to_run(&format!("cannot locate its own binary: {error}")),
    };
    serve(&broker, &Arc::new(program));
    ExitCode::SUCCESS
}

fn refuse_to_run(reason: &str) -> ExitCode {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "hatchling-broker spawner: {reason}");
    ExitCode::FAILURE
}

/// Serves each connection the broker hands over on `broker`, on a thread
/// of its own, until the broker goes. `program` is the binary helpers are
/// started as.
fn serve(broker: &UnixStream, program: &Arc<PathBuf>) {
    while let Ok(fd) = hatchling_sys::net::recv_fd(broker.as_fd()) {
        let program = Arc::clone(program);
        std::thread::spawn(move || {
            let stream = UnixStream::from(fd);
            if let Err(error) = serve_connection(&stream, &program) {
                tracing::warn!(%error, "a connection from the broker ended");
            }
        });
    }
}

/// Answers one connection: one request, then, for a helper, its
/// supervision until it ends or the broker goes.
fn serve_connection(stream: &UnixStream, program: &Path) -> Result<(), Error> {
    let Some(frame) = read_frame_or_eof(stream).map_err(gone)? else {
        return Ok(());
    };
    let request: Request = serde_json::from_slice(&frame).map_err(|e| Error::Spawner {
        reason: format!("a request that doesn't parse: {e}"),
    })?;
    match request {
        Request::Helper {
            helper,
            spec,
            run_dir,
        } => {
            // The descriptors come whatever's decided of the spec, so the
            // stream stays in step.
            let mut stdio = Vec::with_capacity(3);
            for _ in 0..3 {
                stdio.push(hatchling_sys::net::recv_fd(stream.as_fd()).map_err(gone)?);
            }
            serve_helper(stream, program, helper, &spec, &run_dir, stdio)
        }
        Request::Trees {
            give,
            dirs,
            principal,
            group,
        } => {
            let op = if give { TreeOp::Give } else { TreeOp::Reclaim };
            let dirs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
            let cell = CellIdentity {
                principal,
                group,
                count: 1,
                mapping: Mapping::Subordinate,
            };
            let reply = match crate::harness_tree::change_trees(program, op, &dirs, cell) {
                Ok(()) => Reply::Done,
                Err(reason) => Reply::Refused { reason },
            };
            send(stream, &reply)
        }
        Request::Kill => send(
            stream,
            &Reply::Refused {
                reason: String::from("nothing was started to kill"),
            },
        ),
    }
}

/// Starts `helper` for `spec` and supervises it on `stream` until it ends
/// (reporting how) or the broker asks for a kill, or goes.
fn serve_helper(
    stream: &UnixStream,
    program: &Path,
    helper: HelperKind,
    spec: &str,
    run_dir: &Path,
    descriptors: Vec<OwnedFd>,
) -> Result<(), Error> {
    let refuse = |reason: String| send(stream, &Reply::Refused { reason });
    let starts =
        serde_json::from_str::<CellConfig>(spec).is_ok_and(|config| helper.starts(&config));
    if !starts {
        tracing::warn!(?helper, "refused a spec its helper doesn't start");
        return refuse(format!(
            "the spawner refused the spec: not a cell {} starts",
            helper.subcommand()
        ));
    }
    let [stdin, stdout, stderr]: [OwnedFd; 3] = match descriptors.try_into() {
        Ok(stdio) => stdio,
        Err(_) => return refuse(String::from("a helper takes three descriptors")),
    };
    let args = match spec_args(helper.subcommand(), run_dir, spec) {
        Ok(args) => args,
        Err(reason) => return refuse(reason),
    };
    let spawned = Command::new(program)
        .args(args)
        .env("TMPDIR", run_dir)
        // A helper watches its parent, and ends with it: here, the
        // spawner, which ends when the broker does.
        .env(BROKER_PID_ENV, std::process::id().to_string())
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn();
    let std_child = match spawned {
        Ok(child) => child,
        Err(error) => {
            forget_spec(run_dir);
            return refuse(format!("cannot start {}: {error}", program.display()));
        }
    };
    // Just spawned and unreaped: its pid can't name another process.
    let supervised = i32::try_from(std_child.id())
        .ok()
        .and_then(hatchling_sys::ChildPid::new)
        .ok_or_else(|| String::from("its pid isn't one"))
        .and_then(|pid| {
            hatchling_sys::Child::new(pid)
                .map(|child| (child, pid.get()))
                .map_err(|error| error.to_string())
        });
    let (child, pid) = match supervised {
        Ok(supervised) => supervised,
        Err(reason) => {
            let mut std_child = std_child;
            let _ = std_child.kill();
            let _ = std_child.wait();
            return refuse(format!("cannot supervise the helper: {reason}"));
        }
    };
    drop(std_child);
    send(stream, &Reply::Started { pid })?;
    supervise(stream, &child)
}

/// Supervises `child` on `stream`: reports how it ended, and kills it
/// when the broker asks, or goes. One thread watches both, since a
/// child's supervision is one thread's.
fn supervise(stream: &UnixStream, child: &hatchling_sys::Child) -> Result<(), Error> {
    let mut broker_here = true;
    loop {
        if let Some(status) = child.try_wait().map_err(gone)? {
            return if broker_here {
                send(stream, &Reply::Exited { status })
            } else {
                Ok(())
            };
        }
        let fds = if broker_here {
            vec![stream.as_fd(), child.as_fd()]
        } else {
            vec![child.as_fd()]
        };
        let ready = hatchling_sys::wait_readable(&fds).map_err(gone)?;
        if broker_here && ready.first().copied().unwrap_or(false) {
            if let Some(frame) = read_frame_or_eof(stream).map_err(gone)? {
                if matches!(serde_json::from_slice(&frame), Ok(Request::Kill)) {
                    let _ = child.terminate();
                }
            } else {
                // The broker went: so does its helper.
                let _ = child.terminate();
                broker_here = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::time::Instant;

    use super::*;

    /// A spawner serving `program` as every helper, over a pair, and the
    /// broker's end.
    fn serving(program: &str) -> (Spawner, std::thread::JoinHandle<()>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let program = Arc::new(PathBuf::from(program));
        let served = std::thread::spawn(move || serve(&theirs, &program));
        (Spawner::over(ours), served)
    }

    fn run_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-spawner-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A skill cell's spec: one the skill-cell helper starts.
    fn skill_spec() -> String {
        let mut config = CellConfig::skill("spawned").unwrap();
        config.skill_code = Some(PathBuf::from("/nonexistent/code"));
        serde_json::to_string(&config).unwrap()
    }

    /// Stdio for a helper: nothing in, nothing kept out, and stderr's
    /// read end for the test.
    fn stdio() -> ([OwnedFd; 3], File) {
        let null = |write: bool| {
            OwnedFd::from(
                File::options()
                    .read(!write)
                    .write(write)
                    .open("/dev/null")
                    .unwrap(),
            )
        };
        let (read, write) = hatchling_sys::pipe().unwrap();
        ([null(false), null(true), write], File::from(read))
    }

    #[test]
    fn each_helper_starts_its_own_cells() {
        let skill: CellConfig = serde_json::from_str(&skill_spec()).unwrap();
        assert!(HelperKind::SkillCell.starts(&skill));
        assert!(!HelperKind::AgentRun.starts(&skill));
        assert!(!HelperKind::BrowserCell.starts(&skill));
        assert_eq!(HelperKind::AgentRun.subcommand(), "agent-run");
        assert_eq!(HelperKind::SkillCell.subcommand(), "skill-cell");
        assert_eq!(HelperKind::BrowserCell.subcommand(), "browser-cell");
    }

    /// A helper the spawner starts runs with the spec file it wrote, on
    /// the broker's descriptors, and its end comes back over the
    /// connection.
    #[test]
    fn a_helper_runs_on_the_brokers_descriptors_and_its_end_comes_back() {
        // `sh` runs `skill-cell` as a script, which isn't there: it says
        // so on stderr and exits.
        let (spawner, served) = serving("/bin/sh");
        let dir = run_dir("ends");
        let (stdio, mut errors) = stdio();
        let process = spawner
            .helper(HelperKind::SkillCell, &skill_spec(), &dir, stdio)
            .unwrap();
        assert!(process.pid() > 0);
        let exit = process.wait_deadline(Duration::from_secs(10)).unwrap();
        assert_ne!(exit.code, 0);
        assert_eq!(process.try_wait().unwrap(), Some(exit));
        let mut said = String::new();
        errors.read_to_string(&mut said).unwrap();
        assert!(said.contains("skill-cell"), "{said}");
        let written = std::fs::read_to_string(dir.join("cell-spec.json")).unwrap();
        assert!(written.contains("spawned"), "the spawner wrote the spec");
        drop((process, spawner));
        served.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A helper is killed when the broker asks, when its wait runs past
    /// its deadline, and when the broker drops it.
    #[test]
    fn a_helper_is_killed_on_request_on_its_deadline_and_on_drop() {
        // `yes` runs until it's killed.
        let (spawner, served) = serving("/usr/bin/yes");
        let dir = run_dir("kills");
        let start = || {
            let _ = std::fs::remove_file(dir.join("cell-spec.json"));
            let (stdio, _) = stdio();
            spawner
                .helper(HelperKind::SkillCell, &skill_spec(), &dir, stdio)
                .unwrap()
        };
        let asked = start();
        assert_eq!(asked.try_wait().unwrap(), None, "running");
        asked.terminate().unwrap();
        let exit = asked.wait_deadline(Duration::from_secs(10)).unwrap();
        assert_eq!(exit.code, 128 + 9, "killed: {exit:?}");
        let late = start();
        let started = Instant::now();
        assert!(late.wait_deadline(Duration::from_millis(100)).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            late.try_wait().unwrap().is_some(),
            "killed past its deadline"
        );
        let dropped = start();
        let pid = dropped.pid();
        drop(dropped);
        let deadline = Instant::now() + Duration::from_secs(10);
        while hatchling_sys::pid_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!hatchling_sys::pid_alive(pid), "a dropped helper lived on");
        drop((asked, late, spawner));
        served.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The spawner starts nothing for a spec its helper doesn't start,
    /// or for a connection that asks for a kill first; a spawner that's
    /// gone starts nothing again.
    #[test]
    fn the_spawner_refuses_a_spec_its_helper_does_not_start() {
        let (spawner, served) = serving("/bin/sh");
        let dir = run_dir("refused");
        for (helper, spec) in [
            (HelperKind::SkillCell, String::from("{}")),
            (HelperKind::BrowserCell, skill_spec()),
            (HelperKind::AgentRun, skill_spec()),
        ] {
            let (stdio, _) = stdio();
            let refused = spawner.helper(helper, &spec, &dir, stdio).unwrap_err();
            assert!(
                refused.contains("the spawner refused the spec"),
                "{refused}"
            );
        }
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "a refused spec was written"
        );
        let stream = spawner.connect().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        send(&stream, &Request::Kill).unwrap();
        assert!(matches!(receive(&stream).unwrap(), Reply::Refused { .. }));
        drop((stream, spawner));
        served.join().unwrap();
        let (ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        let gone_spawner = Spawner::over(ours);
        let (stdio, _) = stdio();
        assert!(
            gone_spawner
                .helper(HelperKind::SkillCell, &skill_spec(), &dir, stdio)
                .is_err()
        );
        assert!(
            gone_spawner
                .connect()
                .unwrap_err()
                .to_string()
                .contains("lost")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A program the spawner can't start is refused, and leaves no spec;
    /// a malformed request ends its connection.
    #[test]
    fn a_helper_that_cannot_start_leaves_nothing() {
        let (spawner, served) = serving("/nonexistent/hatchling-broker");
        let dir = run_dir("unstartable");
        let (stdio, _) = stdio();
        let refused = spawner
            .helper(HelperKind::SkillCell, &skill_spec(), &dir, stdio)
            .unwrap_err();
        assert!(
            refused.starts_with("cannot start /nonexistent/hatchling-broker"),
            "{refused}"
        );
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        drop(spawner);
        served.join().unwrap();
        let (broker, theirs) = UnixStream::pair().unwrap();
        write_frame(&broker, br#"{"request":"steal"}"#).unwrap();
        assert!(serve_connection(&theirs, Path::new("/bin/sh")).is_err());
        assert!(gone("x").to_string().contains("cannot be reached: x"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A helper's watch reads how it ended, or that the spawner went
    /// first; a wait past its deadline asks for a kill.
    #[test]
    fn a_watch_reads_the_helpers_end_or_the_spawners() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let process = SpawnedProcess::watching(ours, 7).unwrap();
        assert_eq!(process.outcome().unwrap(), None);
        assert_eq!(
            process.ended_within(Duration::from_millis(10)).unwrap(),
            None
        );
        send(&theirs, &Reply::Exited { status: 3 << 8 }).unwrap();
        let exit = process.ended_within(Duration::from_secs(10)).unwrap();
        assert_eq!(exit.map(|exit| exit.code), Some(3));
        assert_eq!(process.try_wait().unwrap().map(|exit| exit.code), Some(3));
        let (ours, theirs) = UnixStream::pair().unwrap();
        let gone_first = SpawnedProcess::watching(ours, 8).unwrap();
        drop(theirs);
        assert!(gone_first.ended_within(Duration::from_secs(10)).is_err());
        assert!(gone_first.outcome().is_err());
        assert!(wait_error("why").to_string().contains("why"));
        // A wait past its deadline asks the spawner for a kill.
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let late = SpawnedProcess::watching(ours, 9).unwrap();
        assert!(late.wait_deadline(Duration::from_millis(10)).is_err());
        let asked = read_frame(&theirs).unwrap();
        assert!(matches!(serde_json::from_slice(&asked), Ok(Request::Kill)));
    }

    /// `serve_helper` and `trees`, called directly: a spec its helper
    /// doesn't start is refused, and so is a tree change the spawner can't
    /// make.
    #[test]
    fn a_spec_or_a_tree_change_the_spawner_cannot_make_is_refused() {
        let (broker, theirs) = UnixStream::pair().unwrap();
        broker
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let (fds, _) = stdio();
        serve_helper(
            &theirs,
            Path::new("/bin/sh"),
            HelperKind::SkillCell,
            "{}",
            Path::new("/nonexistent"),
            fds.into(),
        )
        .unwrap();
        assert!(matches!(receive(&broker).unwrap(), Reply::Refused { .. }));
        let (spawner, served) = serving("/bin/sh");
        let cell = CellIdentity {
            principal: 200_000,
            group: 200_000,
            count: 1,
            mapping: Mapping::Subordinate,
        };
        let refused = spawner
            .trees(TreeOp::Give, &[Path::new("/nonexistent/work")], cell)
            .unwrap_err();
        assert!(!refused.is_empty());
        drop(spawner);
        served.join().unwrap();
    }

    #[test]
    fn the_spawner_runs_only_as_the_broker_starts_it() {
        let failure = format!("{:?}", ExitCode::FAILURE);
        assert_eq!(format!("{:?}", spawner_main()), failure);
        assert_eq!(format!("{:?}", refuse_to_run("why")), failure);
        assert!(spawner().is_none());
    }
}
