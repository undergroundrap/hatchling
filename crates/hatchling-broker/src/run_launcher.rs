//! The scheduler's real launcher (-2): one scheduled run, one
//! dedicated cell, one dedicated socket.
//!
//! # The socket is the credential
//!
//! A run cell has the same uid mapping as the broker's agent cell, so the
//! kernel peer gate cannot tell them apart. What separates a run is its
//! socket.
//!
//! - **Where it lives.** Each fire gets a fresh directory `run-<random>`
//!   (mode `0700`) under the scheduler's run root, holding one socket
//!   (`0600`, owner-only, bound under a `077` umask: [`bind_socket`]).
//! - **Who can see it.** That directory is the only broker-side path the
//!   run's cell mounts. No other run's cell, and not the agent cell,
//!   mounts it, and it is removed when the run ends.
//! - **What it grants.** The broker serves exactly one session on it,
//!   under a configuration cut down to the run's grants
//!   ([`scoped_config`]): the schedule's grants minus any revoked at fire
//!   time, with scheduling, skills, channel accounts, and ingress off.
//!   The session code is the same one the agent speaks to, so every
//!   grant check is the one already tested.
//!
//! # Helper, not fork
//!
//! The broker is threaded, so it never forks a cell itself. It starts its
//! own binary again as `hatchling-broker agent-run <spec>`
//! ([`AGENT_RUN_SUBCOMMAND`]) through `Command`, whose fork-to-exec path
//! is async-signal-safe. The fresh single-threaded helper spawns the cell
//! (mounts, id map, seccomp, no-new-privs, every fd above stderr closed:
//! and ), and the cell execs `hatchling-agent run <task>`
//! with the run's `missed` flag and due time in its environment. The
//! agent's stdout is the helper's, captured here as the run's output.
//!
//! # Bounds
//!
//! - **Time.** A run gets [`DEFAULT_RUN_TIMEOUT`] of wall-clock time,
//!   spawn included. Past it the helper is killed through its pidfd, the
//!   cell dies with it (the PDEATHSIG chain), and the run fails. That
//!   bounds how long a hung run can pin its socket. The dispatcher's
//!   overlap policy already skips fires while a run is still active.
//! - **Output.** At most [`MAX_RUN_OUTPUT_BYTES`] of UTF-8 output; more
//!   fails the run rather than being cut into something that looks
//!   complete.
//! - **Errors.** Up to [`STDERR_KEEP`] bytes of stderr are kept for the
//!   failure report.

