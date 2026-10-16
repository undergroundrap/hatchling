//! Cell lifecycle as typestate.
//!
//! Invariant: an operation that requires preconditions cannot be invoked on
//! a cell that has not checked them, and nothing may touch the broker
//! connection before it exists. The type system enforces this:
//! `Cell<Configured>` only offers [`check_preconditions`](Cell::check_preconditions),
//! `Cell<PreconditionsChecked>` only offers
//! [`connect_broker`](Cell::connect_broker),
//! `Cell<BrokerConnected>` only offers [`authorize`](Cell::authorize),
//! and `Cell<PeerAuthorized>` only offers [`handshake`](Cell::handshake).

use std::marker::PhantomData;
use std::os::unix::net::UnixStream;

use crate::{BrokerAddress, CapabilityName, Config, Error, egress, handshake, skill, vault};
use hatchling_core::peer_credentials;

/// Marker: configuration loaded, preconditions not yet checked.
#[derive(Debug)]
pub struct Configured;

/// Marker: startup preconditions checked, broker not yet connected.
///
/// The name is honest about what this actually establishes: the broker
/// socket's parent directory exists. Kernel-level isolation (user
/// namespaces, seccomp) is roadmap, not claimed here.
#[derive(Debug)]
pub struct PreconditionsChecked;

/// Marker: connected to the broker.
///
/// The connection is held open for the lifetime of the cell. Nothing has
/// been trusted yet: the peer is unauthorized until [`authorize`](Cell::authorize)
/// passes.
#[derive(Debug)]
pub struct BrokerConnected;

/// Marker: the broker peer is authorized.
///
/// The kernel-reported peer uid matched the configured broker uid. The
/// peer's bytes are still untrusted until the handshake completes.
#[derive(Debug)]
pub struct PeerAuthorized;

/// Marker: the broker handshake completed.
///
/// The cell is fully online. Post-handshake traffic goes through the
/// vault methods ([`vault_store`](Cell::vault_store),
/// [`vault_fetch`](Cell::vault_fetch), [`vault_remove`](Cell::vault_remove));
/// the raw handle is [`broker`](Cell::broker).
#[derive(Debug)]
pub struct Ready;

/// An agent cell in a given lifecycle `State`.
///
/// The broker connection, once established, is stored here. The field is
/// `None` until [`connect_broker`](Cell::connect_broker) succeeds; no other
/// state can observe a `Some`, because the field is private, the only
/// transition into connection-holding states always sets it, and the only
/// public accessor is [`Cell<Ready>::broker`].
#[derive(Debug)]
pub struct Cell<State> {
    /// Validated configuration. Always present; lifecycle transitions
    /// never change it.
    config: Config,
    /// The open broker connection, if this state has one.
    broker: Option<UnixStream>,
    /// Lifecycle marker. Zero-sized; exists only at compile time.
    state: PhantomData<State>,
}

impl Cell<Configured> {
    /// Creates a cell from validated configuration.
    #[must_use]
    pub const fn new(config: Config) -> Self {
        Self {
            config,
            broker: None,
            state: PhantomData,
        }
    }

    /// Verifies the preconditions the broker connection depends on,
    /// fail-closed.
    ///
    /// Checks what can be checked honestly at this layer: the broker
    /// socket path is absolute (guaranteed by [`SocketPath`](crate::SocketPath))
    /// and its parent directory exists and is a directory, so a missing
    /// mount or misconfiguration fails here instead of at connect time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PreconditionFailed`] if any check fails. The cell
    /// must not run in that case.
    pub fn check_preconditions(self) -> Result<Cell<PreconditionsChecked>, Error> {
        // A cell's own connection has no path to check: adopting it checks
        // what it is.
        let BrokerAddress::Socket(socket) = &self.config.broker else {
            return Ok(Cell {
                config: self.config,
                broker: None,
                state: PhantomData,
            });
        };
        let socket = socket.as_path();
        let parent = socket.parent().ok_or_else(|| {
            Error::PreconditionFailed(format!(
                "broker socket {} has no parent directory",
                socket.display()
            ))
        })?;
        if !parent.is_dir() {
            return Err(Error::PreconditionFailed(format!(
                "broker socket parent {} is not a directory",
                parent.display()
            )));
        }
        Ok(Cell {
            config: self.config,
            broker: None,
            state: PhantomData,
        })
    }
}

