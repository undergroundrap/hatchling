//! Skill execution: one invocation, one dedicated skill cell.
//!
//! # Runtime model
//!
//! A skill's entry point is an executable. Each invocation verifies the
//! code against the pinned hash, copies exactly the verified bytes
//! ([`VerifiedCode`](crate::skill_cell::VerifiedCode)), and runs that
//! copy as the skill cell's program under the production mount plan and
//! seccomp profile. The input goes in on stdin; the output comes back on
//! stdout. Interpreted skills (a language runtime and its library tree
//! mounted into the cell) are a later task with their own threat
//! review.
//!
//! # Why a helper process
//!
//! [`spawn_cell`] forks, and the child runs ordinary Rust (allocation,
//! mount setup) before `execve`. That is only sound in a single-threaded
//! process, which is why the agent cell is spawned before the broker
//! starts its connection threads. A skill runs on demand, from a session
//! thread. So the broker never forks there: it starts itself again as
//! `hatchling-broker skill-cell <spec>` ([`SKILL_CELL_SUBCOMMAND`]) with
//! [`std::process::Command`], whose fork-to-exec path is async-signal-
//! safe. The fresh single-threaded helper spawns the cell and exits with
//! the skill's exit code. Its stdin and stdout are the skill's.
//!
//! # Bounds
//!
//! - Input: at most [`MAX_SKILL_INPUT_BYTES`], checked before anything
//!   is spawned, and written on its own thread so a skill that never
//!   reads cannot wedge the broker.
//! - Output: at most [`MAX_SKILL_OUTPUT_BYTES`]. The reader stops one
//!   byte past the cap and the run is refused: output is never truncated
//!   into something that looks complete. It must also be UTF-8.
//! - Time: the whole run, spawn included, must finish within the
//!   runtime's timeout ([`DEFAULT_SKILL_TIMEOUT`] in production). Past
//!   it the helper is `SIGKILL`ed through its pidfd, and the cell dies
//!   with it: every process in the chain is armed with
//!   `PR_SET_PDEATHSIG` against its parent.
//! - Stderr is drained (so the helper can never block on it). A failed
//!   exit carries up to [`STDERR_KEEP`] bytes of it for the broker's log;
//!   it never reaches the agent.
//!
//! # Cleanup
//!
//! The helper's staging tree lives in a per-run directory the broker
//! creates and removes after the helper is reaped, so a killed helper
//! leaks nothing. The verified code copy is removed after that too.
//!
//! # What a skill cell cannot do
//!
//! A skill cell gets no broker socket: its mount plan binds the verified
//! code and the program's shared libraries, nothing else. (A skill cell
//! runs under the same uid mapping as the agent cell, so a mounted
//! socket would pass the broker's peer gate: the skill could speak as
//! the agent.) A skill therefore changes no state the confirmation rails
//! guard: it computes an output from an input, and whatever the model
//! later does with that output goes through the ordinary rails.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use hatchling_core::ui::operator::CellKind;
use hatchling_core::{LogLevel, MAX_SKILL_INPUT_BYTES, MAX_SKILL_OUTPUT_BYTES};
use hatchling_isolation::{IsolationBackend as _, SuperviseError, Supervised};
use hatchling_skill::RegisteredSkill;

use crate::cell::Process;
use crate::skill_cell::{SkillCellConfig, prepare_skill_cell};
use crate::{CellConfig, CellHandle, Error, spawn_cell};

/// The broker subcommand the spawner helper runs as.
pub const SKILL_CELL_SUBCOMMAND: &str = "skill-cell";

/// Production bound on one skill run, spawn included.
pub const DEFAULT_SKILL_TIMEOUT: Duration = Duration::from_secs(30);

/// How much of the helper's stderr a failed run keeps for the broker's
/// log (the skill's own diagnostics, or why the helper could not spawn).
pub const STDERR_KEEP: u64 = 16 * 1024;

/// Bound on reaping the helper after it was killed: the kill is already
/// in flight, so anything past this is a wedged kernel, not a skill.
const REAP_AFTER_KILL: Duration = Duration::from_secs(5);

