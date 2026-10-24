//! Connection serving: the per-connection handshake and session.
//!
//! In cell mode the broker's agent cell has a connection of its own
//! ([`serve_cell_connection`]), and no socket admits any other agent.
//! Without a cell, every connection on the broker's socket runs the same
//! pipeline: kernel peer credentials first, then the handshake, then the
//! session. The peer gate fires
//! before a single byte is parsed, and a peer that fails it is closed
//! silently — no `REJECT` goes back. Unauthorized peers get an
//! immediate 0-byte close. The why stays in the broker's logs, where
//! the operator can see it, not on the wire, where anyone can.

use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use hatchling_channel::LocalHub;
use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_core::ui::operator::{CellKind, EgressVerdict};
use hatchling_core::{
    AgentMessage, BrokerMessage, CellId, ChannelResult, EgressOp, EgressResult, GoalsResult,
    HookResult, MAX_FRAME_BYTES, MemoryResult, PROTOCOL_VERSION, PeerCredentials, PeerPid,
    QueryResult, ScheduleResult, SecretName, SessionRequest, SessionResponse, SkillResult,
    VaultResult, decode_secret, encode_secret, peer_credentials,
};
use hatchling_egress::{EgressRequest, Proxy};
use hatchling_memory::{MemoryPrincipal, SessionId};
use hatchling_vault::OpensStores;
use zeroize::Zeroizing;

use crate::cell::cgroup_parent;
use crate::egress_path::{EgressPath, Refusal, RefusalKind};
use crate::environment as env;
use crate::secrets::{Secrets, handle_vault_op};
use crate::{
    CellConfig, Config, EgressConfig, Error, SkillRuntime, Skills, VaultConfig, bind_listener,
    spawn_cell,
};

/// Starts the vault keeper, which loads the master key and hands the
/// broker its stores' keys, binds the listen socket, and serves agents.
///
/// The broker is supervised and runs until killed: `serve_forever`
/// never returns, so neither does this. Startup failures (bad config,
/// key problems, socket setup) are fail-closed via [`Error`].
///
/// # Errors
///
/// Propagates [`Error`] from the keeper's start (key loading, vault
/// validation), cell spawn, or socket setup.
pub fn run_serve(config: &Config) -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_max_level(config.log_level.to_tracing())
        .init();
    // One broker per socket: the lock beside it is held for this
    // broker's life, before anything is written, and `clean` and a
    // second broker are refused while it is (WO-012).
    let _serving = crate::sweep::take_lock(config)?;
    // The vault keeper starts first, while the broker has one thread
    // (WO-017): it loads (or provisions on first run) the master key,
    // checks the stores, and hands the broker the keys of its own stores.
    // The master key and the secrets store's key stay with the keeper; a
    // key or store problem stops the broker before it serves anyone.
    let (key, authoritys) = crate::keeper::start()?;
    // The egress authority next, still on one thread (WO-017, step 2): it
    // alone reads the credentials of the requests it makes, from the
    // keeper on the channel just made for it, which the broker closes.
    crate::egress_authority::start(authoritys)?;
    // The spawner last, still on one thread (WO-017, step 3): every
    // cell's helper is started through it.
    crate::spawner::start()?;
    // Load skills before binding, before any session can name one:
    // each is verified, pinned, and registered, or refused loudly on
    // its own. A broken skill never stops the broker; it never runs.
    // Skills run through the broker's own binary as the spawner helper.
    let spawner = std::env::current_exe().map_err(|err| Error::InvalidConfig {
        key: "skill spawner",
        reason: format!("cannot locate the broker binary: {err}"),
    })?;
    // The egress root store: a bad CA bundle stops the broker
    // here, not on the first agent's first request.
    let tls = egress_tls(|key| std::env::var(key).ok())?;
    tracing::info!(
        operator_roots = tls.operator_roots(),
        "egress TLS roots loaded"
    );
    // The browser, if the operator set one up (WO-029): checked now, so a
    // bad setting stops the broker here, not on the agent's first page.
    crate::browser_ops::configure_from_env(spawner.clone())?;
    let skills = Skills::load(&config.skills)?
        .with_runtime(SkillRuntime::new(spawner))
        .with_harnesses(crate::harness::Harnesses::from_env(config)?.with_tls(&tls));
    // The output gate, when there is an ingress for it to guard:
    // from the operator's environment only, its settings recorded before
    // anything is served.
    let redaction = config
        .channel_ingress
        .as_ref()
        .map(|_| crate::RedactionConfig::from_env(config))
        .transpose()?;
    // Every store of the broker's holds current envelopes (WO-002), or
    // the broker stops here, naming the files `migrate-vault` must
    // re-seal. The vault is the keeper's, which checks it itself.
    let mut stores = crate::sealed_stores(config, redaction.as_ref());
    stores.retain(|(store, _)| *store != hatchling_vault::Store::SECRETS);
    crate::check_sealed_stores(&stores)?;
    // An earlier broker killed mid-run left its cells' staging and its
    // runs' directories: none is live now, and none is your data.
    let _swept = crate::sweep::sweep_at_start(config, redaction.as_ref());
    let gated = match &redaction {
        Some(redaction) => crate::output_gate::build(config, redaction, &key)?,
        None => crate::output_gate::Built {
            gate: hatchling_channel::OutputGate::disabled(),
            worker: None,
        },
    };
    if cgroup_parent(&|key| crate::cell::env_value(std::env::var(key)))?.is_none() {
        tracing::warn!(
            "HATCHLING_CELL_CGROUP unset: cells use rlimits alone; no whole-cell memory or CPU limit"
        );
    }
    // The local channel's ingress, if configured: bound now, served on
    // its own thread once the cell has forked. Every reply its hub
    // delivers passes the gate.
    let ingress = crate::ingress::open_ingress(config, gated.gate)?;
    // The device endpoint, if configured: bound now, served on
    // its own thread once the cell has forked.
    let devices = crate::device::open_device_endpoint(config)?;
    // The local API, if configured: bound now, served on its
    // own thread once the cell has forked.
    let api_config = crate::api_auth::ApiConfig::from_env(config)?;
    let api = crate::api::open_api(config, &api_config)?;
    // The operator socket, if configured (WO-022): bound now, served on
    // its own thread once the cell has forked.
    let operator_socket = crate::operator::parse_operator_socket(
        config,
        &api_config,
        std::env::var(env::HATCHLING_OPERATOR_SOCKET.name)
            .ok()
            .as_deref(),
    )?;
    let operator = crate::operator::open_operator(config, operator_socket.as_ref())?;
    // Spawn the cell BEFORE serving: spawn_cell forks, and forking a
    // multi-threaded process is unsound. HATCHLING_AGENT_BIN selects
    // cell mode: the cell's agent gets a connection of its own, and the
    // broker's socket isn't bound at all, so nothing else can open an
    // agent session. Without it the broker serves bare (tests,
    // debugging), admitting agents on its socket by uid alone.
    let listener;
    let agents = if std::env::var_os(env::HATCHLING_AGENT_BIN.name).is_some() {
        let cell = spawn_agent_cell(
            config,
            CellConfig::from_env(&config.socket, config.agent_uid.as_u32())?,
        )?;
        tracing::info!(
            cell_id = %cell.cell_id,
            agent_uid = %config.agent_uid,
            "broker serving its cell"
        );
        Agents::Cell(cell)
    } else {
        tracing::info!("HATCHLING_AGENT_BIN unset: serving without a cell");
        listener = bind_listener(config)?;
        tracing::info!(
            socket = %config.socket,
            agent_uid = %config.agent_uid,
            "broker listening"
        );
        Agents::Listener(&listener)
    };
    std::thread::scope(|scope| {
        // The gate's rules borrow the keys on a thread of their own
        // (see `crate::output_gate`): the gate only queues work for them.
        if let (Some((worker, phrases)), Some(redaction)) = (gated.worker, &redaction) {
            let rules = crate::Rules::new(config, &key, redaction.builtins, phrases);
            let raw = redaction.dir.join(crate::output_gate::RAW_DIR);
            scope.spawn(move || worker.serve(&rules, &raw));
        }
        if let Some(operator) = &operator {
            scope.spawn(move || operator.serve());
        }
        if let Some(api) = &api {
            let hub = ingress.as_ref().map(crate::ingress::Ingress::hub);
            let (key, skills) = (&key, &skills);
            scope.spawn(move || api.serve(config, key, skills, hub));
            // The worker that runs what the owner approves in the app
            // (WO-027): approvals come only from the owner's sockets, so
            // it runs beside them.
            if config.artifacts.enabled() {
                scope.spawn(move || crate::approval_board::work_forever(config, key, skills, hub));
            }
        }
        serve_agents(
            agents,
            config,
            &key,
            &skills,
            ingress.as_ref(),
            devices.as_ref(),
        )
    })
}

/// Spawns the broker's agent cell from `cell_config`, connected: the
/// agent's way to the broker is a connection made for it alone.
///
/// It starts through the spawner when one runs (WO-017), else here. A
/// reaper thread owns the cell's wait.
///
/// # Errors
///
/// A vault in the cell's view, an invalid cell id, the spawn's errors,
/// or [`Error::CellSpawn`] if the spawned cell came without its
/// connection.
pub fn spawn_agent_cell(config: &Config, mut cell_config: CellConfig) -> Result<AgentCell, Error> {
    cell_config.connected = true;
    cell_config.refuse_vault_in_view(&config.vault)?;
    let cell_id: CellId = cell_config.cell_id.parse()?;
    if let Some(spawner) = crate::spawner::spawner() {
        return spawn_through(spawner, &cell_config, cell_id, &std::env::temp_dir());
    }
    let mut handle = spawn_cell(&cell_config, config.log_level)?;
    let connection = handle.connection().ok_or_else(|| Error::CellSpawn {
        reason: String::from("the agent cell came without its connection"),
    })?;
    let pid = handle.child_pid();
    let running =
        crate::machine::Running::start(&cell_config.cell_id, CellKind::Agent, &cell_config.limits);
    tracing::info!(
        cell_id = %cell_id,
        child_pid = pid,
        uid_base = cell_config.uid_base,
        "cell spawned"
    );
    // The reaper owns the wait: it collects the agent's exit
    // status, surfaces child-reported setup failures in the logs,
    // and removes the staging directory.
    std::thread::spawn(move || {
        match handle.wait_exit() {
            Ok(exit) => tracing::info!(status = exit.status, exit_code = exit.code, "cell exited"),
            Err(err) => tracing::error!(%err, "cell wait failed"),
        }
        // Listed until it is reaped.
        drop(running);
    });
    Ok(AgentCell {
        connection,
        cell_id,
        pid: u32::try_from(pid).unwrap_or_default(),
    })
}