impl<State> Cell<State> {
    /// Returns the validated configuration. Available in every state:
    /// configuration is immutable and validated once at startup.
    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// Returns the open broker connection.
    ///
    /// Private contract: only call on states at or past
    /// [`BrokerConnected`]. The field is `Some` in those states by
    /// construction — [`connect_broker`](Cell::connect_broker) is the only
    /// transition that creates them, and it always installs the stream.
    /// The public typestate keeps this honest: the only public
    /// connection accessor is [`Cell<Ready>::broker`].
    ///
    /// # Panics
    ///
    /// Panics if called on a state that cannot hold a connection. Cannot
    /// happen through the public API: the state markers make
    /// `connect_broker` the only path to the states that call this.
    #[allow(
        clippy::expect_used,
        reason = "invariant: connect_broker is the only transition into connection-holding states and always sets the stream"
    )]
    const fn connection(&self) -> &UnixStream {
        self.broker
            .as_ref()
            .expect("broker connection missing on a connected cell")
    }
}

impl Cell<PreconditionsChecked> {
    /// Connects to the broker socket and holds the connection open.
    ///
    /// Synchronous and fail-fast by design: the agent starts once, at
    /// boot, when the broker is either already listening or the machine
    /// is misconfigured. A missing socket, a refusal, or any other I/O
    /// failure is [`Error::BrokerConnect`] and the cell must not run.
    /// There is no retry loop and no degraded brokerless mode — an agent
    /// without its broker is a fail-closed agent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::BrokerConnect`] if the connection cannot be
    /// established.
    pub fn connect_broker(self) -> Result<Cell<BrokerConnected>, Error> {
        let stream = match &self.config.broker {
            BrokerAddress::Socket(socket) => {
                UnixStream::connect(socket.as_path()).map_err(|source| Error::BrokerConnect {
                    socket: socket.clone(),
                    source,
                })?
            }
            BrokerAddress::Connection(fd) => hatchling_sys::net::inherited_unix_stream(*fd)
                .map_err(|source| Error::BrokerConnection {
                    fd: *fd,
                    reason: source.to_string(),
                })?,
        };
        Ok(Cell {
            config: self.config,
            broker: Some(stream),
            state: PhantomData,
        })
    }
}

impl Cell<BrokerConnected> {
    /// Authorizes the broker against kernel-reported peer credentials.
    ///
    /// Reads `SO_PEERCRED` and requires the peer uid to equal the
    /// configured broker uid. The peer pid is logged for audit and never
    /// used for identity: pids are recyclable. Authorization happens
    /// before a single handshake byte is trusted — this is the fix for
    /// the missing-peer-authorization class of flaw, built in from day
    /// one rather than retrofitted. Failure is fail-closed: there is no
    /// path to the handshake with an unauthorized peer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PeerCredentials`] if the kernel reports nothing,
    /// [`Error::BrokerUnauthorized`] on uid mismatch.
    pub fn authorize(self) -> Result<Cell<PeerAuthorized>, Error> {
        let credentials = peer_credentials(self.connection())?;
        let expected = self.config.broker_uid;
        let actual = credentials.uid();
        if actual != expected {
            return Err(Error::BrokerUnauthorized { expected, actual });
        }
        tracing::info!(
            broker_uid = %actual,
            broker_pid = %credentials.pid(),
            "broker peer authorized"
        );
        Ok(Cell {
            config: self.config,
            broker: self.broker,
            state: PhantomData,
        })
    }
}