use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{JoinHandle, Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

use hatchling_channel::LocalHub;
use hatchling_core::{LogLevel, ScheduleGrant, SocketPath};
use hatchling_isolation::{IsolationBackend as _, Supervised};
use hatchling_scheduler::{Dispatcher, Launcher, Run, RunRequest, Timestamp};
use hatchling_vault::MasterKey;

use crate::cell::{Process, ScheduledRun};
use crate::{
    ArtifactConfig, CellConfig, Config, Error, FollowupScope, GoalsAction, MemoryAction,
    MemoryGrant, SchedulerConfig, SkillConfig, SpaceConfig, VaultGrant, bind_socket, dispatch_once,
    serve_connection, spawn_cell,
};

/// The broker subcommand the run helper runs as.
pub const AGENT_RUN_SUBCOMMAND: &str = "agent-run";

/// Wall-clock bound on one scheduled run, spawn included: ten minutes.
///
/// A scheduled run is one agent task through the loop's own step budget.
/// Ten minutes covers a slow model and several tool calls, and still frees
/// a hung run's socket well inside an hourly cadence.
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Most output a run may produce, in bytes.
pub const MAX_RUN_OUTPUT_BYTES: usize = 16 * 1024;

/// How much of a failed run's stderr is kept for its report.
pub const STDERR_KEEP: u64 = 4 * 1024;

/// Seconds past each minute boundary the dispatcher ticks. A fire is due
/// at a whole minute, so the tick after it lands one second in.
pub const TICK_OFFSET_SECS: i64 = 1;

/// Bound on reaping a helper after it was killed.
const REAP_AFTER_KILL: Duration = Duration::from_secs(5);

/// Where and how runs start.
#[derive(Debug, Clone)]
pub struct RunEnv {
    /// The broker's own binary, started as the run helper.
    pub spawner: PathBuf,
    /// The agent binary the run cell executes.
    pub agent_bin: PathBuf,
    /// The directory run directories are made in.
    pub run_root: PathBuf,
    /// The wall-clock bound on each run.
    pub timeout: Duration,
}

impl RunEnv {
    /// The production run environment, if scheduled runs can start: the
    /// scheduler is on, and the agent binary is configured
    /// (`HATCHLING_AGENT_BIN`). The helper is this process's own binary.
    /// `None` means the broker starts no dispatcher.
    #[must_use]
    pub fn from_process(config: &Config) -> Option<Self> {
        if !config.scheduler.enabled() {
            return None;
        }
        Some(Self {
            spawner: std::env::current_exe().ok()?,
            agent_bin: config.scheduler.agent_bin.clone()?,
            run_root: config.scheduler.run_root(),
            timeout: DEFAULT_RUN_TIMEOUT,
        })
    }
}

/// The configuration a run's session is served under: `config` with
/// every grant replaced by `grants`, and every surface that is not a
/// grant switched off.
///
/// Only grants the vocabulary can name survive (a grant that does not
/// parse is dropped, never widened). Channels are limited to granted,
/// credential-less ones. A run cannot create or cancel schedules or
/// hooks, invoke skills, use channel accounts, or reach the ingress.
///
/// Its one scheduler surface is the follow-up scope: asking
/// for one follow-up of itself, within `request`'s grants and depth,
/// claimed under the run's `token`.
#[must_use]
pub fn scoped_config(
    config: &Config,
    request: &RunRequest,
    token: &str,
    socket: SocketPath,
) -> Config {
    let grants = request.grants.as_slice();
    let mut run = config.clone();
    run.socket = socket;
    run.max_connections = 1;
    run.vault.grants = grants
        .iter()
        .filter_map(|grant| match grant {
            ScheduleGrant::Vault { action, name } => Some(VaultGrant {
                action: action.parse().ok()?,
                name: name.parse().ok()?,
            }),
            ScheduleGrant::Egress { .. }
            | ScheduleGrant::Memory { .. }
            | ScheduleGrant::Goals { .. }
            | ScheduleGrant::ChannelSend { .. } => None,
        })
        .collect();
    run.egress.grants = grants
        .iter()
        .filter_map(|grant| match grant {
            ScheduleGrant::Egress { destination } => Some(destination.clone()),
            ScheduleGrant::Vault { .. }
            | ScheduleGrant::Memory { .. }
            | ScheduleGrant::Goals { .. }
            | ScheduleGrant::ChannelSend { .. } => None,
        })
        .collect();
    run.memory.grants = grants
        .iter()
        .filter_map(|grant| match grant {
            ScheduleGrant::Memory { scope, action } => Some(MemoryGrant {
                scope: scope.clone(),
                action: MemoryAction::from_name(action)?,
            }),
            ScheduleGrant::Vault { .. }
            | ScheduleGrant::Egress { .. }
            | ScheduleGrant::Goals { .. }
            | ScheduleGrant::ChannelSend { .. } => None,
        })
        .collect();
    run.goals.grants = grants
        .iter()
        .filter_map(|grant| match grant {
            ScheduleGrant::Goals { action } => GoalsAction::from_name(action),
            ScheduleGrant::Vault { .. }
            | ScheduleGrant::Egress { .. }
            | ScheduleGrant::Memory { .. }
            | ScheduleGrant::ChannelSend { .. } => None,
        })
        .collect();
    let credentialed: Vec<&str> = config
        .channel_accounts
        .iter()
        .map(|account| account.channel.as_str())
        .collect();
    run.channels = config
        .channels
        .iter()
        .filter(|channel| {
            !credentialed.contains(&channel.as_str())
                && grants.contains(&ScheduleGrant::ChannelSend {
                    channel: (*channel).clone(),
                })
        })
        .cloned()
        .collect();
    run.channel_accounts = Vec::new();
    run.channel_ingress = None;
    run.skills = SkillConfig::disabled();
    run.artifacts = ArtifactConfig::disabled();
    // A space task's run may use its own space and no other; any other
    // run has no spaces at all.
    run.spaces = request
        .space
        .as_ref()
        .map_or_else(SpaceConfig::disabled, |space| SpaceConfig {
            root: config.spaces.root.clone(),
            bound: Some(space.clone()),
            device_socket: None,
        });
    run.scheduler = SchedulerConfig {
        followup: Some(FollowupScope {
            root: config.scheduler.root.clone(),
            run: token.to_owned(),
            grants: request.grants.clone(),
            channel: request.channel.clone(),
            depth: request.depth,
        }),
        ..SchedulerConfig::disabled()
    };
    run
}

/// Launches scheduled runs as cells (see the module docs).
///
/// The run sessions are threads on `scope`, the broker's own scope, so
/// they borrow the broker's single master key rather than copying it.
pub struct CellLauncher<'scope, 'env> {
    scope: &'scope Scope<'scope, 'env>,
    config: &'env Config,
    key: &'env MasterKey,
    env: RunEnv,
}

impl std::fmt::Debug for CellLauncher<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellLauncher")
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}