/// The pauses between attempts to start a program the kernel reports
/// busy: about half a second in all.
///
/// Linux refuses to exec a file that any process holds open for writing
/// (`ETXTBSY`). A program written moments ago can be held that way, for
/// a moment, by an unrelated child that another thread forked while the
/// write was open: the child keeps the descriptor until its own exec
/// closes it. That window is short, so a few doubling pauses outlast it.
const BUSY_PAUSES: [Duration; 8] = [
    Duration::from_millis(2),
    Duration::from_millis(4),
    Duration::from_millis(8),
    Duration::from_millis(16),
    Duration::from_millis(32),
    Duration::from_millis(64),
    Duration::from_millis(128),
    Duration::from_millis(256),
];

/// Runs `attempt` again, after each of [`BUSY_PAUSES`] in turn, while it
/// is refused because the program is busy (`ETXTBSY`); after the last
/// pause that refusal is returned as it is. Any other outcome, success
/// or another error, returns at once.
fn retry_while_busy<T>(
    mut attempt: impl FnMut() -> std::io::Result<T>,
    mut pause: impl FnMut(Duration),
) -> std::io::Result<T> {
    for wait in BUSY_PAUSES {
        match attempt() {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => pause(wait),
            done => return done,
        }
    }
    attempt()
}

/// How the broker runs skills: which program to start as the spawner
/// helper, and how long a run may take.
#[derive(Debug, Clone)]
pub struct SkillRuntime {
    spawner: PathBuf,
    timeout: Duration,
}

impl SkillRuntime {
    /// A runtime that starts `spawner` (the broker's own binary) as the
    /// helper, with the production timeout.
    #[must_use]
    pub const fn new(spawner: PathBuf) -> Self {
        Self {
            spawner,
            timeout: DEFAULT_SKILL_TIMEOUT,
        }
    }

    /// The same runtime with a different bound on each run.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Why a skill run produced no output. The agent only ever sees the
/// opaque skill error; this is for the broker's log and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRunError {
    /// The broker has no runtime for skills, or no package for the id.
    Unavailable(&'static str),
    /// The input exceeds [`MAX_SKILL_INPUT_BYTES`]. Nothing was spawned.
    InputTooLarge {
        /// The input's length in bytes.
        len: usize,
    },
    /// The code failed verification or could not be staged: drift, a
    /// symlink, an unreadable file. Nothing was spawned.
    Prepare(String),
    /// The helper could not be started or supervised.
    Spawn(String),
    /// The run did not finish in time; the helper and cell were killed.
    TimedOut,
    /// The skill wrote more than [`MAX_SKILL_OUTPUT_BYTES`].
    OutputTooLarge,
    /// The output is not UTF-8.
    NotUtf8,
    /// The skill (or the helper, for a spawn failure) exited non-zero.
    Exited {
        /// The exit status.
        code: i32,
        /// The first [`STDERR_KEEP`] bytes of the helper's stderr.
        stderr: String,
    },
}

impl std::fmt::Display for SkillRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(what) => write!(f, "skill unavailable: {what}"),
            Self::InputTooLarge { len } => write!(
                f,
                "skill input is {len} bytes, over the {MAX_SKILL_INPUT_BYTES}-byte cap"
            ),
            Self::Prepare(reason) => write!(f, "skill code refused before spawn: {reason}"),
            Self::Spawn(reason) => write!(f, "skill cell could not be started: {reason}"),
            Self::TimedOut => f.write_str("skill run timed out and was killed"),
            Self::OutputTooLarge => write!(
                f,
                "skill output exceeds the {MAX_SKILL_OUTPUT_BYTES}-byte cap"
            ),
            Self::NotUtf8 => f.write_str("skill output is not UTF-8"),
            Self::Exited { code, stderr } => {
                write!(f, "skill exited with status {code}; stderr: {stderr:?}")
            }
        }
    }
}