/// Spawns the agent cell `cell_config` through `spawner`, in a private
/// directory of its own in `temp`, and in a group of its own when cells
/// have them. A reaper thread owns the helper's wait, then the group and
/// the directory.
///
/// # Errors
///
/// [`Error::CellSpawn`] if the cell can't be prepared or started, or
/// came without its connection, and as
/// [`CellConfig::supervised_cgroup`].
fn spawn_through(
    spawner: &crate::spawner::Spawner,
    cell_config: &CellConfig,
    cell_id: CellId,
    temp: &std::path::Path,
) -> Result<AgentCell, Error> {
    let failed = |reason: String| Error::CellSpawn { reason };
    let (cell, group) = cell_config.supervised_cgroup()?;
    let name = crate::approval::random_hex("hatchling-agent-")
        .map_err(|error| failed(format!("cannot name the agent cell's directory: {error}")))?;
    let run_dir = temp.join(name);
    hatchling_sys::create_private_dir(&run_dir)
        .map_err(|error| failed(format!("cannot make the agent cell's directory: {error}")))?;
    let (process, connection) = spawner.agent_cell(&cell, &run_dir).map_err(|reason| {
        let _ = hatchling_sys::remove_dir_tree(&run_dir);
        failed(reason)
    })?;
    let running = crate::machine::Running::start(&cell.cell_id, CellKind::Agent, &cell.limits);
    tracing::info!(
        cell_id = %cell_id,
        helper_pid = process.pid(),
        uid_base = cell.uid_base,
        "cell spawned through the spawner"
    );
    std::thread::spawn(move || {
        match process.wait() {
            Ok(exit) => tracing::info!(status = exit.status, exit_code = exit.code, "cell exited"),
            Err(err) => tracing::error!(%err, "cell wait failed"),
        }
        // Listed, and its group held, until it is reaped.
        drop((running, group));
        let _ = hatchling_sys::remove_dir_tree(&run_dir);
    });
    // The cell's pid lives in its helper; the audit trail has its id.
    Ok(AgentCell {
        connection,
        cell_id,
        pid: 0,
    })
}

/// An agent cell the broker spawned, with the broker's end of the
/// connection it made for it.
#[derive(Debug)]
pub struct AgentCell {
    /// The broker's end of the cell's connection: in cell mode, the only
    /// way into an agent session.
    pub connection: UnixStream,
    /// The cell's id, as the broker gave it.
    pub cell_id: CellId,
    /// The cell's pid, for the audit trail only.
    pub pid: u32,
}

/// Where agent sessions come from.
enum Agents<'a> {
    /// The broker's socket: any process with the agent uid (bare mode).
    Listener(&'a UnixListener),
    /// The one cell the broker spawned, on its own connection.
    Cell(AgentCell),
}

/// Forgets a session's output gate scope when the session ends.
struct EndScope<'a> {
    hub: &'a LocalHub,
    scope: &'a str,
}

impl Drop for EndScope<'_> {
    fn drop(&mut self) {
        self.hub.end_scope(self.scope);
    }
}

/// Accepts connections forever, handing each to a background thread.
///
/// A thread per connection keeps this dependency-free (no async
/// runtime). The kernel peer gate keeps the client population to the
/// single authorized uid, and the connection cap (,
/// [`ConnectionSlots`](crate::connections::ConnectionSlots),
/// `config.max_connections`) bounds how many of its connections are
/// served at once. A connection past the cap is closed silently, as an
/// unauthorized peer's is, and the loop goes on accepting: a full cap
/// never wedges it, and the next free slot admits again.
///
/// `key` opens the broker's sealed stores: the keys it was handed at
/// start, never the master key. Every session borrows it to open them.
/// The borrow (not a clone) is deliberate — key material has exactly one
/// owner. `skills` is borrowed the same
/// way: the broker owns the one registry, and sessions check calls
/// against it.
///
/// `ingress`, when configured, is served on its own thread, and every
/// session's local channel is backed by its hub. So is `devices`,
/// the device endpoint.
pub fn serve_forever(
    listener: &UnixListener,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    ingress: Option<&crate::ingress::Ingress>,
    devices: Option<&crate::device::DeviceEndpoint>,
) -> ! {
    serve_agents(
        Agents::Listener(listener),
        config,
        key,
        skills,
        ingress,
        devices,
    )
}

/// [`serve_forever`] for the one agent `cell` the broker spawned.
///
/// Its session is served on the cell's own connection
/// ([`serve_cell_connection`]), and no socket admits any other. When
/// the session ends the agent is gone, and the broker goes on serving
/// everything else (scheduled runs, ingress, devices) until killed.
pub fn serve_cell_forever(
    cell: AgentCell,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    ingress: Option<&crate::ingress::Ingress>,
    devices: Option<&crate::device::DeviceEndpoint>,
) -> ! {
    serve_agents(Agents::Cell(cell), config, key, skills, ingress, devices)
}

/// Serves `agents`, and everything beside them, forever.
fn serve_agents(
    agents: Agents<'_>,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    ingress: Option<&crate::ingress::Ingress>,
    devices: Option<&crate::device::DeviceEndpoint>,
) -> ! {
    // Scoped threads, not detached: the `&dyn OpensStores` borrow crosses
    // the thread boundary without an `Arc`, and the single-owner
    // discipline holds — every session borrows the broker's one set of keys.
    // The scope never exits: the accept loop diverges.
    let slots = crate::connections::ConnectionSlots::new(config.max_connections);
    std::thread::scope(|scope| {
        // The trust-roots watcher: rotation without a restart (see
        // `crate::trust_roots`). It returns at once when skills are off.
        scope.spawn(|| skills.watch(crate::trust_roots::TRUST_ROOTS_POLL));
        if let Some(ingress) = ingress {
            scope.spawn(|| ingress.serve());
        }
        if let Some(devices) = devices {
            scope.spawn(|| devices.serve(config, key));
        }
        let hub = ingress.map(crate::ingress::Ingress::hub);
        // Scheduled runs (-2): the dispatcher ticks each minute
        // with the real cell launcher, when scheduling is on and an agent
        // binary is configured.
        if let Some(runs) = crate::run_launcher::RunEnv::from_process(config) {
            // Chat (WO-021): each message the local channel queues is
            // one run, when the API's grants let a run answer there.
            let chat = crate::api_auth::ApiConfig::from_env(config)
                .ok()
                .and_then(|api| crate::chat_run::chat_grants(&api.grants));
            if let (Some(hub), Some(grants)) = (hub, chat) {
                let runs = runs.clone();
                scope.spawn(move || {
                    crate::chat_run::chat_forever(scope, config, key, hub, runs, grants)
                });
            }
            scope.spawn(move || {
                crate::run_launcher::dispatch_forever(scope, config, key, hub, runs)
            });
        }
        let listener = match agents {
            Agents::Listener(listener) => listener,
            Agents::Cell(cell) => serve_cell_then_wait(&cell, config, key, skills, hub),
        };
        loop {
            match listener.accept() {
                Ok((stream, _)) => match slots.try_acquire() {
                    Ok(slot) => {
                        let config = config.clone();
                        // `&dyn OpensStores` is `Copy`: each thread borrows the
                        // same keys.
                        scope.spawn(move || {
                            handle_connection(&stream, &config, key, skills, hub);
                            // Free the slot before the stream closes: a
                            // peer that sees the close can reconnect
                            // into it at once.
                            drop(slot);
                            drop(stream);
                        });
                    }
                    Err(refused) => {
                        if refused.first_of_episode {
                            tracing::warn!(
                                max_connections = slots.max(),
                                "connection cap reached: closing new connections until one \
                                 ends (logged once per episode)"
                            );
                        }
                        // A silent close: the peer learns nothing.
                        drop(stream);
                    }
                },
                Err(source) => {
                    tracing::error!(%source, "accept failed");
                }
            }
        }
    })
}

/// Handles one connection: authorize, handshake, session, then close.
///
/// Every outcome is logged at the site that decided it. The stream is
/// dropped (closed) on return; unauthorized peers never see a byte.
fn handle_connection(
    stream: &UnixStream,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
) {
    match serve_connection_with_ingress(stream, config, key, skills, hub) {
        Ok(()) => {}
        Err(Error::PeerUnauthorized { actual, expected }) => {
            tracing::warn!(
                peer_uid = %actual,
                expected_uid = %expected,
                "rejected unauthorized peer"
            );
        }
        Err(error) => {
            tracing::warn!(%error, "connection failed");
        }
    }
}

/// Runs the per-connection pipeline: peer authorization, handshake,
/// vault session.
///
/// The peer's kernel uid must equal the configured agent uid before any
/// of its bytes are read. After `WELCOME`, the session serves one vault
/// op per frame until the agent closes the connection (clean EOF) or
/// sends a malformed frame (protocol violation: the session ends,
/// unknown traffic is never acted on).
///
/// # Errors
///
/// Returns [`Error::PeerUnauthorized`] for a uid mismatch (the caller
/// closes silently), [`Error::HandshakeIo`] on transport failure,
/// [`Error::HandshakeProtocol`] on any framing, encoding, or version
/// violation during the handshake, [`Error::SessionIo`] /
/// [`Error::SessionProtocol`] for the same classes mid-session, and
/// [`Error::Vault`] if the vault root cannot be opened.
pub fn serve_connection(
    stream: &UnixStream,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
) -> Result<(), Error> {
    serve_connection_with_ingress(stream, config, key, skills, None)
}