impl<'scope, 'env> CellLauncher<'scope, 'env> {
    /// A launcher for `config`'s runs, serving their sessions on `scope`.
    #[must_use]
    pub const fn new(
        scope: &'scope Scope<'scope, 'env>,
        config: &'env Config,
        key: &'env MasterKey,
        env: RunEnv,
    ) -> Self {
        Self {
            scope,
            config,
            key,
            env,
        }
    }
}

/// One run in flight.
pub struct CellRun<'scope> {
    child: Process,
    started: Instant,
    timeout: Duration,
    output: mpsc::Receiver<Vec<u8>>,
    errors: Option<JoinHandle<Vec<u8>>>,
    session: Option<ScopedJoinHandle<'scope, ()>>,
    stop: Arc<AtomicBool>,
    run_dir: PathBuf,
    token: String,
    done: bool,
}

impl std::fmt::Debug for CellRun<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellRun")
            .field("run_dir", &self.run_dir)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl<'scope> Launcher for CellLauncher<'scope, '_> {
    type Run = CellRun<'scope>;

    fn launch(&self, request: RunRequest) -> Result<CellRun<'scope>, String> {
        let run = ScheduledRun {
            task: request.task.clone(),
            missed: request.missed,
            due: request.due.as_second(),
            model_endpoint: self.config.scheduler.model_endpoint.clone(),
            model_name: self.config.scheduler.model_name.clone(),
            depth: request.depth,
            event: request.event.clone(),
            max_steps: None,
            subagent: false,
        };
        self.launch_cell(&request.schedule_id, run, |token, socket| {
            scoped_config(self.config, &request, token, socket)
        })
    }
}

impl<'scope> CellLauncher<'scope, '_> {
    /// Starts `run` in a fresh cell with its own socket, serving its
    /// session under the configuration `scoped` makes from the run's
    /// token and socket. Shared by scheduled runs and subagents
    ///, so every run cell is the same proven one.
    pub(crate) fn launch_cell(
        &self,
        label: &str,
        run: ScheduledRun,
        scoped: impl FnOnce(&str, SocketPath) -> Config,
    ) -> Result<CellRun<'scope>, String> {
        let token = run_token()?;
        let run_dir = make_run_dir(&self.env.run_root, &token)?;
        let cleanup = |error: String| {
            let _ = std::fs::remove_dir_all(&run_dir);
            error
        };
        let socket_path = run_dir.join(RUN_SOCKET_NAME);
        let socket: SocketPath = socket_path
            .to_string_lossy()
            .parse()
            .map_err(|err| cleanup(format!("run socket path: {err}")))?;
        let listener = bind_socket(&socket, 0o600)
            .map_err(|err| cleanup(format!("cannot bind the run socket: {err}")))?;
        let cell = CellConfig::for_run(
            &self.env.agent_bin,
            &token,
            &socket,
            self.config.agent_uid.as_u32(),
            run,
        )
        .map_err(|err| cleanup(format!("cannot configure the run cell: {err}")))?;
        let spec = serde_json::to_string(&cell)
            .map_err(|err| cleanup(format!("cannot encode the run cell: {err}")))?;
        let run_config = scoped(&token, socket);
        let key = self.key;
        let stop = Arc::new(AtomicBool::new(false));
        let session_stop = Arc::clone(&stop);
        let session = self
            .scope
            .spawn(move || serve_run_session(&listener, &run_config, key, &session_stop));
        let started = Instant::now();
        let spawned = start_helper(&self.env.spawner, &spec, &run_dir);
        let (child, output, errors) = match spawned {
            Ok(parts) => parts,
            Err(error) => {
                stop.store(true, Ordering::Release);
                let _ = session.join();
                return Err(cleanup(error));
            }
        };
        tracing::info!(label = %label, run = %token, "run cell started");
        Ok(CellRun {
            child,
            started,
            timeout: self.env.timeout,
            output,
            errors: Some(errors),
            session: Some(session),
            stop,
            run_dir,
            token,
            done: false,
        })
    }
}

impl Run for CellRun<'_> {
    fn poll(&mut self) -> Option<Result<String, String>> {
        let timeout = self.timeout.as_secs();
        self.poll_end().map(|end| match end {
            RunEnd::Exited {
                code: 0, output, ..
            } => captured(output),
            RunEnd::Exited { code, errors, .. } => Err(format!(
                "the run exited {code}: {}",
                String::from_utf8_lossy(&errors).trim()
            )),
            RunEnd::TimedOut => Err(format!("the run timed out after {timeout} s")),
            RunEnd::Failed(reason) => Err(reason),
        })
    }
}