/// Runs `skill` once on `input` in a fresh skill cell and returns its
/// output. See the module docs for the bounds.
///
/// `package_dir` is the skill's directory under the skills dir; the
/// entry point is read from it (never through a symlink), checked
/// against the pinned hash, and only the verified copy is run.
///
/// # Errors
///
/// Returns [`SkillRunError`] for every way a run can fail; each is
/// fail-closed, and none leaves a process or a directory behind.
pub fn run_skill(
    runtime: &SkillRuntime,
    skill: &RegisteredSkill,
    package_dir: &Path,
    input: &str,
) -> Result<String, SkillRunError> {
    if input.len() > MAX_SKILL_INPUT_BYTES {
        return Err(SkillRunError::InputTooLarge { len: input.len() });
    }
    let prepare = |err: Error| SkillRunError::Prepare(err.to_string());
    let base = CellConfig::skill("skill").map_err(prepare)?;
    let config =
        SkillCellConfig::new(skill.clone(), package_dir.to_path_buf(), base).map_err(prepare)?;
    let (cell, code) = prepare_skill_cell(&config).map_err(prepare)?;
    let output = run_cell(
        runtime,
        &cell,
        CellKind::Skill,
        input.as_bytes(),
        MAX_SKILL_OUTPUT_BYTES,
    );
    // `run_cell` returns only after the helper is reaped: nothing can
    // still be using the verified copy.
    drop(code);
    String::from_utf8(output?).map_err(|_| SkillRunError::NotUtf8)
}

/// Runs the prepared skill or harness `cell` (its `kind`) through the
/// spawner helper, with `input` on its stdin, and returns its stdout: at
/// most `limit` bytes, or [`SkillRunError::OutputTooLarge`]. The cell is
/// listed for the operator while it runs.
///
/// # Errors
///
/// As [`run_skill`], once the cell is prepared.
pub(crate) fn run_cell(
    runtime: &SkillRuntime,
    cell: &CellConfig,
    kind: CellKind,
    input: &[u8],
    limit: usize,
) -> Result<Vec<u8>, SkillRunError> {
    let _running = crate::machine::Running::start(&cell.cell_id, kind, &cell.limits);
    let (cell, _cgroup) = cell
        .supervised_cgroup()
        .map_err(|error| SkillRunError::Prepare(error.to_string()))?;
    let spec = serde_json::to_string(&cell)
        .map_err(|err| SkillRunError::Prepare(format!("cannot encode the cell spec: {err}")))?;
    let run_dir = RunDir::create().map_err(SkillRunError::Spawn)?;
    let output = supervise(runtime, &spec, run_dir.path(), input, limit);
    // `supervise` returns only after the helper is reaped: nothing can
    // still be using the staging tree.
    drop(run_dir);
    output
}

/// Starts the helper and runs it to completion within the timeout,
/// reading at most `limit` bytes of output.
fn supervise(
    runtime: &SkillRuntime,
    spec: &str,
    run_dir: &Path,
    input: &[u8],
    limit: usize,
) -> Result<Vec<u8>, SkillRunError> {
    let deadline = Instant::now() + runtime.timeout;
    let mut command = Command::new(&runtime.spawner);
    command
        .arg(SKILL_CELL_SUBCOMMAND)
        .arg(spec)
        .env("TMPDIR", run_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut std_child =
        retry_while_busy(|| command.spawn(), std::thread::sleep).map_err(|err| {
            SkillRunError::Spawn(format!("cannot start {}: {err}", runtime.spawner.display()))
        })?;
    let child = supervised(&mut std_child)?;
    let stdin = std_child.stdin.take();
    let stdout = std_child.stdout.take();
    let stderr = std_child.stderr.take();
    // `child` owns supervision (pidfd, bounded waits, kill-on-drop); the
    // std handle neither kills nor waits on drop.
    drop(std_child);

    // A skill that never reads its input cannot wedge the broker: the
    // write happens off this thread, and the pipe closes (the skill's
    // EOF) when the write is done.
    let writer = stdin.map(|mut pipe| {
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        })
    });
    let errors = stderr.map(|mut pipe| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let _ = (&mut pipe).take(STDERR_KEEP).read_to_end(&mut kept);
            // Keep draining so the helper can never block on stderr.
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
            kept
        })
    });
    let (sender, receiver) = mpsc::channel();
    let reader = stdout.map(|pipe| {
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let _ = pipe.take(output_read_limit(limit)).read_to_end(&mut output);
            let _ = sender.send(output);
        })
    });

    let outcome = match receiver.recv_timeout(remaining(deadline)) {
        Err(mpsc::RecvTimeoutError::Timeout) => Err(SkillRunError::TimedOut),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(SkillRunError::Spawn(String::from(
            "the helper's stdout was not captured",
        ))),
        Ok(output) if output.len() > limit => Err(SkillRunError::OutputTooLarge),
        // On a wait timeout the child is already killed and reaped.
        Ok(output) => {
            child
                .wait_deadline(remaining(deadline))
                .map_or(Err(SkillRunError::TimedOut), |exit| match exit.code {
                    0 => Ok(output),
                    code => Err(SkillRunError::Exited {
                        code,
                        stderr: String::new(),
                    }),
                })
        }
    };
    // Every failure kills the helper; the cell dies with it (PDEATHSIG
    // chain), which closes the pipes the threads below are reading.
    if outcome.is_err() {
        let _ = child.terminate();
    }
    let _ = child.wait_deadline(REAP_AFTER_KILL);
    for handle in [writer, reader].into_iter().flatten() {
        let _ = handle.join();
    }
    let kept = errors
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let mut outcome = outcome;
    if let Err(SkillRunError::Exited { stderr, .. }) = &mut outcome {
        *stderr = String::from_utf8_lossy(&kept).into_owned();
    }
    outcome
}