/// [`serve_connection`], with the session's local channel backed by the
/// ingress `hub` when one is given.
///
/// # Errors
///
/// As [`serve_connection`].
pub fn serve_connection_with_ingress(
    stream: &UnixStream,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
) -> Result<(), Error> {
    serve_connection_with_runs(
        stream,
        config,
        key,
        skills,
        hub,
        crate::RunEnv::from_process(config),
    )
}

/// [`serve_connection_with_ingress`], with the session's subagents
/// started through `runs` (none: the session cannot start
/// children).
///
/// # Errors
///
/// As [`serve_connection`].
pub fn serve_connection_with_runs(
    stream: &UnixStream,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
    runs: Option<crate::RunEnv>,
) -> Result<(), Error> {
    // The peer gate: kernel credentials before bytes. There is nothing to
    // authorize against without them, so a credential failure is
    // fail-closed.
    let creds = peer_credentials(stream)?;
    if creds.uid() != config.agent_uid {
        return Err(Error::PeerUnauthorized {
            actual: creds.uid(),
            expected: config.agent_uid,
        });
    }
    let cell_id = handshake(stream, None)?;
    tracing::info!(
        peer_uid = %creds.uid(),
        peer_pid = %creds.pid(),
        cell_id = %cell_id,
        "agent session established"
    );
    serve_session(stream, config, key, skills, &cell_id, creds, hub, runs)
}

/// Serves the agent session on `cell`'s own connection.
///
/// There is no peer gate: the broker made the connection and gave its
/// other end to the cell it spawned, so whoever speaks on it is that
/// cell's agent. The kernel's report would only name the broker, which
/// made both ends. The hello must name the cell's own id: a session
/// never runs, or is recorded, under another.
///
/// # Errors
///
/// As [`serve_connection`], less [`Error::PeerUnauthorized`];
/// [`Error::HandshakeProtocol`] for a hello naming another cell.
pub fn serve_cell_connection(
    cell: &AgentCell,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
    runs: Option<crate::RunEnv>,
) -> Result<(), Error> {
    let stream = &cell.connection;
    let cell_id = handshake(stream, Some(&cell.cell_id))?;
    let creds = PeerCredentials::of_spawned(config.agent_uid, PeerPid::from_kernel(cell.pid));
    tracing::info!(
        peer_uid = %creds.uid(),
        peer_pid = %creds.pid(),
        cell_id = %cell_id,
        "agent session established"
    );
    serve_session(stream, config, key, skills, &cell_id, creds, hub, runs)
}

/// Serves `cell`'s session on this thread, then waits forever: the
/// broker serves everything else on its other threads.
fn serve_cell_then_wait(
    cell: &AgentCell,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
) -> ! {
    match serve_cell_connection(
        cell,
        config,
        key,
        skills,
        hub,
        crate::RunEnv::from_process(config),
    ) {
        Ok(()) => {}
        Err(error) => tracing::warn!(?error, cell_id = %cell.cell_id, "agent session failed"),
    }
    loop {
        std::thread::park();
    }
}

/// The handshake on an admitted connection: the hello, read within
/// [`HANDSHAKE_TIMEOUT`], then `WELCOME`. Returns the cell id the hello
/// named, which must be `expected` when the broker knows who is on the
/// connection.
///
/// # Errors
///
/// [`Error::HandshakeIo`] on transport failure, and
/// [`Error::HandshakeProtocol`] on any framing, encoding or version
/// violation, an invalid cell id, or one other than `expected`.
fn handshake(stream: &UnixStream, expected: Option<&CellId>) -> Result<CellId, Error> {
    // A frame-level failure means we never got a complete message: close
    // without answering. `REJECT` is only for authorized peers whose
    // complete bytes turned out to be invalid.
    let frame = read_hello(stream, HANDSHAKE_TIMEOUT)?;
    let hello: AgentMessage = match serde_json::from_slice(&frame) {
        Ok(hello) => hello,
        Err(e) => {
            // The parser's detail stays in the broker's log.
            tracing::debug!(error = ?e, "hello did not parse");
            let reason = String::from("hello is not a valid handshake message");
            // The write may fail if the connection is already broken;
            // either way the connection ends here.
            let _ = reject(stream, &reason);
            return Err(Error::HandshakeProtocol { reason });
        }
    };

    let AgentMessage::Hello { v, cell_id } = hello;
    if v != PROTOCOL_VERSION {
        let reason = format!("unsupported protocol version {v}");
        let _ = reject(stream, &reason);
        return Err(Error::HandshakeProtocol { reason });
    }
    let cell_id: CellId = match cell_id.parse() {
        Ok(id) => id,
        Err(error) => {
            let _ = reject(stream, &error.to_string());
            return Err(error.into());
        }
    };
    if let Some(expected) = expected.filter(|expected| **expected != cell_id) {
        let reason = format!("the hello names cell {cell_id}, but this connection is {expected}'s");
        let _ = reject(stream, &reason);
        return Err(Error::HandshakeProtocol { reason });
    }

    let welcome = BrokerMessage::Welcome {
        v: PROTOCOL_VERSION,
    };
    let payload = serde_json::to_vec(&welcome).map_err(|e| Error::HandshakeProtocol {
        reason: format!("failed to encode welcome: {e}"),
    })?;
    write_frame(stream, &payload)?;
    Ok(cell_id)
}

/// How long an authorized peer may leave the connection silent before
/// its hello is complete. An agent sends its hello at once; one that
/// never does would hold a connection slot.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads the hello frame, giving up on a peer silent for `timeout`.
///
/// The timeout covers each read of the hello. It is cleared once the
/// hello is in: the session after it may idle for as long as the agent
/// likes.
///
/// # Errors
///
/// [`Error::HandshakeIo`] when the peer stays silent, closes, or the
/// timeout can't be set.
fn read_hello(stream: &UnixStream, timeout: Duration) -> Result<Vec<u8>, Error> {
    let set = |timeout| {
        stream
            .set_read_timeout(timeout)
            .map_err(|source| Error::HandshakeIo { source })
    };
    set(Some(timeout))?;
    let frame = read_frame(stream)?;
    set(None)?;
    Ok(frame)
}

/// Runs the post-handshake session: one op per frame until EOF.
///
/// The peer was authorized before the handshake; every op is additionally
/// authorized against the grant set before any backend is touched. A
/// malformed frame ends the session: unknown traffic is never acted on.
///
/// The vault is opened per authorized op, not per session: the
/// `0700`/owner validation re-runs every time, so tampering with the
/// store between ops fails closed instead of going stale — and a peer
/// that never presents a granted op causes no filesystem side effect at
/// all.
///
/// The channel registry is built per session from the configured channel
/// ids: channels own I/O state and are not `Sync`, so each session gets
/// its own endpoints — and its own account handles and loaded secrets,
/// dropped (and zeroized) when the session ends.
///
/// # Errors
///
/// Returns [`Error::SessionIo`] on transport failure,
/// [`Error::SessionProtocol`] on any framing, encoding, or op violation,
/// [`Error::Vault`] if the vault root cannot be opened, or
/// [`Error::Egress`] if the egress proxy fails to initialize.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the session's inputs, each the broker's own, and the one dispatch every op is authorized in; splitting either would only hide which is which"
)]
fn serve_session(
    stream: &UnixStream,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    cell_id: &CellId,
    creds: PeerCredentials,
    hub: Option<&LocalHub>,
    runs: Option<crate::RunEnv>,
) -> Result<(), Error> {
    // The session's egress, opened once per session, with a surrogate key
    // fresh for it: surrogates do not survive reconnects. It goes through
    // the egress authority, which holds the key and reads each credential
    // (WO-017, step 2), so the broker holds neither. In-process, with no
    // authority running, it's a proxy of the broker's own.
    let egress_path: Box<dyn EgressPath + '_> = match crate::egress_authority::authority() {
        Some(authority) => Box::new(authority.connect()?.egress()?),
        None => Box::new(init_egress_proxy(&config.egress, &config.vault, key)?),
    };
    // Egress bodies too large for one frame, in transit (see
    // `crate::egress_pages`): one staged request and one answer at most.
    let mut egress_pages = crate::egress_pages::Pages::default();
    // The session id is minted per connection; the agent never supplies
    // it. It names the session's output gate scope, forgotten
    // when the session ends, however it ends.
    let session_id = SessionId::generate().map_err(|source| Error::Memory { source })?;
    let gate_scope = crate::output_gate::session_scope(&session_id);
    let _gate_scope = hub.map(|hub| EndScope {
        hub,
        scope: &gate_scope,
    });
    // The channel registry: one endpoint per configured channel, owned
    // by this session, with the channel accounts' secrets loaded from
    // the sealed vault into it. Unknown ids, or accounts that
    // cannot be loaded, fail the session (fail closed) — the broker
    // must not run with a missing channel. Its local channel delivers in
    // the session's gate scope.
    let mut channels = crate::channel::ChannelRegistry::new(
        &config.channels,
        &config.channel_accounts,
        &config.vault,
        key,
        hub.map(|hub| (hub, gate_scope.as_str())),
    )?;
    // The memory principal: peer uid from kernel credentials and the
    // session id.
    let principal = MemoryPrincipal {
        peer_uid: creds.uid().as_u32(),
        session_id: session_id.clone(),
    };
    // The space session: this session's broker-asserted
    // principal and its kernel-attested peer, which every space admission
    // is pinned to. Its admissions end with it, however it ends.
    let space_session = crate::space_ops::SpaceSession::for_session(config, &creds, &session_id);
    // The session's web: the sites the owner allows it for the session,
    // forgotten when it ends (WO-028).
    let web_tls = if config.web.enabled || config.web.search {
        Some(egress_tls(|key| std::env::var(key).ok())?)
    } else {
        None
    };
    let mut web = crate::web_ops::WebSession::new(config, &gate_scope, web_tls);
    // The session's browser (WO-029): started on its first page, closed
    // when the session ends.
    let mut browser = crate::browser_ops::BrowserSession::new(crate::browser_ops::configured())
        .with_board(crate::browser_board::board(config));
    let _admissions = crate::space_ops::EndAdmissions(space_session.pin.session.clone());

    // The session's children live in the session's own scope:
    // when the session ends, however it ends, they are killed and the
    // scope joins their sessions.
    std::thread::scope(|scope| {
        let mut subagents = crate::subagent_ops::Subagents::new(
            scope,
            config,
            key,
            runs,
            session_id.as_str().to_owned(),
        );
        loop {
            let Some(frame) = read_frame_or_eof(stream).map_err(session_error)? else {
                tracing::info!(cell_id = %cell_id, "agent session closed");
                return Ok(());
            };
            let request = request_of(&Zeroizing::new(frame))?;
            let response = match request {
                SessionRequest::Vault(op) => SessionResponse::Vault(handle_vault_op(
                    &config.vault,
                    key,
                    &op,
                    cell_id,
                    creds.uid().as_u32(),
                )?),
                SessionRequest::Egress(op) => SessionResponse::Egress(handle_egress_op(
                    &*egress_path,
                    &config.egress,
                    &op,
                    cell_id,
                    creds.uid().as_u32(),
                    &mut egress_pages,
                )?),
                SessionRequest::Channel(op) => SessionResponse::Channel(
                    crate::channel::handle_channel_op(&mut channels, &op, creds.uid().as_u32())?,
                ),
                SessionRequest::Memory(op) => {
                    SessionResponse::Memory(crate::memory_ops::handle_memory_op(
                        &config.memory,
                        &op,
                        &principal,
                        &session_id,
                    )?)
                }
                SessionRequest::Skill(op) => {
                    let keys = crate::harness::vault_keys(&config.vault.root, key);
                    SessionResponse::Skill(crate::skill_ops::handle_skill_op(
                        skills, &op, cell_id, &keys,
                    ))
                }
                SessionRequest::Goals(op) => SessionResponse::Goals(
                    crate::goals_ops::handle_goals_op(&config.goals, &op, &principal)?,
                ),
                SessionRequest::Query(op) => {
                    SessionResponse::Query(crate::goals_ops::handle_query_op(
                        &config.goals,
                        &config.memory,
                        &op,
                        &principal,
                    )?)
                }
                SessionRequest::Schedule(op) => {
                    SessionResponse::Schedule(crate::schedule_ops::handle_schedule_op(
                        config,
                        key,
                        &op,
                        hatchling_scheduler::Timestamp::now(),
                        hatchling_scheduler::Creator::Agent,
                    ))
                }
                SessionRequest::Hook(op) => {
                    SessionResponse::Hook(crate::schedule_ops::handle_hook_op(config, key, &op))
                }
                SessionRequest::Artifact(op) => {
                    SessionResponse::Artifact(crate::artifact_ops::handle_artifact_op(
                        config,
                        key,
                        skills,
                        hub,
                        &principal,
                        &op,
                        hatchling_scheduler::Timestamp::now().as_second(),
                    ))
                }
                SessionRequest::Space(op) => {
                    SessionResponse::Space(crate::space_ops::handle_space_op(
                        config,
                        key,
                        &space_session,
                        &op,
                        hatchling_scheduler::Timestamp::now(),
                    ))
                }
                SessionRequest::Subagent(op) => SessionResponse::Subagent(subagents.handle(&op)),
                SessionRequest::Web(op) => {
                    SessionResponse::Web(crate::web_ops::handle_web_op(config, key, &mut web, &op))
                }
                SessionRequest::Browser(op) => SessionResponse::Browser(
                    crate::browser_ops::handle_browser_op(config, key, &mut web, &mut browser, &op),
                ),
            };
            let payload = encode_reply(&response, MAX_FRAME_BYTES)?;
            write_frame(stream, &payload).map_err(session_error)?;
        }
    })
}