/// How a run cell ended, as it ended: for callers that need more than
/// the dispatcher's text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunEnd {
    /// The cell exited with `code`, having written `output` and `errors`.
    Exited {
        /// The exit code.
        code: i32,
        /// Everything it wrote to stdout.
        output: Vec<u8>,
        /// The kept stderr.
        errors: Vec<u8>,
    },
    /// It ran past its time bound and was killed.
    TimedOut,
    /// It could not be waited on.
    Failed(String),
}

impl CellRun<'_> {
    /// The run's token: its cell id and directory name.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// How the run ended, once it has; `None` while it runs, and after
    /// its end was taken. Past its timeout it is killed and ends
    /// [`RunEnd::TimedOut`].
    pub(crate) fn poll_end(&mut self) -> Option<RunEnd> {
        if self.done {
            return None;
        }
        // Expired once the elapsed time reaches the timeout: `checked_sub`
        // is `Some` exactly then.
        let expired = self.started.elapsed().checked_sub(self.timeout).is_some();
        match self.child.try_wait() {
            Ok(Some(exit)) => {
                let (output, errors) = self.teardown();
                Some(RunEnd::Exited {
                    code: exit.code,
                    output,
                    errors,
                })
            }
            Ok(None) if !expired => None,
            Ok(None) => {
                let _ = self.child.terminate();
                let _ = self.child.wait_deadline(REAP_AFTER_KILL);
                let _ = self.teardown();
                Some(RunEnd::TimedOut)
            }
            Err(err) => {
                let _ = self.teardown();
                Some(RunEnd::Failed(format!("cannot wait on the run: {err}")))
            }
        }
    }

    /// Kills the run now, and returns what it had written to stdout.
    pub(crate) fn kill(&mut self) -> Vec<u8> {
        if self.done {
            return Vec::new();
        }
        let _ = self.child.terminate();
        let _ = self.child.wait_deadline(REAP_AFTER_KILL);
        self.teardown().0
    }

    /// Ends the run: its session, its output, its directory. Returns
    /// its stdout and kept stderr.
    fn teardown(&mut self) -> (Vec<u8>, Vec<u8>) {
        self.done = true;
        // A run that never connected leaves its session waiting to
        // accept: the flag ends that wait.
        self.stop.store(true, Ordering::Release);
        if let Some(session) = self.session.take() {
            let _ = session.join();
        }
        let output = self
            .output
            .recv_timeout(REAP_AFTER_KILL)
            .unwrap_or_default();
        let errors = self
            .errors
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        let _ = std::fs::remove_dir_all(&self.run_dir);
        // Its session is gone, so it can ask for nothing more.
        crate::schedule_ops::release_followup(&self.token);
        (output, errors)
    }
}

impl Drop for CellRun<'_> {
    fn drop(&mut self) {
        // The session must not outlive the run, whatever `finish` did:
        // an unstopped session would pin the dispatcher's scope.
        self.stop.store(true, Ordering::Release);
        let _ = self.kill();
    }
}

/// A run's output: UTF-8, within the bound, trimmed.
pub(crate) fn captured(output: Vec<u8>) -> Result<String, String> {
    if output.len() > MAX_RUN_OUTPUT_BYTES {
        return Err(format!(
            "the run's output exceeds {MAX_RUN_OUTPUT_BYTES} bytes"
        ));
    }
    String::from_utf8(output)
        .map(|text| text.trim().to_owned())
        .map_err(|_| String::from("the run's output is not UTF-8"))
}

/// Bytes of a run directory's name: `run-` and 24 hex digits, which is
/// 96 random bits. Short, because the run socket's path must fit a unix
/// socket address.
pub const RUN_TOKEN_LEN: usize = 4 + 24;

/// The file name of a run's socket, inside its directory.
pub const RUN_SOCKET_NAME: &str = "broker.sock";

/// A fresh run directory name.
fn run_token() -> Result<String, String> {
    let token = crate::approval::random_hex("run-")
        .map_err(|err| format!("run entropy unavailable: {err}"))?;
    Ok(token.chars().take(RUN_TOKEN_LEN).collect())
}

/// Makes the run's own directory, owner-only, under `root`.
fn make_run_dir(root: &Path, token: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root)
        .and_then(|()| std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)))
        .map_err(|err| format!("cannot prepare the run root {}: {err}", root.display()))?;
    let dir = root.join(token);
    std::fs::create_dir(&dir)
        .and_then(|()| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)))
        .map_err(|err| format!("cannot make the run directory: {err}"))?;
    Ok(dir)
}