/// One byte past the output cap `limit`: reading that far proves the cap
/// was exceeded without buffering what came after.
fn output_read_limit(limit: usize) -> u64 {
    u64::try_from(limit).map_or(u64::MAX, |cap| cap.saturating_add(1))
}

/// The time left before `deadline`, zero once it has passed.
fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Puts the just-spawned helper under the backend's supervision. If that
/// fails the backend kills and reaps it at once: it is never left
/// unsupervised.
fn supervised(std_child: &mut std::process::Child) -> Result<Process, SkillRunError> {
    hatchling_isolation::platform()
        .supervise(std_child)
        .map_err(|error| match error {
            SuperviseError::InvalidPid => SkillRunError::Spawn(error.to_string()),
            SuperviseError::Refused(source) => {
                SkillRunError::Spawn(format!("cannot supervise: {source}"))
            }
        })
}

/// Disambiguates run directories from one broker process.
static RUN_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The helper's private temp dir, removed (with anything a killed helper
/// left in it) on drop.
struct RunDir {
    dir: PathBuf,
}

impl RunDir {
    fn create() -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        let dir = std::env::temp_dir().join(format!(
            "hatchling-skill-run-{}-{}",
            std::process::id(),
            RUN_DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A fresh directory, never an existing one reused.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|err| format!("cannot create run dir {}: {err}", dir.display()))?;
        Ok(Self { dir })
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for RunDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The helper: `hatchling-broker skill-cell <spec>`.
///
/// Spawns the skill cell the spec describes, waits for it, and exits
/// with the skill's exit code. Runs in a fresh process, before any
/// thread exists, which is what makes the fork inside [`spawn_cell`]
/// sound. Refuses a spec that is not a skill cell: the helper never
/// starts an agent cell, and so never hands a cell the broker socket.
#[must_use]
pub fn skill_cell_main(spec: Option<&str>) -> ExitCode {
    match run_helper(spec) {
        Ok(code) => ExitCode::from(helper_exit_code(code)),
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "hatchling-broker skill-cell: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_helper(spec: Option<&str>) -> Result<i32, Error> {
    let spec = spec.ok_or_else(|| Error::Usage {
        reason: String::from("skill-cell requires a cell spec"),
    })?;
    let config: CellConfig = serde_json::from_str(spec).map_err(|err| Error::Usage {
        reason: format!("skill-cell spec is not a cell config: {err}"),
    })?;
    if config.skill_code.is_none() {
        return Err(Error::Usage {
            reason: String::from("skill-cell spawns skill cells only"),
        });
    }
    let mut cell = spawn_cell(&config, LogLevel::Error)?;
    if let Some(network) = config
        .harness
        .as_ref()
        .and_then(|harness| harness.network.as_ref())
    {
        hand_relay_over(&mut cell, &network.handoff)?;
    }
    Ok(cell.wait_exit()?.code)
}

/// Hands a real CLI's relay listener to the broker, at the unix
/// socket in the broker's private run directory, and keeps no copy. The
/// cell dies with this helper if it cannot.
fn hand_relay_over(cell: &mut CellHandle, handoff: &Path) -> Result<(), Error> {
    let failed = |reason: String| Error::Usage {
        reason: format!("cannot hand the relay to the broker: {reason}"),
    };
    let listener = cell
        .relay()
        .ok_or_else(|| failed(String::from("the cell has no relay listener")))?;
    let channel = std::os::unix::net::UnixStream::connect(handoff)
        .map_err(|error| failed(error.to_string()))?;
    hatchling_sys::net::send_fd(
        std::os::fd::AsFd::as_fd(&channel),
        std::os::fd::AsFd::as_fd(&listener),
    )
    .map_err(|error| failed(error.to_string()))
}

/// The helper's own exit status for a skill exit code: the code itself
/// when it fits a process status, failure otherwise. Never 0 for a
/// non-zero code.
fn helper_exit_code(code: i32) -> u8 {
    u8::try_from(code).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every attempt a busy program needs, and every pause between them.
    fn busy_then(
        busy: usize,
        then: std::io::Result<u8>,
    ) -> (std::io::Result<u8>, usize, Vec<Duration>) {
        let mut attempts = 0;
        let mut pauses = Vec::new();
        let mut then = Some(then);
        let result = retry_while_busy(
            || {
                attempts += 1;
                if attempts <= busy {
                    Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
                } else {
                    then.take().unwrap()
                }
            },
            |wait| pauses.push(wait),
        );
        (result, attempts, pauses)
    }

    #[test]
    fn a_busy_program_is_retried_after_doubling_pauses_then_given_up() {
        let (started, attempts, pauses) = busy_then(2, Ok(7));
        assert_eq!(started.unwrap(), 7);
        assert_eq!(attempts, 3);
        assert_eq!(pauses, BUSY_PAUSES.get(..2).unwrap());

        let (started, attempts, pauses) = busy_then(0, Ok(7));
        assert_eq!(started.unwrap(), 7);
        assert_eq!((attempts, pauses.len()), (1, 0), "no pause when it starts");

        let (refused, attempts, pauses) = busy_then(
            0,
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        );
        assert_eq!(
            refused.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!((attempts, pauses.len()), (1, 0), "only busy is retried");

        let (busy, attempts, pauses) = busy_then(usize::MAX, Ok(7));
        assert_eq!(
            busy.unwrap_err().kind(),
            std::io::ErrorKind::ExecutableFileBusy
        );
        assert_eq!(
            attempts,
            BUSY_PAUSES.len() + 1,
            "one try after the last pause"
        );
        assert_eq!(pauses, BUSY_PAUSES);
        assert_eq!(pauses.iter().sum::<Duration>(), Duration::from_millis(510));
    }

    /// The real kernel case, without timing: the program is held open for
    /// writing (as an unrelated forked child would hold it), so the first
    /// exec is refused `ETXTBSY`; the pause releases it, and the next starts.
    #[test]
    fn a_program_held_open_for_writing_starts_once_it_is_released() {
        use std::os::unix::fs::OpenOptionsExt as _;
        let dir = std::env::temp_dir().join(format!("hatchling-busy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("held");
        let mut held = Some(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o755)
                .open(&program)
                .unwrap(),
        );
        held.as_mut()
            .unwrap()
            .write_all(b"#!/bin/sh\nexit 0\n")
            .unwrap();
        let mut attempts = 0;
        let started = retry_while_busy(
            || {
                attempts += 1;
                Command::new(&program).stdin(Stdio::null()).spawn()
            },
            |wait| {
                drop(held.take());
                std::thread::sleep(wait);
            },
        );
        let status = started.unwrap().wait().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(status.success());
        // At least 2: the first exec is always refused (the file is held);
        // a parallel test's fork can hold it a moment longer, which is the
        // very race the retry is for, so more attempts are allowed.
        assert!(
            attempts >= 2,
            "refused busy while held, started once released: {attempts}"
        );
    }

    #[test]
    fn a_cell_without_a_relay_has_nothing_to_hand_over() {
        let code = std::env::temp_dir().join(format!("hatchling-handover-{}", std::process::id()));
        std::fs::create_dir_all(&code).unwrap();
        let mut config = CellConfig::skill("handover").unwrap();
        config.agent_bin = PathBuf::from("/bin/true");
        config.skill_code = Some(code.clone());
        let mut cell = match spawn_cell(&config, LogLevel::Error) {
            Ok(cell) => cell,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&code);
                let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1");
                assert!(!required, "HATCHLING_REQUIRE_CELL=1 but no cell: {error}");
                return;
            }
        };
        let refused = hand_relay_over(&mut cell, Path::new("/nonexistent/handoff"));
        assert!(
            matches!(
                &refused,
                Err(Error::Usage { reason })
                    if reason == "cannot hand the relay to the broker: the cell has no relay listener"
            ),
            "{refused:?}"
        );
        let _ = cell.wait();
        let _ = std::fs::remove_dir_all(&code);
    }

    /// A registered skill (no package on disk: the input check must fire
    /// before the code is ever read).
    fn registered_skill() -> RegisteredSkill {
        use ed25519_dalek::{Signer, SigningKey};
        let signing = SigningKey::from_bytes(&[0x0d; 32]);
        let manifest = hatchling_skill::Manifest {
            name: String::from("run-skill"),
            version: String::from("1.0.0"),
            capabilities: vec!["text.transform".parse().unwrap()],
            entry_point: String::from("skill"),
        };
        let sig = signing.sign(&manifest.canonical_bytes().unwrap());
        let signed = hatchling_skill::SignedManifest {
            manifest,
            signature: Some(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                sig.to_bytes(),
            )),
        };
        let grants: std::collections::HashSet<hatchling_core::CapabilityName> =
            std::iter::once("text.transform".parse().unwrap()).collect();
        let admission = hatchling_skill::verify_admission(
            &signed,
            &signing.verifying_key().to_bytes(),
            &grants,
        )
        .unwrap();
        let mut registry = hatchling_skill::SkillRegistry::new();
        let id = registry.register(admission, b"code").unwrap();
        registry.get(&id).unwrap().clone()
    }

    /// A helper goes under the backend's supervision, and one
    /// that cannot is reported as a spawn failure, never left running.
    #[test]
    fn a_helper_is_supervised_or_reported() {
        let mut child = Command::new("/bin/true").spawn().unwrap();
        let process = supervised(&mut child).unwrap();
        let exit = process.wait_deadline(Duration::from_secs(10)).unwrap();
        assert_eq!(exit.code, 0);
        let mut reaped = Command::new("/bin/true").spawn().unwrap();
        reaped.wait().unwrap();
        let refused = supervised(&mut reaped).err().unwrap();
        assert!(
            matches!(&refused, SkillRunError::Spawn(reason) if reason.starts_with("cannot supervise: ")),
            "{refused}"
        );
    }

    #[test]
    fn helper_exit_codes_keep_the_skill_status() {
        assert_eq!(helper_exit_code(0), 0);
        assert_eq!(helper_exit_code(3), 3);
        assert_eq!(helper_exit_code(255), 255);
        // Out of range: failure, never success.
        assert_eq!(helper_exit_code(256), 1);
        assert_eq!(helper_exit_code(-1), 1);
    }

    #[test]
    fn the_output_read_limit_is_one_past_the_cap() {
        assert_eq!(
            output_read_limit(MAX_SKILL_OUTPUT_BYTES),
            u64::try_from(MAX_SKILL_OUTPUT_BYTES).unwrap() + 1
        );
        assert_eq!(
            output_read_limit(usize::MAX),
            u64::MAX,
            "saturates, never wraps"
        );
    }

    #[test]
    fn remaining_time_counts_down_to_zero() {
        let later = Instant::now() + Duration::from_secs(60);
        assert!(remaining(later) > Duration::from_secs(50));
        assert_eq!(remaining(Instant::now()), Duration::ZERO);
    }

    #[test]
    fn run_dirs_are_fresh_private_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt as _;
        let first = RunDir::create().unwrap();
        let second = RunDir::create().unwrap();
        assert_ne!(first.path(), second.path());
        let mode = std::fs::metadata(first.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::write(first.path().join("leftover"), b"x").unwrap();
        let path = first.path().to_path_buf();
        drop(first);
        assert!(!path.exists(), "a run dir must not outlive its run");
    }

    #[test]
    fn oversized_input_is_refused_before_anything_is_spawned() {
        // A spawner that does not exist: reaching the spawn would be a
        // Spawn error, not InputTooLarge.
        let runtime = SkillRuntime::new(PathBuf::from("/nonexistent/spawner"));
        let skill = registered_skill();
        let input = "x".repeat(MAX_SKILL_INPUT_BYTES + 1);
        assert_eq!(
            run_skill(&runtime, &skill, Path::new("/nonexistent"), &input),
            Err(SkillRunError::InputTooLarge {
                len: MAX_SKILL_INPUT_BYTES + 1
            })
        );
    }

    #[test]
    fn errors_describe_themselves() {
        for (err, text) in [
            (
                SkillRunError::Unavailable("no runtime"),
                String::from("skill unavailable: no runtime"),
            ),
            (
                SkillRunError::InputTooLarge { len: 9 },
                format!("skill input is 9 bytes, over the {MAX_SKILL_INPUT_BYTES}-byte cap"),
            ),
            (
                SkillRunError::Prepare(String::from("drift")),
                String::from("skill code refused before spawn: drift"),
            ),
            (
                SkillRunError::Spawn(String::from("gone")),
                String::from("skill cell could not be started: gone"),
            ),
            (
                SkillRunError::TimedOut,
                String::from("skill run timed out and was killed"),
            ),
            (
                SkillRunError::OutputTooLarge,
                format!("skill output exceeds the {MAX_SKILL_OUTPUT_BYTES}-byte cap"),
            ),
            (
                SkillRunError::NotUtf8,
                String::from("skill output is not UTF-8"),
            ),
            (
                SkillRunError::Exited {
                    code: 3,
                    stderr: String::from("boom"),
                },
                String::from("skill exited with status 3; stderr: \"boom\""),
            ),
        ] {
            assert_eq!(err.to_string(), text);
        }
    }

    /// A spawner that is a shell script (it ignores the helper's
    /// arguments), staged the way a skill's verified code is.
    fn script(body: &str) -> (crate::skill_cell::VerifiedCode, SkillRuntime) {
        let code = format!(
            "#!/bin/sh
{body}
"
        );
        let staged =
            crate::skill_cell::VerifiedCode::write("spawner", "spawner", code.as_bytes()).unwrap();
        let runtime =
            SkillRuntime::new(staged.dir().join("spawner")).with_timeout(Duration::from_secs(20));
        (staged, runtime)
    }

    #[test]
    fn a_cell_is_listed_for_the_operator_while_it_runs() {
        let (_slow, runtime) = script("sleep 1");
        let cell = CellConfig::skill("listed-harness").unwrap();
        let listed = || {
            crate::machine::cells()
                .iter()
                .any(|seen| seen.cell_id == cell.cell_id && seen.kind == CellKind::Harness)
        };
        std::thread::scope(|scope| {
            let running = scope.spawn(|| run_cell(&runtime, &cell, CellKind::Harness, b"", 1));
            let deadline = Instant::now() + Duration::from_secs(5);
            while !listed() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(listed(), "listed while it runs");
            assert!(running.join().unwrap().is_ok());
        });
        assert!(!listed(), "gone once it returns");
    }

    #[test]
    fn a_run_reads_at_most_its_own_limit() {
        let (_echo, runtime) = script("exec cat");
        let run_dir = RunDir::create().unwrap();
        let echoed = supervise(&runtime, "{}", run_dir.path(), b"hello", 5);
        assert_eq!(echoed.unwrap(), b"hello", "exactly the limit is fine");
        let over = supervise(&runtime, "{}", run_dir.path(), b"hello", 4);
        assert!(
            matches!(over, Err(SkillRunError::OutputTooLarge)),
            "{over:?}"
        );
        let (_fail, failing) = script("exit 3");
        let failed = supervise(&failing, "{}", run_dir.path(), b"", 5);
        assert!(
            matches!(failed, Err(SkillRunError::Exited { code: 3, .. })),
            "{failed:?}"
        );
    }

    #[test]
    fn a_cell_runs_through_the_helper_with_its_input_and_limit() {
        let (_echo, runtime) = script("exec cat");
        let cell = CellConfig::skill("echo").unwrap();
        let big = vec![b'x'; MAX_SKILL_OUTPUT_BYTES * 2];
        assert_eq!(
            run_cell(&runtime, &cell, CellKind::Harness, &big, big.len()).unwrap(),
            big,
            "a harness's cap, above a skill's"
        );
        assert!(matches!(
            run_cell(&runtime, &cell, CellKind::Skill, b"ab", 1),
            Err(SkillRunError::OutputTooLarge)
        ));
        let missing = SkillRuntime::new(PathBuf::from("/nonexistent/hatchling-spawner"));
        assert!(matches!(
            run_cell(&missing, &cell, CellKind::Skill, b"", 1),
            Err(SkillRunError::Spawn(_))
        ));
    }
}