impl Cell<PeerAuthorized> {
    /// Performs the broker handshake over the authorized connection.
    ///
    /// Sends `HELLO` and requires a well-formed `WELCOME`. Any I/O
    /// failure, rejection, or protocol violation is fail-closed — the
    /// cell never runs on a handshake it cannot verify.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HandshakeIo`], [`Error::HandshakeRejected`], or
    /// [`Error::HandshakeProtocol`].
    pub fn handshake(self) -> Result<Cell<Ready>, Error> {
        handshake::perform_handshake(self.connection(), &self.config.cell_id)?;
        tracing::info!("broker handshake complete");
        Ok(Cell {
            config: self.config,
            broker: self.broker,
            state: PhantomData,
        })
    }
}

impl Cell<Ready> {
    /// Returns the open broker connection. Post-handshake traffic goes
    /// through the vault methods below; the raw handle remains for
    /// future protocol extensions.
    #[must_use]
    pub const fn broker(&self) -> &UnixStream {
        self.connection()
    }

    /// Stores `secret` under `name` in the broker-mediated vault,
    /// replacing any existing value.
    ///
    /// The broker checks its grant set for (`store`, `name`) before
    /// touching the vault; without a grant the op is denied.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSecretName`] or [`Error::SecretTooLarge`]
    /// before any I/O, [`Error::VaultDenied`] if the broker holds no
    /// grant, [`Error::VaultFailed`] on a broker-side backend failure,
    /// [`Error::VaultIo`] on transport failure, or
    /// [`Error::VaultProtocol`] on a malformed broker reply.
    pub fn vault_store(&self, name: &str, secret: &[u8]) -> Result<(), Error> {
        vault::store(self.connection(), name, secret)
    }

    /// Fetches the secret stored under `name` from the broker-mediated
    /// vault. Returns `Ok(None)` when no secret is stored under the name.
    ///
    /// # Errors
    ///
    /// Same as [`vault_store`](Self::vault_store).
    pub fn vault_fetch(&self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        vault::fetch(self.connection(), name)
    }

    /// Removes the secret stored under `name` from the broker-mediated
    /// vault. Idempotent: removing a missing secret succeeds.
    ///
    /// # Errors
    ///
    /// Same as [`vault_store`](Self::vault_store).
    pub fn vault_remove(&self, name: &str) -> Result<(), Error> {
        vault::remove(self.connection(), name)
    }

    /// Runs the skill with the exact id `skill` (`name@version`) on
    /// `input`, exercising `capability`, through the broker's skill
    /// session. The output is untrusted.
    ///
    /// # Errors
    ///
    /// See [`skill::invoke`].
    pub fn skill_invoke(
        &self,
        skill: &str,
        capability: &CapabilityName,
        input: &str,
    ) -> Result<String, Error> {
        skill::invoke(self.connection(), skill, capability, input)
    }

    /// Mints an egress surrogate for `destination`.
    ///
    /// The broker checks the peer's grant for the destination, then the
    /// egress allowlist. The surrogate is bound to (`peer_uid`,
    /// `destination`, `expiry`) and is valid for `ttl_secs` seconds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EgressDenied`] if the broker holds no grant or
    /// the destination is not on the allowlist,
    /// [`Error::EgressFailed`] on a broker-side failure,
    /// [`Error::EgressIo`] on transport failure, or
    /// [`Error::EgressProtocol`] on a malformed broker reply.
    pub fn egress_mint_surrogate(&self, destination: &str, ttl_secs: u64) -> Result<String, Error> {
        egress::mint_surrogate(self.connection(), destination, ttl_secs)
    }

    /// Performs an outbound HTTP request through the broker-mediated
    /// egress proxy, using a previously minted surrogate.
    ///
    /// The agent never touches the network and never holds a real
    /// credential: the broker validates the surrogate, checks the
    /// allowlist, swaps in the real credential, and forwards the
    /// request.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EgressDenied`] if the surrogate is invalid or
    /// the destination is not allowed, [`Error::EgressFailed`] on a
    /// broker-side failure, [`Error::EgressIo`] on transport failure, or
    /// [`Error::EgressProtocol`] on a malformed broker reply.
    pub fn egress_request(
        &self,
        surrogate_b64: &str,
        method: &str,
        destination: &str,
        path: &str,
        headers: Vec<(String, String)>,
        body: &[u8],
    ) -> Result<egress::EgressResponse, Error> {
        egress::egress_request(
            self.connection(),
            surrogate_b64,
            method,
            destination,
            path,
            headers,
            body,
        )
    }

