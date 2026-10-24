//! A browser cell's launch (WO-029, part 2): the cell, its controller's
//! connection, and the proxy on its relay.
//!
//! # Helper, not fork
//!
//! A browser session starts from a session thread, so its cell is spawned
//! the way a skill's or a run's is: the broker starts its own binary again
//! as `hatchling-broker browser-cell <spec-file>` ([`BROWSER_CELL_SUBCOMMAND`]),
//! a fresh single-threaded helper. The helper hands the broker two things
//! and keeps neither: the controller's connection, on the channel that is
//! its stdin, and the cell's relay listener, at the handoff socket in the
//! cell's own run directory. Then it waits for the cell.
//!
//! # What the broker holds
//!
//! A [`BrowserCell`]: the connection, which carries one command and its
//! reply at a time ([`BrowserCell::command`]); the proxy on the relay
//! ([`crate::browser_proxy`]), the cell's only way out; the helper, under
//! the backend's supervision; and the cell's resource group. However the
//! cell ends, its helper is killed and the cell dies with it, then the
//! group and the run directory are removed.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hatchling_core::LogLevel;
use hatchling_core::browser::{BrowserCommand, BrowserReply};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::ui::operator::CellKind;
use hatchling_isolation::{IsolationBackend as _, Supervised as _};

use crate::browser_bundle::Snapshot;
use crate::browser_proxy::{BrowserProxy, ProxyCount, Route, Sites};
use crate::browser_session::{BrowserSettings, browser_limits};
use crate::cell::Process;
use crate::harness_relay::HANDOFF_TIMEOUT;
use crate::run_launcher::{BROKER_PID_ENV, broker_pid};
use crate::skill_run::{REAP_AFTER_KILL, RunDir, STDERR_KEEP};
use crate::{CellConfig, CellIds, Error, spawn_cell};

/// The broker subcommand the browser cell's helper runs as.
pub const BROWSER_CELL_SUBCOMMAND: &str = "browser-cell";

/// How long the controller has to answer one command: past its own bound
/// on a page load, so its own `Timeout` reply comes first.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(90);

/// How long a browser told to close has to answer and exit before its
/// helper is killed.
pub const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// What browser cells start from: the broker's settings, chosen once, and
/// the snapshot of the browser's bundle, made once.
#[derive(Debug, Clone)]
pub struct BrowserLauncher {
    spawner: PathBuf,
    settings: BrowserSettings,
    ids: CellIds,
    cgroup: PathBuf,
    route: Route,
    lifetime: Duration,
    snapshot: Arc<Mutex<Option<Arc<Snapshot>>>>,
}

impl BrowserLauncher {
    /// Starts `spawner` (the broker's own binary) as each cell's helper,
    /// for the browser `settings` names, on the ids `ids`, in a group of
    /// its own under the delegated `cgroup`. A cell's sites are found by
    /// `route`, and it lives at most `lifetime`.
    #[must_use]
    pub fn new(
        spawner: PathBuf,
        settings: BrowserSettings,
        ids: CellIds,
        cgroup: PathBuf,
        route: Route,
        lifetime: Duration,
    ) -> Self {
        Self {
            spawner,
            settings,
            ids,
            cgroup,
            route,
            lifetime,
            snapshot: Arc::new(Mutex::new(None)),
        }
    }

    /// The snapshot of the browser's bundle: made and checked the first
    /// time, and the same one after.
    ///
    /// # Errors
    ///
    /// As [`BrowserSettings::snapshot`]: the next start tries again.
    pub fn snapshot(&self) -> Result<Arc<Snapshot>, String> {
        let mut kept = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(snapshot) = kept.as_ref() {
            return Ok(Arc::clone(snapshot));
        }
        let snapshot = Arc::new(self.settings.snapshot()?);
        *kept = Some(Arc::clone(&snapshot));
        drop(kept);
        Ok(snapshot)
    }

    /// The cell `cell_id`: the controller, running the browser from
    /// `snapshot`, never from the operator's directory, with its relay
    /// handed over at `handoff`.
    fn cell_config(
        &self,
        cell_id: &str,
        snapshot: &Snapshot,
        handoff: PathBuf,
    ) -> Result<CellConfig, String> {
        CellConfig::browser(
            cell_id,
            self.ids,
            self.settings.controller().to_path_buf(),
            self.settings.dirs(snapshot.dir(), handoff),
            browser_limits(),
            self.cgroup.clone(),
        )
        .map_err(|error| error.to_string())
    }