/// The session request in `frame`. A request can carry a secret to store,
/// so its frame is taken only as a buffer wiped when dropped.
///
/// # Errors
///
/// Returns [`Error::SessionProtocol`] for a frame that is not a request.
fn request_of(frame: &Zeroizing<Vec<u8>>) -> Result<SessionRequest, Error> {
    serde_json::from_slice(frame).map_err(|e| Error::SessionProtocol {
        reason: format!("session request is not a valid protocol message: {e}"),
    })
}

/// Encodes a session reply, guaranteeing it fits in `max` bytes.
///
/// A reply that would not fit one frame is replaced by its kind's opaque
/// error reply, so an op whose answer is too large fails as that op and
/// the session survives: no frame the peer's `read_frame` would reject
/// is ever sent. Single memory items fit by construction
/// (`hatchling_memory::MAX_ENCODED_ITEM_BYTES`); this catches the
/// replies that carry many — recall-by-tag, a long context log — and
/// anything else unbounded.
///
/// # Errors
///
/// The reply is made at its final size and wiped when dropped: a fetched
/// secret leaves no copy behind in a freed buffer.
///
/// # Errors
///
/// Returns [`Error::SessionProtocol`] if a reply cannot be encoded.
pub(crate) fn encode_reply(
    response: &SessionResponse,
    max: usize,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let encode = |reply: &SessionResponse| {
        let failed = |e: serde_json::Error| Error::SessionProtocol {
            reason: format!("failed to encode session response: {e}"),
        };
        let mut size = Counted(0);
        serde_json::to_writer(&mut size, reply).map_err(failed)?;
        let mut payload = Zeroizing::new(Vec::with_capacity(size.0));
        serde_json::to_writer(&mut *payload, reply).map_err(failed)?;
        Ok(payload)
    };
    let payload = encode(response)?;
    if payload.len() <= max {
        return Ok(payload);
    }
    tracing::warn!(
        len = payload.len(),
        max,
        "session reply exceeds the frame bound; answering with the op's opaque error"
    );
    encode(&oversize_reply(response))
}

/// A writer that only counts what is written to it.
struct Counted(usize);

impl std::io::Write for Counted {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The opaque error reply of the same kind as `response`.
const fn oversize_reply(response: &SessionResponse) -> SessionResponse {
    match response {
        SessionResponse::Vault(_) => SessionResponse::Vault(VaultResult::VaultError),
        SessionResponse::Egress(_) => SessionResponse::Egress(EgressResult::EgressError),
        SessionResponse::Channel(_) => SessionResponse::Channel(ChannelResult::ChannelError),
        SessionResponse::Memory(_) => SessionResponse::Memory(MemoryResult::MemoryError),
        SessionResponse::Skill(_) => SessionResponse::Skill(SkillResult::SkillError),
        SessionResponse::Goals(_) => SessionResponse::Goals(GoalsResult::GoalsError),
        SessionResponse::Query(_) => SessionResponse::Query(QueryResult::QueryError),
        SessionResponse::Schedule(_) => SessionResponse::Schedule(ScheduleResult::ScheduleError),
        SessionResponse::Hook(_) => SessionResponse::Hook(HookResult::HookError),
        SessionResponse::Artifact(_) => {
            SessionResponse::Artifact(hatchling_core::ArtifactResult::ArtifactError)
        }
        SessionResponse::Subagent(_) => {
            SessionResponse::Subagent(hatchling_core::SubagentResult::SubagentError)
        }
        SessionResponse::Space(_) => {
            SessionResponse::Space(hatchling_core::SpaceResult::SpaceError)
        }
        SessionResponse::Web(_) => SessionResponse::Web(hatchling_core::WebResult::Refused {
            reason: String::new(),
        }),
        SessionResponse::Browser(_) => {
            SessionResponse::Browser(hatchling_core::BrowserResult::Refused {
                reason: String::new(),
            })
        }
    }
}

/// Maps a frame-layer failure mid-session onto the session vocabulary.
///
/// Only `FrameIo` and `FrameProtocol` can occur here; anything else is a
/// bug, and bugs fail closed as protocol violations rather than panics.
pub(crate) fn session_error(error: hatchling_core::Error) -> Error {
    // No wildcard arm: `wildcard_enum_match_arm` is deny, and a future
    // core variant must be a conscious decision here, not a silent
    // fallthrough.
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::SessionIo { source };
    }
    if let hatchling_core::Error::FrameProtocol { reason } = error {
        return Error::SessionProtocol { reason };
    }
    Error::SessionProtocol {
        reason: format!("unexpected session failure: {error}"),
    }
}

/// The credential lookup an in-process proxy is given, with no egress
/// authority running: maps a destination to its vault secret name and
/// decrypts the sealed credential with the secrets store's key. Unknown
/// destinations, unparseable names, and undecryptable files all yield
/// `None` — the proxy treats that as "no credential" and denies the
/// destination rather than failing the session.
fn credential_fetcher(
    vault_root: std::path::PathBuf,
    key: &dyn OpensStores,
) -> impl Fn(&str) -> Option<Zeroizing<Vec<u8>>> + '_ {
    move |destination: &str| {
        // Map destination to vault secret name. The authorization is
        // the egress destination grant `handle_egress_op` checked
        // before we get here (and the proxy's allowlist), not a vault
        // grant: this is the broker's own read, and no vault grant may
        // name the secret. The sealed vault borrows the broker's store
        // keys.
        let name = destination_to_secret_name(destination)
            .parse::<SecretName>()
            .ok()?;
        let secret = Secrets::new(&vault_root, key)
            .broker_read("egress", std::slice::from_ref(&name))
            .ok()?
            .pop()??;
        Some(Zeroizing::new(secret.as_bytes().to_vec()))
    }
}

/// Initializes the egress proxy for a session served in-process, with no
/// egress authority running (the authority builds its own the same way,
/// reading each credential from the keeper).
///
/// The proxy is built over the config's allowlist, resolved once and
/// shared by every session (see [`crate::EgressAllowlist`]): the first
/// session resolves it, and its addresses stay pinned for the broker's
/// life. The HMAC key is generated fresh per session; surrogates do not
/// survive reconnects.
///
/// The credential fetcher maps a destination to the vault secret name
/// holding its credential. v0.1 uses a fixed mapping: the destination
/// `host:port` maps to a secret named `egress-{host}-{port}` with
/// non-alphanumeric characters replaced. No vault grant is involved: the
/// broker reads the secret itself, after `handle_egress_op` has checked
/// the peer's *egress destination* grant and the proxy its allowlist.
/// Config parsing refuses any vault grant that names such a secret, so
/// the cell cannot reach it through the vault ops.
///
/// # Errors
///
/// Returns [`Error::Egress`] if the allowlist is invalid or a
/// destination doesn't resolve, or [`Error::InvalidConfig`] for a bad
/// CA bundle.
#[allow(
    clippy::type_complexity,
    reason = "the credential-lookup closure type is inherent to the proxy's generic design; a named type would add indirection without clarity"
)]
fn init_egress_proxy<'a>(
    egress_config: &EgressConfig,
    vault_config: &VaultConfig,
    key: &'a dyn OpensStores,
) -> Result<Proxy<impl Fn(&str) -> Option<Zeroizing<Vec<u8>>> + 'a>, Error> {
    let vault_root = vault_config.root.clone();
    let tls = egress_tls(|key| std::env::var(key).ok())?;
    let allowlist = egress_config
        .allowlist
        .resolve()
        .map_err(|source| Error::Egress { source })?;
    Proxy::shared(allowlist, tls, credential_fetcher(vault_root, key))
        .map_err(|source| Error::Egress { source })
}