    /// Run the agent tool-calling loop against `model`.
    ///
    /// Builds the registry from the broker-mediated built-ins
    /// (`vault_fetch`, `egress_request`, `skill_invoke`) and drives the loop to a
    /// final answer within `budget`. Every tool effect goes through
    /// the peer-authorized broker session; the loop adds no new
    /// trust surface. Recalled memory comes from the broker-backed
    /// provider (tagged items, user + project scopes); a recall
    /// failure is fail-closed on the recall path and never blocks
    /// the task.
    ///
    /// # Errors
    ///
    /// Returns [`hatchling_loop::Error`] variants per the loop's
    /// contract: budget exhaustion, unknown tools, and invalid
    /// arguments stop fail-closed; tool execution failures feed
    /// back to the model.
    pub fn run_loop(
        &self,
        kind: hatchling_loop::PromptKind,
        model: &dyn hatchling_loop::Model,
        request: String,
        budget: hatchling_loop::LoopBudget,
    ) -> Result<hatchling_loop::LoopResult, hatchling_loop::Error> {
        self.run_loop_redacted(
            kind,
            model,
            request,
            budget,
            &hatchling_model::Redactor::default(),
        )
    }

    /// [`run_loop`](Self::run_loop), registering every secret the run
    /// fetches with `redactor`. A provider built with the same
    /// redactor never sends one: the model, local or cloud, sees the
    /// fetch succeed, not the secret.
    ///
    /// # Errors
    ///
    /// As [`run_loop`](Self::run_loop).
    pub fn run_loop_redacted(
        &self,
        kind: hatchling_loop::PromptKind,
        model: &dyn hatchling_loop::Model,
        request: String,
        budget: hatchling_loop::LoopBudget,
        redactor: &hatchling_model::Redactor,
    ) -> Result<hatchling_loop::LoopResult, hatchling_loop::Error> {
        self.run_loop_delegating(kind, model, request, budget, redactor, None)
    }

    /// [`run_loop_redacted`](Self::run_loop_redacted), able to delegate
    ///: child agents' budgets come out of this run's own step
    /// ledger, `model_grant` (the run's model endpoint) goes to every
    /// child so it can think, and when the run ends, however it ends,
    /// every child it left running is killed.
    pub(crate) fn run_loop_delegating(
        &self,
        kind: hatchling_loop::PromptKind,
        model: &dyn hatchling_loop::Model,
        request: String,
        budget: hatchling_loop::LoopBudget,
        redactor: &hatchling_model::Redactor,
        model_grant: Option<hatchling_core::ScheduleGrant>,
    ) -> Result<hatchling_loop::LoopResult, hatchling_loop::Error> {
        let ledger = std::sync::Arc::new(hatchling_loop::StepLedger::new(budget.max_steps));
        let children = crate::subagent::Children::default();
        let mut registry = hatchling_loop::ToolRegistry::new();
        registry.register(Box::new(crate::tools::VaultFetchTool::new(
            self.connection(),
            redactor,
        )));
        registry.register(Box::new(crate::tools::EgressRequestTool::new(
            self.connection(),
        )));
        registry.register(Box::new(crate::tools::SkillInvokeTool::new(
            self.connection(),
        )));
        registry.register(Box::new(crate::tools::SpawnSubagentTool::new(
            self.connection(),
            std::sync::Arc::clone(&ledger),
            &children,
            model_grant,
        )));
        registry.register(Box::new(crate::tools::SubagentResultTool::new(
            self.connection(),
            &children,
        )));
        registry.register(Box::new(crate::tools::KillSubagentTool::new(
            self.connection(),
            &children,
        )));
        let result = hatchling_loop::Loop::new(&registry, kind)
            .with_budget(budget)
            .with_ledger(ledger)
            .with_memory_provider(Box::new(crate::memory_provider::BrokerMemoryProvider::new(
                self.connection(),
            )))
            .run(model, request);
        // The orphan rule: no child outlives the run that spawned it.
        let _ = children.reap(self.connection(), hatchling_core::subagent::ORPHANED);
        result
    }