    /// Starts the browser cell `cell_id`, reaching only the `sites` its
    /// session holds, from the snapshot of the pinned bundle.
    ///
    /// # Errors
    ///
    /// A sentence for the broker's log: the browser isn't the pinned one,
    /// the cell couldn't be made or started, or it didn't hand its
    /// connection and relay over in time. Nothing is left running.
    pub fn start(&self, cell_id: &str, sites: Sites) -> Result<BrowserCell, String> {
        let snapshot = self.snapshot()?;
        let run_dir = RunDir::create()?;
        let handoff_path = run_dir.path().join("relay.sock");
        let handoff = UnixListener::bind(&handoff_path)
            .map_err(|error| format!("cannot make the relay handoff: {error}"))?;
        let config = self.cell_config(cell_id, &snapshot, handoff_path)?;
        let running = crate::machine::Running::start(cell_id, CellKind::Browser, &config.limits);
        let (config, group) = config
            .supervised_cgroup()
            .map_err(|error| error.to_string())?;
        let spec = serde_json::to_string(&config)
            .map_err(|error| format!("cannot encode the cell spec: {error}"))?;
        let (channel, theirs) = UnixStream::pair()
            .map_err(|error| format!("cannot make the handoff channel: {error}"))?;
        let helper = Helper::start(&self.spawner, &spec, run_dir.path(), theirs)?;
        let deadline = Instant::now() + HANDOFF_TIMEOUT;
        let connection = match receive_connection(&channel) {
            Ok(connection) => connection,
            Err(reason) => return Err(helper.failed(&reason)),
        };
        let listener =
            match crate::harness_relay::receive(&handoff, deadline, &AtomicBool::new(false)) {
                Ok(listener) => listener,
                Err(reason) => return Err(helper.failed(&reason)),
            };
        if let Err(error) = connection
            .set_read_timeout(Some(REPLY_TIMEOUT))
            .and_then(|()| connection.set_write_timeout(Some(REPLY_TIMEOUT)))
        {
            return Err(helper.failed(&format!("the browser cell's connection: {error}")));
        }
        let deadline = Instant::now() + self.lifetime;
        let proxy = BrowserProxy::start(listener, cell_id, self.route.clone(), sites, deadline);
        Ok(BrowserCell {
            commands: Commands::new(connection, deadline),
            proxy: Some(proxy),
            helper,
            _group: group,
            _running: running,
            _run_dir: run_dir,
            _snapshot: snapshot,
        })
    }
}

/// A running browser cell, as the broker holds it. Dropped, it ends: its
/// connection closes, its proxy stops, and its helper is killed, with the
/// cell; then its group and run directory are removed.
pub struct BrowserCell {
    // Dropped in this order.
    commands: Commands,
    proxy: Option<BrowserProxy>,
    helper: Helper,
    _group: Option<hatchling_sys::cgroup::CellCgroup>,
    _running: crate::machine::Running,
    _run_dir: RunDir,
    _snapshot: Arc<Snapshot>,
}

impl std::fmt::Debug for BrowserCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserCell")
            .field("broken", &self.commands.broken)
            .finish_non_exhaustive()
    }
}

impl BrowserCell {
    /// Sends `command` to the controller, and returns its reply.
    ///
    /// # Errors
    ///
    /// A sentence for the broker's log: the cell is past its lifetime, or
    /// the connection broke, timed out or carried something that isn't a
    /// reply. After any of these the cell takes no more commands: a reply
    /// that came late would answer the wrong one.
    pub fn command(&mut self, command: &BrowserCommand) -> Result<BrowserReply, String> {
        self.commands.send(command)
    }