/// The egress proxy's HTTPS root store.
///
/// The bundled Mozilla roots, plus every certificate in the PEM file
/// `HATCHLING_EGRESS_CA_BUNDLE` names, when it is set (hatchling's own CA,
/// or a local service's). The system CA store is never read.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] for a bundle path that is not absolute,
/// cannot be read, or holds no usable certificate.
pub fn egress_tls(
    get: impl Fn(&str) -> Option<String>,
) -> Result<hatchling_egress::TlsClient, Error> {
    const KEY: &str = env::HATCHLING_EGRESS_CA_BUNDLE.name;
    let invalid = |reason: String| Error::InvalidConfig { key: KEY, reason };
    let Some(raw) = get(KEY).filter(|raw| !raw.trim().is_empty()) else {
        return hatchling_egress::TlsClient::bundled().map_err(|error| invalid(error.to_string()));
    };
    let path = std::path::PathBuf::from(raw.trim());
    if !path.is_absolute() {
        return Err(invalid(format!(
            "must be an absolute path: {}",
            path.display()
        )));
    }
    let bundle =
        std::fs::read(&path).map_err(|error| invalid(format!("{}: {error}", path.display())))?;
    hatchling_egress::TlsClient::with_operator_bundle(&bundle)
        .map_err(|error| invalid(format!("{}: {error}", path.display())))
}

/// Maps a `host:port` destination to the vault secret name holding its
/// credential: `egress-{host}-{port}` with non-alphanumeric characters
/// replaced by `-`.
///
/// The one definition of the mapping: the credential lookup reads these
/// names, and config parsing protects exactly these names from vault
/// grants. It is lossy — distinct destinations can share a
/// name — so neither side may reimplement it.
pub(crate) fn destination_to_secret_name(destination: &str) -> String {
    let sanitized: String = destination
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("egress-{sanitized}")
}

/// Authorizes and executes one egress op, producing the reply.
///
/// The grant check fires before the proxy is touched: an op for a
/// destination the broker holds no grant for never reaches the network,
/// so ungranted destinations cannot be probed. Denials answer `Denied`
/// — the agent learns nothing beyond "no".
///
/// For `MintSurrogate`: the broker checks the egress grant for the
/// destination, then asks the proxy to mint. For `Egress`: the broker
/// checks the grant, then the proxy validates the surrogate, allowlist,
/// and peer binding before forwarding.
///
/// A body too large for one frame moves in parts through `pages` (see
/// [`crate::egress_pages`]): `Stage` adds to a request's body before its
/// `Egress` op, and `Read` returns the next part of an answer an `Ok`
/// said the broker holds. Neither names a destination: staging only
/// fills the session's bounded stage, and a part read was already
/// checked as its request's answer.
///
/// Backend failures collapse to the opaque `EgressError` reply: the
/// session survives them, and the agent learns that the op failed, not
/// why.
///
/// # Errors
///
/// Returns [`Error::SessionProtocol`] if the op's body is not valid
/// base64.
pub(crate) fn handle_egress_op(
    proxy: &dyn EgressPath,
    egress_config: &EgressConfig,
    op: &EgressOp,
    cell_id: &CellId,
    peer_uid: u32,
    pages: &mut crate::egress_pages::Pages,
) -> Result<EgressResult, Error> {
    let destination = match op {
        EgressOp::MintSurrogate { destination, .. } | EgressOp::Egress { destination, .. } => {
            destination
        }
        EgressOp::Stage { .. } | EgressOp::Read => return handle_paging_op(op, pages),
    };

    // Grant check: the peer must hold a grant for this destination.
    // Grants are exact destination strings, deny-by-default.
    let granted = egress_config.grants.iter().any(|g| g == destination);
    if !granted {
        tracing::warn!(
            cell_id = %cell_id,
            // The cell chose it: logged escaped, so it can't forge a line.
            destination = ?destination,
            "denied egress op: no grant"
        );
        crate::machine::decided(cell_id.as_str(), destination, EgressVerdict::NoGrant);
        return Ok(EgressResult::Denied);
    }

    match op {
        // Answered above: neither names a destination.
        EgressOp::Stage { .. } | EgressOp::Read => Ok(EgressResult::EgressError),
        EgressOp::MintSurrogate { ttl_secs, .. } => {
            Ok(mint_for(proxy, cell_id, peer_uid, destination, *ttl_secs))
        }
        EgressOp::Egress {
            method,
            path,
            headers,
            body_b64,
            surrogate_b64,
            staged,
            ..
        } => {
            let rest = decode_secret(body_b64).map_err(|e| Error::SessionProtocol {
                reason: format!("egress body is not valid base64: {e}"),
            })?;
            // A new request's answer replaces any still held.
            pages.drop_answer();
            let Some(body) = pages.body(*staged, &rest) else {
                tracing::warn!(
                    cell_id = %cell_id,
                    destination = %destination,
                    "egress request refused: its staged body doesn't match the stage"
                );
                return Ok(EgressResult::EgressError);
            };
            let request = EgressRequest {
                method: method.clone(),
                destination: destination.clone(),
                path: path.clone(),
                headers: headers.clone(),
                body,
                surrogate_b64: surrogate_b64.clone(),
            };
            let forwarded = proxy.forward(peer_uid, &request);
            crate::machine::decided(cell_id.as_str(), destination, egress_verdict(&forwarded));
            match forwarded {
                Ok(response) => Ok(ok_reply(response, pages)),
                Err(refusal) if refusal.kind == RefusalKind::Failed => {
                    tracing::error!(
                        cell_id = %cell_id,
                        destination = %destination,
                        reason = %refusal.reason,
                        "egress request failed"
                    );
                    Ok(EgressResult::EgressError)
                }
                Err(refusal) => {
                    tracing::warn!(
                        cell_id = %cell_id,
                        destination = %destination,
                        reason = %refusal.reason,
                        "egress request denied"
                    );
                    Ok(EgressResult::Denied)
                }
            }
        }
    }
}

/// Mints `cell_id`'s surrogate for `destination`, granted, living at most
/// an hour whatever `ttl_secs` asks: longer is a confused-deputy risk.
/// A refusal is kept for the operator.
fn mint_for(
    proxy: &dyn EgressPath,
    cell_id: &CellId,
    peer_uid: u32,
    destination: &str,
    ttl_secs: u64,
) -> EgressResult {
    match proxy.mint(peer_uid, destination, ttl_secs.min(3600)) {
        Ok(surrogate_b64) => {
            tracing::info!(
                cell_id = %cell_id,
                destination = %destination,
                "minted egress surrogate"
            );
            EgressResult::Surrogate { surrogate_b64 }
        }
        Err(refusal) => {
            tracing::warn!(
                cell_id = %cell_id,
                destination = %destination,
                reason = %refusal.reason,
                "egress surrogate mint failed"
            );
            crate::machine::decided(cell_id.as_str(), destination, EgressVerdict::NotAllowed);
            EgressResult::Denied
        }
    }
}

/// The operator's verdict on a forwarded request's outcome.
const fn egress_verdict<T>(forwarded: &Result<T, Refusal>) -> EgressVerdict {
    match forwarded {
        Ok(_) => EgressVerdict::Allowed,
        Err(refusal) => match refusal.kind {
            RefusalKind::NotAllowed => EgressVerdict::NotAllowed,
            RefusalKind::Policy => EgressVerdict::Policy,
            RefusalKind::BadSurrogate => EgressVerdict::BadSurrogate,
            RefusalKind::Failed => EgressVerdict::Failed,
        },
    }
}

/// Answers the ops that move a body in parts (see [`crate::egress_pages`]):
/// `Stage` adds a part to the session's stage, `Read` returns the next
/// part of the answer it holds. Either fails, opaquely, past the stage's
/// bound or with nothing to read. The others name a destination and
/// are refused here.
///
/// # Errors
///
/// Returns [`Error::SessionProtocol`] if a part is not valid base64.
fn handle_paging_op(
    op: &EgressOp,
    pages: &mut crate::egress_pages::Pages,
) -> Result<EgressResult, Error> {
    Ok(match op {
        EgressOp::Stage { part_b64 } => {
            let part = decode_secret(part_b64).map_err(|e| Error::SessionProtocol {
                reason: format!("egress body part is not valid base64: {e}"),
            })?;
            pages
                .stage(&part)
                .map_or(EgressResult::EgressError, |held| EgressResult::Staged {
                    held,
                })
        }
        EgressOp::Read => pages
            .next_part()
            .map_or(EgressResult::EgressError, |(part, more)| {
                EgressResult::Part {
                    body_b64: encode_secret(&part),
                    more,
                }
            }),
        EgressOp::MintSurrogate { .. } | EgressOp::Egress { .. } => EgressResult::EgressError,
    })
}

/// The `Ok` reply to a forwarded request: the whole answer, if it fits
/// one frame; otherwise its status and headers, with the body held in
/// `pages` for the agent to read in parts.
fn ok_reply(
    response: hatchling_egress::EgressResponse,
    pages: &mut crate::egress_pages::Pages,
) -> EgressResult {
    let whole = SessionResponse::Egress(EgressResult::Ok {
        status: response.status,
        headers: response.headers.clone(),
        body_b64: encode_secret(&response.body),
        more: 0,
    });
    if hatchling_core::fits_one_frame(&whole) {
        if let SessionResponse::Egress(whole) = whole {
            return whole;
        }
    }
    let more = pages.hold(response.body);
    EgressResult::Ok {
        status: response.status,
        headers: response.headers,
        body_b64: String::new(),
        more,
    }
}