/// How often a run's session checks for its connection or its stop.
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// Serves the one session a run gets, or returns once `stop` is set
/// without a connection: a run that never connects must not pin its
/// session thread.
fn serve_run_session(listener: &UnixListener, config: &Config, key: &MasterKey, stop: &AtomicBool) {
    if let Err(error) = listener.set_nonblocking(true) {
        tracing::warn!(%error, "scheduled run socket setup failed");
        return;
    }
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let served = stream
                    .set_nonblocking(false)
                    .map_err(|source| Error::SessionIo { source })
                    .and_then(|()| {
                        serve_connection(&stream, config, key, &crate::Skills::default())
                    });
                if let Err(error) = served {
                    tracing::debug!(%error, "scheduled run session ended");
                }
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(error) => {
                tracing::warn!(%error, "scheduled run socket accept failed");
                return;
            }
        }
    }
}

type Started = (Process, mpsc::Receiver<Vec<u8>>, JoinHandle<Vec<u8>>);

/// Starts the helper under the backend's supervision, with its output
/// captured.
fn start_helper(spawner: &Path, spec: &str, run_dir: &Path) -> Result<Started, String> {
    let mut std_child = Command::new(spawner)
        .arg(AGENT_RUN_SUBCOMMAND)
        .arg(spec)
        .env("TMPDIR", run_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("cannot start {}: {err}", spawner.display()))?;
    // On a refusal the backend has already killed and reaped the helper.
    let child = hatchling_isolation::platform()
        .supervise(&mut std_child)
        .map_err(|error| format!("cannot supervise the run helper: {error}"))?;
    let stdout = std_child.stdout.take();
    let stderr = std_child.stderr.take();
    drop(std_child);
    let (sender, output) = mpsc::channel();
    let limit = u64::try_from(MAX_RUN_OUTPUT_BYTES).map_or(u64::MAX, |cap| cap + 1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = stdout {
            let _ = pipe.take(limit).read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    let errors = std::thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut pipe) = stderr {
            let _ = (&mut pipe).take(STDERR_KEEP).read_to_end(&mut kept);
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        }
        kept
    });
    Ok((child, output, errors))
}

/// The time until the next tick: [`TICK_OFFSET_SECS`] past the next
/// minute boundary after `now`.
#[must_use]
pub fn until_next_tick(now: Timestamp) -> Duration {
    let second = now.as_second();
    let next = (second.div_euclid(60) + 1) * 60 + TICK_OFFSET_SECS;
    let whole = u64::try_from(next - second).unwrap_or(0);
    let nanos = u64::try_from(now.subsec_nanosecond()).unwrap_or(0);
    Duration::from_secs(whole).saturating_sub(Duration::from_nanos(nanos))
}

/// The dispatcher loop: tick after each minute boundary, forever, with
/// the real launcher, store, and delivery.
pub fn dispatch_forever<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    config: &'env Config,
    key: &'env MasterKey,
    hub: Option<&'env LocalHub>,
    env: RunEnv,
) -> ! {
    // Feed runs are answered by the broker; every other run
    // gets its own cell.
    let launcher =
        crate::feed_run::FeedLauncher::new(config, CellLauncher::new(scope, config, key, env));
    let mut dispatcher = Dispatcher::default();
    loop {
        std::thread::sleep(until_next_tick(Timestamp::now()));
        match dispatch_once(
            &mut dispatcher,
            config,
            key,
            hub,
            &launcher,
            Timestamp::now(),
        ) {
            Ok(report) => tracing::debug!(?report, "scheduler tick"),
            Err(error) => tracing::error!(%error, "scheduler tick failed"),
        }
    }
}