    /// Runs a research run for `question` against `model`.
    ///
    /// Every source is fetched through the broker's egress proxy
    /// ([`crate::research::EgressFetcher`]), so the peer's grants and the
    /// egress allowlist decide what may be reached. `approver` is the
    /// user: it approves the plan and each amendment.
    ///
    /// # Errors
    ///
    /// Returns why the run did not finish
    /// ([`hatchling_research::ResearchError`]).
    pub fn run_research(
        &self,
        model: &dyn hatchling_loop::Model,
        approver: &dyn hatchling_research::Approver,
        bounds: hatchling_research::Bounds,
        question: &str,
    ) -> Result<hatchling_research::Outcome, hatchling_research::ResearchError> {
        let fetcher = crate::research::EgressFetcher::new(self.connection());
        hatchling_research::Research {
            model,
            fetcher: &fetcher,
            approver,
            clock: &hatchling_research::SystemClock,
            bounds,
        }
        .run(question)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CellId, LogLevel, PeerUid, SocketPath};
    use std::io::ErrorKind;
    use std::os::unix::net::UnixListener;
    use std::path::Path;

    fn config_with_socket(socket: &str) -> Config {
        Config {
            cell_id: "cell-01".parse::<CellId>().unwrap(),
            broker: BrokerAddress::Socket(socket.parse::<SocketPath>().unwrap()),
            // Tests that exercise authorization override this with the
            // real uid; the placeholder only matters where authorization
            // is never reached.
            broker_uid: PeerUid::from_kernel(0),
            log_level: LogLevel::Info,
            model_endpoint: None,
            model_name: None,
            model_max_steps: 10,
            model_context_window: None,
            model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
            run_subagent: false,
        }
    }

    /// Socket path unique to this test and process, so tests can run in
    /// parallel without colliding.
    fn test_socket_path(name: &str) -> crate::test_socket::TestSocket {
        crate::test_socket::TestSocket::new(format!(
            "/tmp/hatchling-test-{}-{name}.sock",
            std::process::id()
        ))
    }

    /// Binds a listener on a fresh test socket path, removing any stale
    /// file left by a previous crashed run.
    fn bind_test_listener(name: &str) -> (UnixListener, crate::test_socket::TestSocket) {
        let path = test_socket_path(name);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        (listener, path)
    }

    #[test]
    fn check_preconditions_succeeds_when_parent_dir_exists() {
        let cell = Cell::new(config_with_socket("/tmp/hatchling-test-broker.sock"));
        let checked = cell.check_preconditions().unwrap();
        assert_eq!(checked.config().cell_id.as_str(), "cell-01");
    }

    #[test]
    fn a_cells_own_connection_is_adopted_and_authorized_like_a_socket() {
        use std::os::fd::IntoRawFd as _;
        let (_broker, agent) = UnixStream::pair().unwrap();
        let mut config = config_with_socket("/no/such/dir-xyz/broker.sock");
        config.broker = BrokerAddress::Connection(agent.into_raw_fd());
        // The broker made the pair here, so the kernel names this process.
        config.broker_uid = PeerUid::from_kernel(rustix::process::getuid().as_raw());
        let authorized = Cell::new(config)
            .check_preconditions()
            .and_then(Cell::connect_broker)
            .and_then(Cell::authorize);
        assert!(authorized.is_ok(), "{authorized:?}");
        // A descriptor that isn't a connection is refused, closed.
        let mut config = config_with_socket("/no/such/dir-xyz/broker.sock");
        config.broker = BrokerAddress::Connection(i32::MAX - 1);
        let err = Cell::new(config)
            .check_preconditions()
            .and_then(Cell::connect_broker)
            .unwrap_err();
        assert!(
            matches!(&err, Error::BrokerConnection { fd, .. } if *fd == i32::MAX - 1),
            "{err:?}"
        );
    }