    /// Closes the browser and ends the cell, and says what its proxy did.
    /// A browser that doesn't close within [`CLOSE_GRACE`] is killed.
    #[must_use]
    pub fn finish(mut self) -> ProxyCount {
        if self
            .commands
            .connection
            .set_read_timeout(Some(CLOSE_GRACE))
            .is_ok()
        {
            let _ = self.command(&BrowserCommand::Close);
        }
        let _ = self.helper.process.wait_deadline(CLOSE_GRACE);
        self.proxy
            .take()
            .map_or_else(ProxyCount::default, BrowserProxy::finish)
    }
}

/// The broker's end of the controller's connection: one command and its
/// reply at a time, until the cell's lifetime ends or an exchange fails.
struct Commands {
    connection: UnixStream,
    deadline: Instant,
    broken: bool,
}

impl Commands {
    const fn new(connection: UnixStream, deadline: Instant) -> Self {
        Self {
            connection,
            deadline,
            broken: false,
        }
    }

    /// As [`BrowserCell::command`].
    fn send(&mut self, command: &BrowserCommand) -> Result<BrowserReply, String> {
        if self.broken {
            return Err(String::from("the browser cell takes no more commands"));
        }
        let exchanged = if self
            .deadline
            .saturating_duration_since(Instant::now())
            .is_zero()
        {
            Err(String::from("the browser cell is past its lifetime"))
        } else {
            exchange(&self.connection, command)
        };
        self.broken = exchanged.is_err();
        exchanged
    }
}

/// `command` sent on `connection`, and the reply read back.
fn exchange(connection: &UnixStream, command: &BrowserCommand) -> Result<BrowserReply, String> {
    let bytes = serde_json::to_vec(command)
        .map_err(|error| format!("cannot encode a browser command: {error}"))?;
    write_frame(connection, &bytes)
        .map_err(|error| format!("cannot send the browser a command: {error}"))?;
    let frame =
        read_frame(connection).map_err(|error| format!("no answer from the browser: {error}"))?;
    serde_json::from_slice(&frame)
        .map_err(|error| format!("the browser's answer isn't a reply: {error}"))
}

/// The controller's connection, which the helper sends on `channel`.
fn receive_connection(channel: &UnixStream) -> Result<UnixStream, String> {
    receive_within(channel, HANDOFF_TIMEOUT, hatchling_sys::net::recv_fd)
}

/// The connection `receive` takes off `channel` within `timeout`. A wait
/// that's interrupted is waited again, for what's left: a wait with a
/// timeout returns `EINTR` once the broker is stopped and continued, with
/// no handler at all (`signal(7)`), and the helper, still handing over,
/// mustn't be killed for it.
fn receive_within(
    channel: &UnixStream,
    timeout: Duration,
    mut receive: impl FnMut(std::os::fd::BorrowedFd<'_>) -> Result<OwnedFd, hatchling_sys::Error>,
) -> Result<UnixStream, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(String::from(
                "the browser cell's connection never came: the wait was interrupted until it ran out",
            ));
        }
        channel
            .set_read_timeout(Some(left))
            .map_err(|error| format!("the browser cell's handoff: {error}"))?;
        match receive(channel.as_fd()) {
            Ok(fd) => return Ok(UnixStream::from(fd)),
            Err(hatchling_sys::Error::Net { source, .. })
                if source.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(format!("the browser cell's connection never came: {error}"));
            }
        }
    }
}

/// A browser cell's helper, under the backend's supervision. Dropped, it's
/// killed and reaped, and its cell dies with it.
struct Helper {
    process: Process,
    errors: Option<JoinHandle<Vec<u8>>>,
}