/// The `agent-run` helper: spawns one run cell and exits with its code.
#[must_use]
pub fn agent_run_main(spec: Option<&str>) -> ExitCode {
    match run_helper(spec) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "hatchling-broker agent-run: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_helper(spec: Option<&str>) -> Result<i32, Error> {
    let spec = spec.ok_or_else(|| Error::Usage {
        reason: String::from("agent-run requires a cell spec"),
    })?;
    let config: CellConfig = serde_json::from_str(spec).map_err(|err| Error::Usage {
        reason: format!("agent-run spec is not a cell config: {err}"),
    })?;
    if config.scheduled.is_none() || config.is_skill_cell() {
        return Err(Error::Usage {
            reason: String::from("agent-run spawns scheduled run cells only"),
        });
    }
    Ok(spawn_cell(&config, LogLevel::Error)?.wait_exit()?.code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelAccountSpec, EgressConfig, GoalsConfig, MemoryConfig, VaultConfig};
    use hatchling_core::{PeerUid, VaultAction};

    fn config() -> Config {
        let root = PathBuf::from("/srv/hatchling/vault");
        Config {
            socket: "/run/hatchling/sock/broker.sock".parse().unwrap(),
            agent_uid: PeerUid::from_kernel(1000),
            socket_mode: 0o600,
            max_connections: 16,
            scheduler: SchedulerConfig {
                root: PathBuf::from("/srv/hatchling/schedules"),
                ..SchedulerConfig::disabled()
            },
            log_level: LogLevel::Error,
            vault: VaultConfig {
                key_file: VaultConfig::default_key_file(&root),
                root,
                grants: vec![
                    VaultGrant {
                        action: VaultAction::Fetch,
                        name: "app-token".parse().unwrap(),
                    },
                    VaultGrant {
                        action: VaultAction::Store,
                        name: "notes".parse().unwrap(),
                    },
                ],
            },
            egress: EgressConfig {
                allowlist: crate::EgressAllowlist::new("api.example.com:443"),
                grants: vec![
                    String::from("api.example.com:443"),
                    String::from("b.example:1"),
                ],
            },
            channels: vec![String::from("local"), String::from("mail")],
            channel_accounts: vec![ChannelAccountSpec {
                channel: String::from("mail"),
                label: String::from("work"),
                secret: "mail-work".parse().unwrap(),
            }],
            channel_ingress: Some("/run/hatchling/ingress.sock".parse().unwrap()),
            skills: SkillConfig::disabled(),
            memory: MemoryConfig {
                root: PathBuf::from("/srv/hatchling/memory"),
                grants: vec![MemoryGrant {
                    scope: String::from("user"),
                    action: MemoryAction::Recall,
                }],
            },
            artifacts: ArtifactConfig::disabled(),
            spaces: SpaceConfig::disabled(),
            goals: GoalsConfig {
                root: PathBuf::from("/srv/hatchling/goals"),
                grants: vec![GoalsAction::GoalCreate, GoalsAction::QueryGoals],
            },
        }
    }

    fn run_socket() -> SocketPath {
        "/srv/hatchling/schedules.runs/run-1/broker.sock"
            .parse()
            .unwrap()
    }

    /// A run request as the dispatcher builds one.
    fn request(grants: Vec<ScheduleGrant>) -> RunRequest {
        RunRequest {
            schedule_id: String::from("sch-1"),
            task: String::from("t"),
            grants,
            due: Timestamp::from_second(1_790_000_000).unwrap(),
            missed: false,
            channel: String::from("local"),
            depth: 3,
            event: None,
            space: None,
            feed: None,
        }
    }

    #[test]
    fn a_run_session_holds_exactly_its_grants_and_nothing_else() {
        let config = config();
        let grants = vec![
            ScheduleGrant::Vault {
                action: String::from("fetch"),
                name: String::from("app-token"),
            },
            ScheduleGrant::Egress {
                destination: String::from("api.example.com:443"),
            },
            ScheduleGrant::Memory {
                scope: String::from("user"),
                action: String::from("recall"),
            },
            ScheduleGrant::Goals {
                action: String::from("query-goals"),
            },
            ScheduleGrant::ChannelSend {
                channel: String::from("local"),
            },
            // Credentialed: a run has no accounts, so it gets no such channel.
            ScheduleGrant::ChannelSend {
                channel: String::from("mail"),
            },
        ];
        let run = scoped_config(&config, &request(grants.clone()), "run-1", run_socket());
        assert_eq!(run.socket, run_socket());
        assert_eq!(run.max_connections, 1);
        assert_eq!(
            crate::held_grants(&run),
            grants
                .iter()
                .filter(|grant| !matches!(grant, ScheduleGrant::ChannelSend { channel } if channel == "mail"))
                .cloned()
                .collect()
        );
        assert_eq!(run.channels, ["local"]);
        assert!(run.channel_accounts.is_empty());
        assert!(run.channel_ingress.is_none());
        assert!(!run.scheduler.enabled(), "a run cannot schedule");
        // Its one scheduler surface: its own follow-up, within its own
        // grants and depth, recorded in the broker's store.
        assert_eq!(
            run.scheduler.followup,
            Some(FollowupScope {
                root: config.scheduler.root.clone(),
                run: String::from("run-1"),
                grants,
                channel: String::from("local"),
                depth: 3,
            })
        );
        assert!(
            config.scheduler.followup.is_none(),
            "the broker's own has none"
        );
        assert!(
            run.skills.skills_dir.as_os_str().is_empty(),
            "a run invokes no skills"
        );
        // Not grants: kept as the operator set them.
        assert_eq!(run.egress.allowlist.raw(), config.egress.allowlist.raw());
        assert_eq!(run.vault.root, config.vault.root);
        assert_eq!(run.agent_uid, config.agent_uid);
    }

    #[test]
    fn only_a_space_tasks_run_has_a_space_and_only_its_own() {
        let mut broker = config();
        broker.spaces = SpaceConfig {
            root: PathBuf::from("/srv/hatchling/spaces"),
            bound: None,
            device_socket: None,
        };
        // An ordinary run: no spaces at all.
        let plain = scoped_config(&broker, &request(Vec::new()), "run-1", run_socket());
        assert!(!plain.spaces.enabled());
        assert_eq!(plain.spaces.bound, None);
        // A space task's run: the broker's store, bound to its one space.
        let mut task = request(Vec::new());
        task.space = Some(String::from("spc-1"));
        let run = scoped_config(&broker, &task, "run-1", run_socket());
        assert_eq!(run.spaces.root, PathBuf::from("/srv/hatchling/spaces"));
        assert_eq!(run.spaces.bound.as_deref(), Some("spc-1"));
        // With spaces off at the broker, the task's run has none either.
        let off = scoped_config(&config(), &task, "run-1", run_socket());
        assert!(!off.spaces.enabled());
    }

    #[test]
    fn a_run_with_no_grants_holds_nothing() {
        let run = scoped_config(&config(), &request(Vec::new()), "run-1", run_socket());
        assert!(crate::held_grants(&run).is_empty());
        assert!(run.channels.is_empty());
    }

    #[test]
    fn grants_that_do_not_parse_are_dropped_never_widened() {
        let grants = vec![
            ScheduleGrant::Vault {
                action: String::from("erase"),
                name: String::from("app-token"),
            },
            ScheduleGrant::Vault {
                action: String::from("fetch"),
                name: String::from("../etc"),
            },
            ScheduleGrant::Memory {
                scope: String::from("user"),
                action: String::from("erase"),
            },
            ScheduleGrant::Goals {
                action: String::from("goal-delete"),
            },
        ];
        let run = scoped_config(&config(), &request(grants), "run-1", run_socket());
        assert!(crate::held_grants(&run).is_empty());
    }

    #[test]
    fn ticks_land_one_second_past_each_minute() {
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        assert_eq!(
            until_next_tick(at("2026-09-21T12:00:00Z")),
            Duration::from_secs(61)
        );
        assert_eq!(
            until_next_tick(at("2026-09-21T12:00:59Z")),
            Duration::from_secs(2)
        );
        assert_eq!(
            until_next_tick(at("2026-09-21T12:00:01Z")),
            Duration::from_secs(60)
        );
        assert_eq!(
            until_next_tick(at("2026-09-21T12:00:30.25Z")),
            Duration::from_millis(30_750)
        );
        assert_eq!(TICK_OFFSET_SECS, 1);
    }

    #[test]
    fn output_is_utf8_bounded_and_trimmed() {
        assert_eq!(captured(b"  done\n".to_vec()), Ok(String::from("done")));
        assert_eq!(
            captured(vec![b'x'; MAX_RUN_OUTPUT_BYTES]).map(|s| s.len()),
            Ok(MAX_RUN_OUTPUT_BYTES)
        );
        assert!(
            captured(vec![b'x'; MAX_RUN_OUTPUT_BYTES + 1])
                .unwrap_err()
                .contains("exceeds")
        );
        assert!(captured(vec![0xff, 0xfe]).unwrap_err().contains("UTF-8"));
        assert_eq!((MAX_RUN_OUTPUT_BYTES, STDERR_KEEP), (16 * 1024, 4 * 1024));
        assert_eq!(DEFAULT_RUN_TIMEOUT, Duration::from_secs(600));
    }

    /// A fresh, owner-only directory for one helper test.
    fn helper_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-helper-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A run around a helper `start_helper` started, as `launch` builds
    /// it, but with no session.
    fn helper_run(spawner: &Path, dir: &Path, timeout: Duration) -> CellRun<'static> {
        let (child, output, errors) = start_helper(spawner, "{}", dir).unwrap();
        CellRun {
            child,
            started: Instant::now(),
            timeout,
            output,
            errors: Some(errors),
            session: None,
            stop: Arc::new(AtomicBool::new(false)),
            run_dir: dir.to_path_buf(),
            token: String::from("helper-test"),
            done: false,
        }
    }

    /// A helper started under the backend's supervision, polled
    /// to its end: `/bin/sh agent-run {}` finds no script called
    /// `agent-run` and exits non-zero. The end is taken once, and the run's
    /// directory goes with it.
    #[test]
    fn a_started_helper_is_polled_to_its_exit_code() {
        let dir = helper_dir("exit");
        let mut run = helper_run(Path::new("/bin/sh"), &dir, Duration::from_secs(30));
        let deadline = Instant::now() + Duration::from_secs(10);
        let end = loop {
            if let Some(end) = run.poll_end() {
                break end;
            }
            assert!(Instant::now() < deadline, "the helper never ended");
            std::thread::sleep(Duration::from_millis(10));
        };
        // dash, Ubuntu's `/bin/sh`, exits 2 on a script it cannot open.
        assert!(
            matches!(&end, RunEnd::Exited { code: 2, errors, .. }
                if String::from_utf8_lossy(errors).contains("agent-run")),
            "{end:?}"
        );
        assert_eq!(run.poll_end(), None, "an end is taken once");
        assert!(!dir.exists(), "the run directory goes with the run");
    }

    #[test]
    fn a_helper_past_its_time_is_killed() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = helper_dir("linger");
        let script = dir.join("linger");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut run = helper_run(&script, &dir, Duration::ZERO);
        assert_eq!(run.poll_end(), Some(RunEnd::TimedOut));
        assert!(!dir.exists());
    }

    #[test]
    fn a_helper_that_cannot_start_is_reported() {
        let error = start_helper(Path::new("/nonexistent/spawner"), "{}", Path::new("/tmp"))
            .err()
            .unwrap();
        assert!(
            error.starts_with("cannot start /nonexistent/spawner"),
            "{error}"
        );
    }

    #[test]
    fn the_helper_spawns_run_cells_only() {
        assert!(matches!(run_helper(None), Err(Error::Usage { .. })));
        assert!(matches!(
            run_helper(Some("not json")),
            Err(Error::Usage { .. })
        ));
        let agent = serde_json::to_string(&CellConfig::skill("cell-x").unwrap()).unwrap();
        let err = run_helper(Some(&agent)).unwrap_err();
        assert!(
            format!("{err}").contains("scheduled run cells only"),
            "{err}"
        );
        let mut skill = CellConfig::skill("cell-x").unwrap();
        skill.skill_code = Some(PathBuf::from("/tmp/code"));
        skill.scheduled = Some(ScheduledRun {
            task: String::from("t"),
            missed: false,
            due: 0,
            model_endpoint: None,
            model_name: None,
            depth: 0,
            event: None,
            max_steps: None,
            subagent: false,
        });
        let err = run_helper(Some(&serde_json::to_string(&skill).unwrap())).unwrap_err();
        assert!(
            format!("{err}").contains("scheduled run cells only"),
            "{err}"
        );
        assert_eq!(AGENT_RUN_SUBCOMMAND, "agent-run");
    }

    #[test]
    fn run_tokens_are_short_distinct_and_cell_ids() {
        let a = run_token().unwrap();
        let b = run_token().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), RUN_TOKEN_LEN);
        assert!(a.starts_with("run-"));
        assert!(a.parse::<hatchling_core::CellId>().is_ok(), "{a}");
        assert_eq!((RUN_TOKEN_LEN, RUN_SOCKET_NAME), (28, "broker.sock"));
    }

    #[test]
    fn a_run_directory_is_fresh_and_owner_only() {
        let root =
            std::env::temp_dir().join(format!("hatchling-test-runroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = make_run_dir(&root, "run-abc").unwrap();
        assert_eq!(dir, root.join("run-abc"));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&dir), 0o700);
        assert!(make_run_dir(&root, "run-abc").is_err(), "never reused");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_accept_ends_the_session_without_waiting_for_stop() {
        // A connected stream socket is no listener: accept fails with
        // EINVAL, not WouldBlock, so the session ends at once instead of
        // retrying until its run stops it.
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let listener = UnixListener::from(std::os::fd::OwnedFd::from(socket));
        let config = config();
        let key = MasterKey::generate().unwrap();
        let stop = AtomicBool::new(false);
        let (ended, done) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                serve_run_session(&listener, &config, &key, &stop);
                let _ = ended.send(());
            });
            let returned = done.recv_timeout(Duration::from_secs(2));
            stop.store(true, Ordering::Release);
            assert!(
                returned.is_ok(),
                "the session kept retrying a failed accept"
            );
        });
    }

    #[test]
    fn a_run_environment_needs_a_scheduler_and_an_agent_binary() {
        let mut off = config();
        off.scheduler = SchedulerConfig::disabled();
        off.scheduler.agent_bin = Some(PathBuf::from("/usr/lib/hatchling/hatchling-agent"));
        assert!(
            RunEnv::from_process(&off).is_none(),
            "no scheduler, no runs"
        );
        let no_agent = config();
        assert!(
            RunEnv::from_process(&no_agent).is_none(),
            "no agent binary, no runs"
        );
        let mut on = config();
        on.scheduler.agent_bin = Some(PathBuf::from("/usr/lib/hatchling/hatchling-agent"));
        let env = RunEnv::from_process(&on).unwrap();
        assert_eq!(
            env.agent_bin,
            PathBuf::from("/usr/lib/hatchling/hatchling-agent")
        );
        assert_eq!(env.run_root, PathBuf::from("/srv/hatchling/schedules.runs"));
        assert_eq!(env.timeout, DEFAULT_RUN_TIMEOUT);
        assert_eq!(env.spawner, std::env::current_exe().unwrap());
    }
}