    #[test]
    fn check_preconditions_fails_closed_when_parent_dir_missing() {
        let cell = Cell::new(config_with_socket("/no/such/dir-xyz/broker.sock"));
        let err = cell.check_preconditions().unwrap_err();
        assert!(matches!(err, Error::PreconditionFailed(_)));
    }

    #[test]
    fn connect_broker_fails_closed_when_socket_missing() {
        let path = test_socket_path("missing");
        let _ = std::fs::remove_file(&path);
        let cell = Cell::new(config_with_socket(&path))
            .check_preconditions()
            .unwrap();
        let err = cell.connect_broker().unwrap_err();
        assert!(matches!(err, Error::BrokerConnect { .. }));
        if let Error::BrokerConnect { socket, source } = err {
            assert_eq!(socket.as_path(), Path::new(&*path));
            assert_eq!(source.kind(), ErrorKind::NotFound);
        }
    }

    #[test]
    fn connect_broker_fails_closed_when_connection_refused() {
        // A regular file where the socket should be: the path exists but
        // nothing is listening on it.
        let path = test_socket_path("refused");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"not a socket").unwrap();
        let cell = Cell::new(config_with_socket(&path))
            .check_preconditions()
            .unwrap();
        let err = cell.connect_broker().unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(matches!(err, Error::BrokerConnect { .. }));
        if let Error::BrokerConnect { socket, source } = err {
            assert_eq!(socket.as_path(), Path::new(&*path));
            assert_eq!(source.kind(), ErrorKind::ConnectionRefused);
        }
    }

    #[test]
    fn connect_broker_establishes_a_live_connection() {
        let (listener, path) = bind_test_listener("live");
        let _cell = Cell::new(config_with_socket(&path))
            .check_preconditions()
            .unwrap()
            .connect_broker()
            .unwrap();

        // The listener observed the connection arrive; the byte-level
        // liveness is proven by the handshake tests, which run the full
        // HELLO/WELCOME exchange over a connection made the same way.
        let (_server, _) = listener.accept().unwrap();
    }

    #[test]
    fn authorize_succeeds_when_kernel_uid_matches_config() {
        let (listener, path) = bind_test_listener("authz-ok");
        let uid = rustix::process::getuid().as_raw();
        let mut config = config_with_socket(&path);
        config.broker_uid = PeerUid::from_kernel(uid);
        let cell = Cell::new(config)
            .check_preconditions()
            .unwrap()
            .connect_broker()
            .unwrap();
        let (_server, _) = listener.accept().unwrap();

        let authorized = cell.authorize().unwrap();
        assert_eq!(authorized.config().cell_id.as_str(), "cell-01");
    }

    #[test]
    fn authorize_fails_closed_when_kernel_uid_mismatches() {
        let (listener, path) = bind_test_listener("authz-deny");
        let real_uid = rustix::process::getuid().as_raw();
        let mut config = config_with_socket(&path);
        // Deliberately wrong: the kernel will report our real uid.
        config.broker_uid = PeerUid::from_kernel(real_uid.wrapping_add(1));
        let cell = Cell::new(config)
            .check_preconditions()
            .unwrap()
            .connect_broker()
            .unwrap();
        let (_server, _) = listener.accept().unwrap();

        let err = cell.authorize().unwrap_err();
        assert!(matches!(err, Error::BrokerUnauthorized { .. }));
        if let Error::BrokerUnauthorized { expected, actual } = err {
            assert_eq!(expected, PeerUid::from_kernel(real_uid.wrapping_add(1)));
            assert_eq!(actual, PeerUid::from_kernel(real_uid));
        }
    }
}