/// Sends a `REJECT` frame carrying the given reason.
///
/// Only called for authorized peers whose complete bytes were invalid —
/// never for peers that failed the peer gate, and never for framing
/// failures where no complete message exists to refuse.
fn reject(stream: &UnixStream, reason: &str) -> Result<(), Error> {
    let message = BrokerMessage::Reject {
        v: PROTOCOL_VERSION,
        reason: reason.to_owned(),
    };
    let payload = serde_json::to_vec(&message).map_err(|e| Error::HandshakeProtocol {
        reason: format!("failed to encode reject: {e}"),
    })?;
    write_frame(stream, &payload)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Through a spawner that's gone, the agent cell isn't started: the
    /// spawn says so, and the directory made for it goes with it.
    #[test]
    fn an_agent_cell_through_a_gone_spawner_fails_and_leaves_nothing() {
        let temp =
            std::env::temp_dir().join(format!("hatchling-serve-agent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let (ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        let spawner = crate::spawner::Spawner::over(ours);
        let mut cell = CellConfig::skill("agent").unwrap();
        cell.connected = true;
        let failed = spawn_through(&spawner, &cell, "agent".parse().unwrap(), &temp).unwrap_err();
        assert!(matches!(failed, Error::CellSpawn { .. }), "{failed}");
        assert_eq!(
            std::fs::read_dir(&temp).unwrap().count(),
            0,
            "the agent cell's directory was left"
        );
        std::fs::remove_dir(&temp).unwrap();
    }
    use hatchling_core::VaultOp;
    use hatchling_vault::{SealedVault, Secret};

    use crate::secrets::vault_op_result;
    use hatchling_vault::MasterKey;

    #[test]
    fn a_hello_cannot_forge_a_line_in_the_brokers_log() {
        // The cell chose the variant name serde echoes back.
        let (agent, broker) = UnixStream::pair().unwrap();
        write_frame(&agent, br#"{"t":"x\nFORGED cell admitted"}"#).unwrap();
        let logged = crate::test_log::captured(|| {
            assert!(handshake(&broker, None).is_err());
        });
        crate::test_log::assert_no_forged_line(&logged, "FORGED cell admitted");
    }

    #[test]
    fn a_silent_peer_times_out_before_its_hello() {
        let (_agent, broker) = UnixStream::pair().unwrap();
        // On its own thread, so a hello read that never times out fails
        // this test instead of hanging it.
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let read = read_hello(&broker, Duration::from_millis(100));
            let _ = done.send((read, started.elapsed()));
        });
        let (read, took) = result
            .recv_timeout(Duration::from_secs(10))
            .expect("the hello read never timed out");
        assert!(
            matches!(&read, Err(Error::HandshakeIo { source })
                if matches!(source.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "{read:?}"
        );
        // It waited: the kernel may wake it a tick early, never at once.
        assert!(took >= Duration::from_millis(50), "{took:?}");
    }

    #[test]
    fn the_handshake_welcomes_only_the_cell_it_expects() {
        let hello = |agent: &UnixStream, cell: &str| {
            let hello = AgentMessage::Hello {
                v: PROTOCOL_VERSION,
                cell_id: cell.to_owned(),
            };
            write_frame(agent, &serde_json::to_vec(&hello).unwrap()).unwrap();
        };
        let reply = |agent: &UnixStream| -> BrokerMessage {
            serde_json::from_slice(&read_frame(agent).unwrap()).unwrap()
        };
        let ours: CellId = "cell-01".parse().unwrap();
        // Expecting this cell: welcomed, and its id returned.
        let (agent, broker) = UnixStream::pair().unwrap();
        hello(&agent, "cell-01");
        assert_eq!(handshake(&broker, Some(&ours)).unwrap(), ours);
        assert!(matches!(reply(&agent), BrokerMessage::Welcome { .. }));
        // Another cell's id: refused, with the reason on the wire.
        let (agent, broker) = UnixStream::pair().unwrap();
        hello(&agent, "cell-02");
        assert!(matches!(
            handshake(&broker, Some(&ours)),
            Err(Error::HandshakeProtocol { reason })
                if reason == "the hello names cell cell-02, but this connection is cell-01's"
        ));
        assert!(matches!(reply(&agent), BrokerMessage::Reject { .. }));
        // Expecting nothing in particular (the socket): any valid id.
        let (agent, broker) = UnixStream::pair().unwrap();
        hello(&agent, "cell-02");
        assert_eq!(handshake(&broker, None).unwrap().as_str(), "cell-02");
    }

    #[test]
    fn a_hello_in_time_is_read_and_the_session_has_no_timeout() {
        let (agent, broker) = UnixStream::pair().unwrap();
        write_frame(&agent, b"hello").unwrap();
        assert_eq!(read_hello(&broker, HANDSHAKE_TIMEOUT).unwrap(), b"hello");
        assert_eq!(broker.read_timeout().unwrap(), None);
        // A peer that closes instead is an I/O failure too.
        drop(agent);
        assert!(matches!(
            read_hello(&broker, HANDSHAKE_TIMEOUT),
            Err(Error::HandshakeIo { .. })
        ));
    }

    /// `destination_to_secret_name` maps `host:port` to
    /// `egress-{host}-{port}` with non-alphanumerics as `-`. Pins the
    /// return-value substitution mutants (`String::new()`, `"xyzzy"`).
    #[test]
    fn secret_name_mapping() {
        assert_eq!(
            destination_to_secret_name("api.github.com:443"),
            "egress-api-github-com-443"
        );
        assert_eq!(
            destination_to_secret_name("localhost:8080"),
            "egress-localhost-8080"
        );
        // Non-alphanumeric characters become '-'.
        assert_eq!(
            destination_to_secret_name("my_host.example.com:443"),
            "egress-my-host-example-com-443"
        );
    }

    /// `credential_fetcher` decrypts through the sealed vault with the
    /// broker's key: the on-disk bytes are an `HVL3` envelope (no
    /// plaintext), and the fetcher returns the cleartext credential
    /// for the mapped destination. Pins the wiring — a mutant
    /// that reads the vault unsealed (or with the wrong key) makes the
    /// fetcher return `None`.
    #[test]
    fn credential_fetcher_reads_sealed_credentials() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!(
            "hatchling-test-egress-fetcher-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let key = MasterKey::generate().unwrap();
        let vault = SealedVault::open(&dir, &key).unwrap();
        let name: SecretName = "egress-example-com-443".parse().unwrap();
        vault
            .store(&name, &Secret::new(b"credential-123".to_vec()).unwrap())
            .unwrap();

        // On-disk proof of sealing: versioned envelope, no plaintext.
        let raw = std::fs::read(dir.join("egress-example-com-443")).unwrap();
        assert_eq!(raw.get(..4), Some(b"HVL3".as_slice()));
        assert!(
            !raw.windows(b"credential-123".len())
                .any(|window| window == b"credential-123"),
            "plaintext credential on disk"
        );

        // The exact fetcher the broker hands the proxy decrypts it.
        let fetcher = credential_fetcher(dir.clone(), &key);
        let credential = fetcher("example.com:443").expect("sealed credential must decrypt");
        assert_eq!(&credential[..], b"credential-123");

        // Fail-closed surfaces: unknown destination, and a fetcher
        // bound to the wrong key, both yield `None`.
        assert!(fetcher("unknown.example:1").is_none());
        let wrong_key = MasterKey::generate().unwrap();
        let wrong_fetcher = credential_fetcher(dir.clone(), &wrong_key);
        assert!(wrong_fetcher("example.com:443").is_none());

        // The vault root keeps its 0700 discipline even in the test.
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        // Each of those reads is the broker's own, on record.
        let (accesses, _) = crate::vault_access::read_log(&dir).unwrap();
        let seen: Vec<(&str, crate::vault_access::Outcome)> = accesses
            .iter()
            .map(|access| {
                (
                    access.names.first().map_or("", String::as_str),
                    access.outcome,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "egress-example-com-443",
                    crate::vault_access::Outcome::Allowed
                ),
                (
                    "egress-unknown-example-1",
                    crate::vault_access::Outcome::NotFound
                ),
                (
                    "egress-example-com-443",
                    crate::vault_access::Outcome::Failed
                ),
            ]
        );
        assert!(accesses.iter().all(|access| access.caller
            == crate::vault_access::Caller::Broker {
                purpose: String::from("egress")
            }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// the egress root store comes from `HATCHLING_EGRESS_CA_BUNDLE`
    /// or is the bundled set; a bad bundle stops the broker, and every
    /// session's proxy is built with it.
    #[test]
    fn an_answer_too_large_for_one_frame_is_held_and_read_in_parts() {
        let mut pages = crate::egress_pages::Pages::default();
        let answer = |body: Vec<u8>| hatchling_egress::EgressResponse {
            status: 200,
            headers: vec![(String::from("X-A"), String::from("1"))],
            body,
        };
        // Small: whole, nothing held.
        assert_eq!(
            ok_reply(answer(b"ok".to_vec()), &mut pages),
            EgressResult::Ok {
                status: 200,
                headers: vec![(String::from("X-A"), String::from("1"))],
                body_b64: encode_secret(b"ok"),
                more: 0,
            }
        );
        assert_eq!(pages.next_part(), None);
        // 512 KiB: the status and headers now, the body held.
        let large = vec![b'x'; 512 << 10];
        assert_eq!(
            ok_reply(answer(large.clone()), &mut pages),
            EgressResult::Ok {
                status: 200,
                headers: vec![(String::from("X-A"), String::from("1"))],
                body_b64: String::new(),
                more: 512 << 10,
            }
        );
        let mut read = Vec::new();
        while let Some((part, _)) = pages.next_part() {
            read.extend_from_slice(&part);
        }
        assert_eq!(read, large);
    }

    #[test]
    fn handle_egress_op_stages_a_body_and_reads_a_held_answer() {
        // Its own egress decisions must still be kept when it reads them.
        let _flood = crate::machine::FLOOD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir =
            std::env::temp_dir().join(format!("hatchling-egress-pages-{}", std::process::id()));
        let key = MasterKey::generate().unwrap();
        let egress = EgressConfig {
            allowlist: crate::EgressAllowlist::new("internal:localhost:443"),
            grants: vec![String::from("localhost:443")],
        };
        let vault = VaultConfig {
            key_file: VaultConfig::default_key_file(&dir),
            root: dir.join("vault"),
            grants: Vec::new(),
        };
        let proxy = init_egress_proxy(&egress, &vault, &key).unwrap();
        let cell_id: CellId = "cell-pages".parse().unwrap();
        let mut pages = crate::egress_pages::Pages::default();
        let mut op =
            |op: EgressOp| handle_egress_op(&proxy, &egress, &op, &cell_id, 1000, &mut pages);
        let stage = |part: &[u8]| EgressOp::Stage {
            part_b64: encode_secret(part),
        };
        assert_eq!(op(stage(b"abc")).unwrap(), EgressResult::Staged { held: 3 });
        assert_eq!(op(stage(b"de")).unwrap(), EgressResult::Staged { held: 5 });
        // Nothing held to read.
        assert_eq!(op(EgressOp::Read).unwrap(), EgressResult::EgressError);
        // A request whose `staged` doesn't match the stage is refused
        // before it reaches the proxy, and the stage is emptied.
        let request = |staged: u64| EgressOp::Egress {
            method: String::from("POST"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: Vec::new(),
            body_b64: String::new(),
            surrogate_b64: String::from("c3Vycm9nYXRl"),
            staged,
        };
        assert_eq!(op(request(4)).unwrap(), EgressResult::EgressError);
        assert_eq!(op(stage(b"f")).unwrap(), EgressResult::Staged { held: 1 });
        // A matching one reaches the proxy, which refuses the surrogate.
        assert_eq!(op(request(1)).unwrap(), EgressResult::Denied);
        assert!(matches!(
            op(EgressOp::Stage {
                part_b64: String::from("not base64!")
            }),
            Err(Error::SessionProtocol { .. })
        ));
        // A destination with no grant is refused before the proxy.
        let ungranted = EgressOp::Egress {
            method: String::from("GET"),
            destination: String::from("ungranted.example:443"),
            path: String::from("/"),
            headers: Vec::new(),
            body_b64: String::new(),
            surrogate_b64: String::new(),
            staged: 0,
        };
        assert_eq!(op(ungranted).unwrap(), EgressResult::Denied);
        // Granted but not on the proxy's allowlist: its mint is refused.
        let granted_elsewhere = EgressConfig {
            allowlist: crate::EgressAllowlist::new("internal:localhost:443"),
            grants: vec![String::from("elsewhere.example:443")],
        };
        let mint = EgressOp::MintSurrogate {
            destination: String::from("elsewhere.example:443"),
            ttl_secs: 60,
        };
        assert_eq!(
            handle_egress_op(
                &proxy,
                &granted_elsewhere,
                &mint,
                &cell_id,
                1000,
                &mut pages
            )
            .unwrap(),
            EgressResult::Denied
        );
        // Minting directly: an allowlisted destination gets its surrogate,
        // and the refusal for another is kept.
        assert!(matches!(
            mint_for(&proxy, &cell_id, 1000, "localhost:443", 7200),
            EgressResult::Surrogate { .. }
        ));
        // Each decision is kept for the operator, in order.
        let kept: Vec<(String, EgressVerdict)> = crate::machine::decisions()
            .into_iter()
            .filter(|decision| decision.cell_id == "cell-pages")
            .map(|decision| (decision.destination, decision.verdict))
            .collect();
        assert_eq!(
            kept,
            [
                (String::from("localhost:443"), EgressVerdict::BadSurrogate),
                (
                    String::from("ungranted.example:443"),
                    EgressVerdict::NoGrant
                ),
                (
                    String::from("elsewhere.example:443"),
                    EgressVerdict::NotAllowed
                )
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_egress_outcome_has_its_verdict() {
        let forwarded: Result<(), Refusal> = Ok(());
        assert_eq!(egress_verdict(&forwarded), EgressVerdict::Allowed);
        for (kind, verdict) in [
            (RefusalKind::NotAllowed, EgressVerdict::NotAllowed),
            (RefusalKind::Policy, EgressVerdict::Policy),
            (RefusalKind::BadSurrogate, EgressVerdict::BadSurrogate),
            (RefusalKind::Failed, EgressVerdict::Failed),
        ] {
            let refused: Result<(), Refusal> = Err(Refusal {
                kind,
                reason: String::new(),
            });
            assert_eq!(egress_verdict(&refused), verdict);
        }
    }

    #[test]
    fn handle_paging_op_answers_only_the_paging_ops() {
        let mut pages = crate::egress_pages::Pages::default();
        let _ = pages.hold(b"held".to_vec());
        assert_eq!(
            handle_paging_op(&EgressOp::Read, &mut pages).unwrap(),
            EgressResult::Part {
                body_b64: encode_secret(b"held"),
                more: 0,
            }
        );
        let stage = EgressOp::Stage {
            part_b64: encode_secret(b"ab"),
        };
        assert_eq!(
            handle_paging_op(&stage, &mut pages).unwrap(),
            EgressResult::Staged { held: 2 }
        );
        let mint = EgressOp::MintSurrogate {
            destination: String::from("localhost:443"),
            ttl_secs: 60,
        };
        assert_eq!(
            handle_paging_op(&mint, &mut pages).unwrap(),
            EgressResult::EgressError
        );
    }

    #[test]
    fn the_egress_root_store_is_the_operators_choice_or_the_bundled_set() {
        let none = |_: &str| None;
        assert_eq!(egress_tls(none).unwrap().operator_roots(), 0);
        let blank = |_: &str| Some(String::from("  "));
        assert_eq!(egress_tls(blank).unwrap().operator_roots(), 0);
        let dir =
            std::env::temp_dir().join(format!("hatchling-test-egress-tls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("junk.pem"), b"no certificate here").unwrap();
        let junk = dir.join("junk.pem").to_string_lossy().into_owned();
        let missing = dir.join("missing.pem").to_string_lossy().into_owned();
        for (raw, why) in [
            (String::from("relative/ca.pem"), "must be an absolute path"),
            (missing, "No such file"),
            (junk, "holds no certificate"),
        ] {
            let refused = egress_tls(|_: &str| Some(raw.clone()));
            assert!(
                matches!(&refused, Err(Error::InvalidConfig { key: "HATCHLING_EGRESS_CA_BUNDLE", reason }) if reason.contains(why)),
                "{why}: {refused:?}"
            );
        }
        let key = MasterKey::generate().unwrap();
        let egress = EgressConfig {
            allowlist: crate::EgressAllowlist::new("internal:localhost:443"),
            grants: Vec::new(),
        };
        let vault = VaultConfig {
            key_file: VaultConfig::default_key_file(&dir),
            root: dir.join("vault"),
            grants: Vec::new(),
        };
        // Two sessions' proxies: one resolution of the allowlist between
        // them, each with its own surrogate key.
        let first = init_egress_proxy(&egress, &vault, &key).unwrap();
        let second = init_egress_proxy(&egress.clone(), &vault, &key).unwrap();
        assert!(std::ptr::eq(first.allowlist(), second.allowlist()));
        assert!(first.allowlist().allows("localhost:443"));
        let surrogate = first.mint_surrogate(1000, "localhost:443", 60).unwrap();
        let request = EgressRequest {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: Vec::new(),
            body: Vec::new(),
            surrogate_b64: surrogate,
        };
        assert!(matches!(
            second.forward(1000, &request),
            Err(hatchling_egress::Error::InvalidSurrogate)
        ));
        let bad = EgressConfig {
            allowlist: crate::EgressAllowlist::new("*.example.com:443"),
            grants: Vec::new(),
        };
        assert!(matches!(
            init_egress_proxy(&bad, &vault, &key),
            Err(Error::Egress { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// every vault op is recorded against the session's uid and
    /// cell, whatever its outcome; an allowed op that cannot be recorded is
    /// answered as failed, so no secret leaves unlogged; a denial never
    /// creates the vault root to record itself.
    #[test]
    fn every_vault_op_is_recorded_and_an_unrecorded_one_fails_closed() {
        use crate::vault_access::Outcome;
        let dir = std::env::temp_dir().join(format!(
            "hatchling-test-vault-access-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let key = MasterKey::generate().unwrap();
        let vault = VaultConfig {
            key_file: VaultConfig::default_key_file(&dir),
            root: dir.clone(),
            grants: vec![
                crate::VaultGrant {
                    action: hatchling_core::VaultAction::Store,
                    name: "token".parse().unwrap(),
                },
                crate::VaultGrant {
                    action: hatchling_core::VaultAction::Fetch,
                    name: "token".parse().unwrap(),
                },
            ],
        };
        let cell: CellId = "cell-v".parse().unwrap();
        let fetch = |name: &str| VaultOp::Fetch {
            name: name.to_owned(),
        };
        let remove = VaultOp::Remove {
            name: String::from("token"),
        };
        // Denied before the vault exists: answered, traced, nothing created.
        assert_eq!(
            handle_vault_op(&vault, &key, &remove, &cell, 7).unwrap(),
            VaultResult::Denied
        );
        assert!(!dir.exists(), "a denial never creates the vault root");
        let store = VaultOp::Store {
            name: String::from("token"),
            secret_b64: encode_secret(b"s3cret").into(),
        };
        assert_eq!(
            handle_vault_op(&vault, &key, &store, &cell, 7).unwrap(),
            VaultResult::Stored
        );
        assert!(matches!(
            handle_vault_op(&vault, &key, &fetch("token"), &cell, 7).unwrap(),
            VaultResult::Fetched { .. }
        ));
        assert_eq!(
            handle_vault_op(&vault, &key, &remove, &cell, 7).unwrap(),
            VaultResult::Denied
        );
        assert_eq!(
            handle_vault_op(&vault, &key, &fetch("bad name"), &cell, 7).unwrap(),
            VaultResult::Denied
        );
        let (accesses, unreadable) = crate::vault_access::read_log(&dir).unwrap();
        assert_eq!(unreadable, 0);
        let seen: Vec<(&str, &str, Outcome)> = accesses
            .iter()
            .map(|access| {
                (
                    access.action.as_str(),
                    access.names.first().map_or("", String::as_str),
                    access.outcome,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("store", "token", Outcome::Allowed),
                ("fetch", "token", Outcome::Allowed),
                ("remove", "token", Outcome::Denied),
                ("fetch", "(not a secret name)", Outcome::Denied),
            ]
        );
        assert!(accesses.iter().all(|access| access.caller
            == crate::vault_access::Caller::Session {
                uid: 7,
                cell: String::from("cell-v")
            }));
        let text = std::fs::read_to_string(dir.join(crate::vault_access::ACCESS_LOG)).unwrap();
        assert!(!text.contains("s3cret"), "names only, never a value");
        // The log cannot be written: the fetch is answered as failed.
        std::fs::remove_file(dir.join(crate::vault_access::ACCESS_LOG)).unwrap();
        std::fs::create_dir(dir.join(crate::vault_access::ACCESS_LOG)).unwrap();
        assert_eq!(
            handle_vault_op(&vault, &key, &fetch("token"), &cell, 7).unwrap(),
            VaultResult::VaultError
        );
        // An op that did not go ahead is still answered as it was.
        assert_eq!(
            handle_vault_op(&vault, &key, &remove, &cell, 7).unwrap(),
            VaultResult::Denied
        );
        assert!(matches!(
            vault_op_result(&vault, &key, &fetch("token"), &cell).unwrap(),
            VaultResult::Fetched { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod reply_bound_tests {
    use super::*;
    use hatchling_core::{SecretB64, VaultOp};

    const fn opaque_errors() -> [SessionResponse; 7] {
        [
            SessionResponse::Vault(VaultResult::VaultError),
            SessionResponse::Egress(EgressResult::EgressError),
            SessionResponse::Channel(ChannelResult::ChannelError),
            SessionResponse::Memory(MemoryResult::MemoryError),
            SessionResponse::Skill(SkillResult::SkillError),
            SessionResponse::Goals(GoalsResult::GoalsError),
            SessionResponse::Query(QueryResult::QueryError),
        ]
    }

    #[test]
    fn a_reply_at_the_bound_is_sent_unchanged_and_one_byte_over_is_replaced() {
        let reply = SessionResponse::Memory(MemoryResult::Items {
            items_json: vec![String::from("x").repeat(100)],
            next: None,
        });
        let exact = serde_json::to_vec(&reply).unwrap();
        assert_eq!(*encode_reply(&reply, exact.len()).unwrap(), exact);
        assert_eq!(
            *encode_reply(&reply, exact.len() - 1).unwrap(),
            serde_json::to_vec(&SessionResponse::Memory(MemoryResult::MemoryError)).unwrap()
        );
    }

    #[test]
    fn a_request_is_read_from_a_wiped_frame() {
        let store = SessionRequest::Vault(VaultOp::Store {
            name: String::from("token"),
            secret_b64: SecretB64::encode(b"s3cret"),
        });
        let frame = Zeroizing::new(serde_json::to_vec(&store).unwrap());
        assert_eq!(request_of(&frame).unwrap(), store);
        assert!(matches!(
            request_of(&Zeroizing::new(b"{not a request".to_vec())),
            Err(Error::SessionProtocol { reason }) if reason.contains("not a valid protocol message")
        ));
    }

    #[test]
    fn a_fetched_secret_is_encoded_at_its_final_size() {
        let reply = SessionResponse::Vault(VaultResult::Fetched {
            secret_b64: SecretB64::encode(&[0xAB; 3000]),
        });
        let payload = encode_reply(&reply, MAX_FRAME_BYTES).unwrap();
        assert_eq!(*payload, serde_json::to_vec(&reply).unwrap());
        assert_eq!(payload.capacity(), payload.len(), "the buffer grew");
    }

    #[test]
    fn every_kind_falls_back_to_its_own_opaque_error() {
        for reply in opaque_errors() {
            assert_eq!(oversize_reply(&reply), reply);
            // With no room at all, the fallback is still the same kind.
            assert_eq!(
                *encode_reply(&reply, 0).unwrap(),
                serde_json::to_vec(&reply).unwrap()
            );
        }
        let big = SessionResponse::Channel(ChannelResult::Message {
            sender: String::from("uid:1"),
            channel: String::from("local"),
            text: String::from("t"),
        });
        assert_eq!(
            oversize_reply(&big),
            SessionResponse::Channel(ChannelResult::ChannelError)
        );
    }

    #[test]
    fn the_session_bound_is_the_frame_maximum() {
        let reply = SessionResponse::Memory(MemoryResult::Items {
            items_json: vec![String::from("\"").repeat(MAX_FRAME_BYTES)],
            next: None,
        });
        let payload = encode_reply(&reply, MAX_FRAME_BYTES).unwrap();
        assert!(payload.len() <= MAX_FRAME_BYTES);
        assert_eq!(
            *payload,
            serde_json::to_vec(&SessionResponse::Memory(MemoryResult::MemoryError)).unwrap()
        );
    }
}

#[cfg(test)]
mod gate_scope_tests {
    use std::sync::{Arc, Mutex};

    use hatchling_channel::{
        Category, ChannelId, Classifier, Cleared, Message, NoAudit, NoOverrides, OutputGate,
        Request, SenderId, Verdict,
    };
    use hatchling_core::ChannelOp;

    use super::*;
    use hatchling_vault::MasterKey;

    /// Flags anything holding `forbidden`, recording each scope and turn.
    #[derive(Default)]
    struct Flagging(Mutex<Vec<(String, u64)>>);

    impl Classifier for Flagging {
        fn id(&self) -> &'static str {
            "flagging@test"
        }

        fn classify(&self, request: &Request<'_>) -> Result<Verdict, String> {
            self.0
                .lock()
                .unwrap()
                .push((request.scope.to_owned(), request.turn));
            Ok(if request.text.contains("forbidden") {
                Verdict::Redact {
                    category: Category::new("codename").unwrap(),
                    flagged: Some(String::from("forbidden")),
                }
            } else {
                Verdict::Allow
            })
        }
    }

    fn ask(stream: &UnixStream, op: ChannelOp) -> ChannelResult {
        write_frame(
            stream,
            &serde_json::to_vec(&SessionRequest::Channel(op)).unwrap(),
        )
        .unwrap();
        let response: SessionResponse =
            serde_json::from_slice(&read_frame(stream).unwrap()).unwrap();
        let SessionResponse::Channel(result) = response else {
            unreachable!("not a channel reply: {response:?}");
        };
        result
    }

    /// A session's replies are judged in its own `session:<id>` scope; a
    /// redacted send tells the cell the category and the event id; and the
    /// scope is forgotten when the session ends, so the next delivery in
    /// it starts again at turn 0.
    #[test]
    fn a_session_is_judged_in_its_own_scope_until_it_ends() {
        let flagging = Arc::new(Flagging::default());
        let classifier: Arc<dyn Classifier> = flagging.clone();
        let hub = LocalHub::new(OutputGate::new(
            classifier,
            Arc::new(NoAudit),
            Arc::new(NoOverrides),
            hatchling_channel::gate::DEFAULT_CLASSIFIER_TIMEOUT,
        ));
        let shown = Arc::new(Mutex::new(Vec::new()));
        let sink_shown = Arc::clone(&shown);
        let _ = hub.attach(Box::new(move |cleared: &Cleared| {
            sink_shown
                .lock()
                .unwrap()
                .push(cleared.message().text.clone());
            Ok(())
        }));
        let config = crate::api_auth::tests::owner_config(None);
        let key = MasterKey::generate().unwrap();
        let (agent, broker) = UnixStream::pair().unwrap();
        let creds = peer_credentials(&broker).unwrap();
        let cell_id: CellId = "cell-gate".parse().unwrap();
        let skills = Skills::default();
        let user = |text: &str| Message {
            sender: SenderId(String::from("someone")),
            channel: ChannelId::local(),
            text: text.to_owned(),
        };
        std::thread::scope(|scope| {
            let served = scope.spawn(|| {
                serve_session(
                    &broker,
                    &config,
                    &key,
                    &skills,
                    &cell_id,
                    creds,
                    Some(&hub),
                    None,
                )
            });
            let _ = hub.inject(user("hello"));
            assert!(matches!(
                ask(
                    &agent,
                    ChannelOp::Receive {
                        channel: String::from("local"),
                        account: None
                    }
                ),
                ChannelResult::Message { .. }
            ));
            let send = ChannelOp::Send {
                channel: String::from("local"),
                text: String::from("the forbidden plan"),
                state_changing: false,
                approval: None,
                account: None,
            };
            let ChannelResult::Redacted { category, event_id } = ask(&agent, send) else {
                unreachable!("the send was not redacted");
            };
            assert_eq!(category, "codename");
            assert!(event_id.starts_with("red-"), "{event_id}");
            drop(agent);
            served.join().unwrap().unwrap();
        });
        let judged = flagging.0.lock().unwrap().clone();
        let [(session, 0)] = judged.as_slice() else {
            unreachable!("one judgement in one scope: {judged:?}");
        };
        assert!(session.starts_with("session:"), "{session}");
        assert!(
            shown
                .lock()
                .unwrap()
                .iter()
                .all(|text| !text.contains("forbidden")),
            "the user saw only the notice"
        );
        // Ended: the scope's state went with the session.
        hub.deliver(&user("after"), session).unwrap();
        assert_eq!(
            flagging.0.lock().unwrap().last(),
            Some(&(session.clone(), 0))
        );
    }

    /// A peer that isn't the agent is turned away unanswered: the
    /// connection ends without a byte sent.
    #[test]
    fn handle_connection_sends_an_unauthorized_peer_nothing() {
        let config = crate::api_auth::tests::owner_config(None);
        let key = MasterKey::generate().unwrap();
        let (mut peer, broker) = UnixStream::pair().unwrap();
        handle_connection(&broker, &config, &key, &Skills::default(), None);
        drop(broker);
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut seen = Vec::new();
        std::io::Read::read_to_end(&mut peer, &mut seen).unwrap();
        assert!(seen.is_empty(), "{seen:?}");
    }
}