impl Helper {
    /// Starts `spawner` as the helper for the cell `spec` describes, with
    /// `run_dir` its temporary directory and `channel` its stdin.
    fn start(
        spawner: &Path,
        spec: &str,
        run_dir: &Path,
        channel: UnixStream,
    ) -> Result<Self, String> {
        let mut std_child = {
            let mut command = Command::new(spawner);
            command
                .args(crate::run_launcher::spec_args(
                    BROWSER_CELL_SUBCOMMAND,
                    run_dir,
                    spec,
                )?)
                .env("TMPDIR", run_dir)
                .env(BROKER_PID_ENV, broker_pid())
                .stdin(Stdio::from(OwnedFd::from(channel)))
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            // The command holds the helper's end of the channel until it's
            // dropped here: then the helper's exit is the channel's end.
            crate::skill_run::retry_while_busy(|| command.spawn(), std::thread::sleep)
        }
        .map_err(|error| {
            crate::run_launcher::forget_spec(run_dir);
            format!("cannot start {}: {error}", spawner.display())
        })?;
        let process = hatchling_isolation::platform()
            .supervise(&mut std_child)
            .map_err(|error| format!("cannot supervise the browser cell's helper: {error}"))?;
        let errors = std_child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut kept = Vec::new();
                let _ = (&mut pipe).take(STDERR_KEEP).read_to_end(&mut kept);
                // Keep draining so the helper can never block on stderr.
                let _ = std::io::copy(&mut pipe, &mut std::io::sink());
                kept
            })
        });
        Ok(Self { process, errors })
    }

    /// `reason`, with what the helper said, once it's killed and reaped.
    fn failed(mut self, reason: &str) -> String {
        self.stop();
        let said = self
            .errors
            .take()
            .and_then(|errors| errors.join().ok())
            .unwrap_or_default();
        let said = String::from_utf8_lossy(&said);
        let said = said.trim();
        if said.is_empty() {
            reason.to_owned()
        } else {
            format!("{reason} ({said})")
        }
    }

    fn stop(&self) {
        let _ = self.process.terminate();
        let _ = self.process.wait_deadline(REAP_AFTER_KILL);
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The helper: `hatchling-broker browser-cell <spec-file>`.
///
/// Spawns the browser cell the spec describes, hands the broker its
/// connection (on the channel that is the helper's stdin) and its relay
/// (at the spec's handoff socket), keeping neither, and exits with the
/// cell's code. Runs in a fresh process, before any thread exists, which
/// is what makes the fork inside [`spawn_cell`] sound. Refuses a spec that
/// isn't a browser cell.
#[must_use]
pub fn browser_cell_main(spec: Option<&str>) -> ExitCode {
    match run_helper(spec) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "hatchling-broker browser-cell: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_helper(spec: Option<&str>) -> Result<i32, Error> {
    let spec = crate::run_launcher::take_spec(spec, BROWSER_CELL_SUBCOMMAND)?;
    let config: CellConfig = serde_json::from_str(&spec).map_err(|err| Error::Usage {
        reason: format!("browser-cell spec is not a cell config: {err}"),
    })?;
    let handoff = config
        .browser
        .as_ref()
        .map(|browser| browser.handoff.clone())
        .ok_or_else(|| Error::Usage {
            reason: String::from("browser-cell spawns browser cells only"),
        })?;
    // The broker's channel comes off stdin before the cell is spawned, so
    // the cell inherits `/dev/null` there, never the channel.
    let channel = hatchling_sys::take_stdin()
        .map(UnixStream::from)
        .map_err(|source| Error::CellSpawn {
            reason: format!("cannot take the broker's handoff channel: {source}"),
        })?;
    let watch = crate::run_launcher::watch_broker(std::env::var_os(BROKER_PID_ENV))?;
    let mut cell = spawn_cell(&config, LogLevel::Error)?;
    watch.exit_with_parent(1);
    hand_connection_over(&channel, cell.connection())?;
    crate::skill_run::hand_relay_over(&mut cell, &handoff)?;
    Ok(cell.wait_exit()?.code)
}

/// Sends the broker `connection`, the broker's end of the controller's
/// connection, on `channel`, and keeps no copy.
fn hand_connection_over(channel: &UnixStream, connection: Option<UnixStream>) -> Result<(), Error> {
    let connection = connection.ok_or_else(|| Error::CellSpawn {
        reason: String::from("the browser cell came without its connection"),
    })?;
    hatchling_sys::net::send_fd(channel.as_fd(), connection.as_fd()).map_err(|source| {
        Error::CellSpawn {
            reason: format!("cannot hand the browser cell's connection to the broker: {source}"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the helper refuses, before it takes anything or spawns.
    fn refusal(spec: Option<&str>) -> String {
        run_helper(spec).unwrap_err().to_string()
    }

    #[test]
    fn the_helper_refuses_anything_but_a_browser_cell() {
        assert!(refusal(None).contains("browser-cell requires a cell spec"));
        let spec = crate::run_launcher::test_spec;
        assert!(refusal(Some(&spec("{}"))).contains("browser-cell spec is not a cell config"));
        let skill = serde_json::to_string(&CellConfig::skill("browser-spec").unwrap()).unwrap();
        assert!(refusal(Some(&spec(&skill))).contains("browser-cell spawns browser cells only"));
        assert_eq!(browser_cell_main(None), ExitCode::FAILURE);
    }

    #[test]
    fn a_cell_without_its_connection_is_never_handed_over() {
        let (channel, broker) = UnixStream::pair().unwrap();
        let refused = hand_connection_over(&channel, None).unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("the browser cell came without its connection")
        );
        broker.set_nonblocking(true).unwrap();
        assert!(
            hatchling_sys::net::recv_fd(broker.as_fd()).is_err(),
            "nothing sent"
        );
    }

    #[test]
    fn a_connection_is_handed_over_and_kept_nowhere_else() {
        let (channel, broker) = UnixStream::pair().unwrap();
        let (cell_end, controller) = UnixStream::pair().unwrap();
        hand_connection_over(&channel, Some(cell_end)).unwrap();
        let received = receive_connection(&broker).unwrap();
        // The helper kept no copy: the controller's end sees the broker's
        // close as soon as the one it received is gone.
        (&received).write_all(b"x").unwrap();
        let mut byte = [0_u8; 1];
        (&controller).read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"x");
        drop(received);
        assert_eq!((&controller).read(&mut byte).unwrap(), 0, "closed");
    }

    /// A launcher for a browser that's a small file in a fresh directory
    /// for `name`, pinned to `sha256` (or to what it is), with its group
    /// under `cgroup` and a helper that doesn't exist.
    fn launcher(name: &str, sha256: Option<&str>, cgroup: &str) -> (BrowserLauncher, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-browser-cell-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let browser = dir.join("chrome");
        std::fs::write(&browser, b"not a browser").unwrap();
        let actual = crate::browser_bundle::digest(&dir).unwrap();
        let settings = BrowserSettings::parse(|key| match key {
            "HATCHLING_BROWSER" => Some(browser.display().to_string()),
            "HATCHLING_BROWSER_SHA256" => Some(sha256.unwrap_or(&actual).to_owned()),
            "HATCHLING_BROWSER_CONTROLLER" => Some(String::from("/opt/hatchling-browser")),
            _ => None,
        })
        .unwrap()
        .unwrap();
        let launcher = BrowserLauncher::new(
            PathBuf::from("/nonexistent/hatchling-broker"),
            settings,
            CellIds::default(),
            PathBuf::from(cgroup),
            Route::public(),
            Duration::from_secs(60),
        );
        (launcher, dir)
    }

    /// Whether `cell_id` is listed for the operator.
    fn listed(cell_id: &str) -> bool {
        crate::machine::cells()
            .iter()
            .any(|cell| cell.cell_id == cell_id)
    }

    #[test]
    fn a_browser_that_isnt_the_pinned_one_is_never_started() {
        let (launcher, dir) = launcher("pin", Some(&"0".repeat(64)), "/nonexistent-group");
        let refused = launcher.start("browser-pin", Sites::default()).unwrap_err();
        assert!(refused.contains("not the pinned"), "{refused}");
        assert!(!listed("browser-pin"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_launcher_snapshots_its_bundle_once() {
        let (kept, dir) = launcher("once", None, "/nonexistent-group");
        let first = kept.snapshot().unwrap();
        std::fs::write(dir.join("chrome"), b"changed after").unwrap();
        let second = kept.snapshot().unwrap();
        assert!(Arc::ptr_eq(&first, &second), "the same snapshot");
        assert_eq!(
            std::fs::read(second.dir().join("chrome")).unwrap(),
            b"not a browser",
            "as it was checked"
        );
        // One that couldn't be made is tried again at the next start.
        let (refusing, refused_dir) = launcher("again", Some(&"0".repeat(64)), "/x");
        assert!(refusing.snapshot().is_err());
        assert!(refusing.snapshot().is_err());
        assert!(refusing.snapshot.lock().unwrap().is_none(), "nothing kept");
        // A cell runs the browser from the snapshot, never from the
        // operator's directory.
        let config = kept
            .cell_config("browser-once", &first, PathBuf::from("/run/relay.sock"))
            .unwrap();
        let dirs = config.browser.unwrap();
        assert_eq!(dirs.browser_dir, first.dir());
        assert_ne!(dirs.browser_dir, dir);
        assert_eq!(dirs.browser(), first.dir().join("chrome"));
        drop((first, second, kept));
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(refused_dir).unwrap();
    }

    #[test]
    fn a_browser_cell_without_its_group_is_never_started() {
        let (launcher, dir) = launcher("group", None, "/nonexistent-group");
        let refused = launcher
            .start("browser-group", Sites::default())
            .unwrap_err();
        assert!(refused.contains("HATCHLING_CELL_CGROUP"), "{refused}");
        assert!(!listed("browser-group"), "unlisted once it's refused");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A controller that answers each command it reads with the next of
    /// `replies`, as raw bytes, then closes; and the commands it read.
    fn controller(
        connection: UnixStream,
        replies: Vec<Vec<u8>>,
    ) -> JoinHandle<Vec<BrowserCommand>> {
        std::thread::spawn(move || {
            let mut read = Vec::new();
            for reply in replies {
                let Ok(frame) = read_frame(&connection) else {
                    break;
                };
                read.push(serde_json::from_slice(&frame).unwrap());
                write_frame(&connection, &reply).unwrap();
            }
            read
        })
    }

    fn snapshot_reply() -> Vec<u8> {
        serde_json::to_vec(&BrowserReply::Page {
            snapshot: hatchling_core::browser::PageSnapshot::default(),
        })
        .unwrap()
    }

    #[test]
    fn a_command_gets_its_reply_and_a_failed_exchange_ends_the_commands() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let peer = controller(theirs, vec![snapshot_reply(), b"not a reply".to_vec()]);
        let mut commands = Commands::new(ours, Instant::now() + Duration::from_secs(60));
        assert_eq!(
            commands.send(&BrowserCommand::Snapshot).unwrap(),
            BrowserReply::Page {
                snapshot: hatchling_core::browser::PageSnapshot::default()
            }
        );
        let refused = commands.send(&BrowserCommand::Snapshot).unwrap_err();
        assert!(
            refused.starts_with("the browser's answer isn't a reply"),
            "{refused}"
        );
        // Nothing more is sent: the controller, out of replies, has closed,
        // and a command now is refused before it's written.
        let refused = commands.send(&BrowserCommand::Close).unwrap_err();
        assert_eq!(refused, "the browser cell takes no more commands");
        drop(commands);
        assert_eq!(
            peer.join().unwrap(),
            [BrowserCommand::Snapshot, BrowserCommand::Snapshot]
        );
    }

    #[test]
    fn a_reply_that_doesnt_come_in_time_ends_the_commands() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut commands = Commands::new(ours, Instant::now() + Duration::from_secs(60));
        let refused = commands.send(&BrowserCommand::Snapshot).unwrap_err();
        assert!(
            refused.starts_with("no answer from the browser"),
            "{refused}"
        );
        // A reply that comes late is never read as the next one's.
        write_frame(&theirs, &snapshot_reply()).unwrap();
        let refused = commands.send(&BrowserCommand::Snapshot).unwrap_err();
        assert_eq!(refused, "the browser cell takes no more commands");
    }

    #[test]
    fn a_cell_past_its_lifetime_is_sent_nothing() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut commands = Commands::new(ours, Instant::now());
        let refused = commands.send(&BrowserCommand::Snapshot).unwrap_err();
        assert_eq!(refused, "the browser cell is past its lifetime");
        assert_eq!(
            commands.send(&BrowserCommand::Snapshot).unwrap_err(),
            "the browser cell takes no more commands"
        );
        drop(commands);
        let mut byte = [0_u8; 1];
        assert_eq!((&theirs).read(&mut byte).unwrap(), 0, "nothing was written");
    }

    #[test]
    fn a_helper_that_fails_says_why_in_its_own_words() {
        let run_dir = RunDir::create().unwrap();
        let (channel, theirs) = UnixStream::pair().unwrap();
        // `sh` started as a helper runs `browser-cell` as a script, which
        // isn't there: it says so on stderr and exits, closing the channel.
        let helper = Helper::start(Path::new("/bin/sh"), "{}", run_dir.path(), theirs).unwrap();
        let reason = receive_connection(&channel).unwrap_err();
        assert!(
            reason.starts_with("the browser cell's connection never came"),
            "{reason}"
        );
        let failed = helper.failed("it failed");
        assert!(failed.starts_with("it failed ("), "{failed}");
        assert!(
            failed.contains(BROWSER_CELL_SUBCOMMAND),
            "its words: {failed}"
        );
        let quiet_dir = RunDir::create().unwrap();
        let quiet = Helper::start(
            Path::new("/bin/true"),
            "{}",
            quiet_dir.path(),
            UnixStream::pair().unwrap().0,
        )
        .unwrap();
        assert_eq!(
            quiet.failed("it failed"),
            "it failed",
            "nothing said, nothing added"
        );
    }

    #[test]
    fn a_stopped_helper_is_gone() {
        let run_dir = RunDir::create().unwrap();
        // `yes` started as a helper runs until it's killed.
        let (_, theirs) = UnixStream::pair().unwrap();
        let mut helper =
            Helper::start(Path::new("/usr/bin/yes"), "{}", run_dir.path(), theirs).unwrap();
        assert!(helper.process.try_wait().unwrap().is_none(), "running");
        helper.stop();
        // Its stderr closes once it's gone.
        let errors = helper.errors.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !errors.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(errors.is_finished(), "still running after it was stopped");
    }

    #[test]
    fn a_helper_that_cant_start_is_named() {
        let run_dir = RunDir::create().unwrap();
        let (_, theirs) = UnixStream::pair().unwrap();
        let refused = Helper::start(
            Path::new("/nonexistent/hatchling-broker"),
            "{}",
            run_dir.path(),
            theirs,
        )
        .err()
        .unwrap();
        assert!(
            refused.starts_with("cannot start /nonexistent/hatchling-broker"),
            "{refused}"
        );
        assert!(
            std::fs::read_dir(run_dir.path()).unwrap().next().is_none(),
            "an unstarted helper's spec was left behind"
        );
    }

    /// What an interrupted wait gives.
    fn interrupted() -> hatchling_sys::Error {
        hatchling_sys::Error::Net {
            operation: "receive fd",
            source: std::io::Error::from(std::io::ErrorKind::Interrupted),
        }
    }

    #[test]
    fn an_interrupted_wait_for_the_connection_waits_again() {
        let (channel, _helper) = UnixStream::pair().unwrap();
        let (cell_end, _controller) = UnixStream::pair().unwrap();
        let mut sent = Some(OwnedFd::from(cell_end));
        let mut waits = 0;
        let received = receive_within(&channel, Duration::from_secs(60), |_| {
            waits += 1;
            if waits < 3 {
                Err(interrupted())
            } else {
                sent.take().ok_or_else(interrupted)
            }
        });
        assert!(received.is_ok(), "{received:?}");
        assert_eq!(waits, 3);
        // Interrupted until the time runs out: it's given up, not waited
        // for forever.
        let started = Instant::now();
        let refused = receive_within(&channel, Duration::from_millis(50), |_| Err(interrupted()))
            .unwrap_err();
        assert!(
            refused.ends_with("the wait was interrupted until it ran out"),
            "{refused}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        // Any other failure is the helper's, at once.
        let mut waits = 0;
        let refused = receive_within(&channel, Duration::from_secs(60), |_| {
            waits += 1;
            Err(hatchling_sys::Error::Net {
                operation: "receive fd",
                source: std::io::Error::other("no fd was sent"),
            })
        })
        .unwrap_err();
        assert_eq!(waits, 1);
        assert!(refused.contains("no fd was sent"), "{refused}");
    }

    #[test]
    fn a_connection_that_never_comes_is_the_helpers_failure() {
        let (channel, helper_end) = UnixStream::pair().unwrap();
        drop(helper_end);
        let refused = receive_connection(&channel).unwrap_err();
        assert!(
            refused.starts_with("the browser cell's connection never came"),
            "{refused}"
        );
    }
}
