//! The local API: Hatchling as infrastructure, for external
//! agents, scripts and tools on this machine.
//!
//! The API speaks the frontend protocol ([`hatchling_core::ui`]) on a
//! Unix socket at `HATCHLING_API_SOCKET`, one frame per request and per
//! response, in the session's length-prefixed framing:
//!
//! 0. A peer that is not the socket's owner, by the kernel's peer
//!    credentials, is closed before a byte is read.
//! 1. The caller's first frame is `hello`. A version mismatch is answered
//!    `incompatible`, with the sentence for the person; a caller the
//!    deployment's [`Provider`] does not admit is closed without a word.
//!    Otherwise `welcome`.
//! 2. `subscribe` replays the retained events after the caller's mark,
//!    then follows new ones (see [`crate::api_stream`]).
//! 3. Each `action` gets one `action_result`, with the same request id.
//!
//! This module is the adapter, and only transports. It holds no rule
//! about who may do what: the provider names the caller,
//! [`crate::api_auth::admit`] narrows its grants to the owner's, and
//! [`crate::api_ops`] decides each action and records it. The arch check
//! holds this file to that, as it does the frontend host.
//!
//! The socket is owner-only (`0600`), outside the cell's sight, and never
//! started for the agent's own uid. There is no network listener (see
//! [`crate::api_auth`]).
//!
//! The UI socket (`HATCHLING_UI_SOCKET`), when it's set, is served the
//! same way beside it, with one difference: its callers are named by a
//! [`TokenProvider`], which also wants the UI token. Both share one
//! stream and one activity log.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hatchling_channel::LocalHub;
use hatchling_core::PeerUid;
use hatchling_core::framing::{read_frame_or_eof, write_frame};
use hatchling_core::ui::{
    ActionRefusal, ApiOutcome, ApiRequest, UiActionResult, UiRequest, UiResponse, negotiate,
};
use hatchling_vault::MasterKey;

use crate::api_auth::{
    ApiConfig, Caller, PeerProvider, Provider, TokenProvider, admit, mint_ui_token, ui_token_path,
};
use crate::api_ops::{Audit, Ops, Replies};
use crate::api_stream::{Bus, FOLLOW_POLL, Follower};
use crate::connections::ConnectionSlots;
use crate::serve::session_error;
use crate::skills::Skills;
use crate::{Config, Error, bind_socket};

/// The API socket's mode bits: owner only.
pub const API_SOCKET_MODE: u32 = 0o600;

/// Most API connections served at once.
pub const API_MAX_CONNECTIONS: usize = 8;

/// How long a new connection may take to say `hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a write may block on a slow caller before the connection
/// ends.
pub const API_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the stream waits for a new event before it looks again
/// whether its connection has ended.
const STREAM_WAIT: Duration = Duration::from_millis(500);

/// A bound socket of the API, and the provider that names its callers.
struct Door {
    listener: UnixListener,
    provider: Box<dyn Provider>,
}

/// The bound API: its socket and, when it has one, the UI's, each with
/// the provider that names its callers; its stream; and the activity log
/// its calls are recorded in.
pub struct Api {
    local: Door,
    ui: Option<Door>,
    /// The only uid a byte is read from: the sockets' owner, the broker.
    owner: PeerUid,
    bus: Arc<Bus>,
    log: PathBuf,
    slots: ConnectionSlots,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api")
            .field("log", &self.log)
            .finish_non_exhaustive()
    }
}

/// The broker's API, bound, when one is configured.
///
/// The sockets' owner (the broker's own uid) is its one local caller,
/// with the configured grants; on the UI socket, it must also present
/// the UI token, minted here.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] when the API would serve the agent's
/// own uid, or for a UI socket without an API socket; the bind errors of
/// [`bind_socket`]; or [`mint_ui_token`]'s.
pub fn open_api(config: &Config, api: &ApiConfig) -> Result<Option<Api>, Error> {
    let Some(socket) = &api.socket else {
        return match api.ui_socket {
            Some(_) => Err(Error::InvalidConfig {
                key: "HATCHLING_UI_SOCKET",
                reason: String::from(
                    "the UI socket is served beside the API socket: set HATCHLING_API_SOCKET too",
                ),
            }),
            None => Ok(None),
        };
    };
    // The broker binds the sockets, so they're its own: its uid, never
    // read back from a path.
    let owner = PeerUid::from_kernel(hatchling_sys::euid());
    if owner == config.agent_uid {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_API_SOCKET",
            reason: format!(
                "the API serves uid {owner}, the broker's own, which is also the agent's: \
                 the cell could drive the API"
            ),
        });
    }
    let listener = bind_socket(socket, API_SOCKET_MODE)?;
    let peer = PeerProvider::new(owner, api.grants.clone());
    let mut opened = Api::new(
        listener,
        Box::new(peer.clone()),
        config.scheduler.activity_log(),
    );
    if let Some(ui_socket) = &api.ui_socket {
        let listener = bind_socket(ui_socket, API_SOCKET_MODE)?;
        let token = mint_ui_token(&ui_token_path(ui_socket))?;
        opened = opened.with_ui(listener, Box::new(TokenProvider::new(peer, token)));
    }
    Ok(Some(opened))
}

impl Api {
    /// The API on `listener`, naming callers with `provider` and
    /// recording calls in `log`.
    #[must_use]
    pub fn new(listener: UnixListener, provider: Box<dyn Provider>, log: PathBuf) -> Self {
        Self {
            local: Door { listener, provider },
            ui: None,
            owner: PeerUid::from_kernel(hatchling_sys::euid()),
            bus: Arc::new(Bus::starting_now()),
            log,
            slots: ConnectionSlots::new(API_MAX_CONNECTIONS),
        }
    }

    /// This API, also serving the UI on `listener`, its callers named by
    /// `provider`.
    #[must_use]
    pub fn with_ui(mut self, listener: UnixListener, provider: Box<dyn Provider>) -> Self {
        self.ui = Some(Door { listener, provider });
        self
    }

    /// Whether it serves the UI too.
    #[must_use]
    pub const fn serves_ui(&self) -> bool {
        self.ui.is_some()
    }

    /// The API's stream.
    #[must_use]
    pub const fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }

    /// Follows the activity log onto the stream, and serves callers,
    /// each on its own thread, forever. `hub` is the local channel's,
    /// when the broker has one.
    pub fn serve(
        &self,
        config: &Config,
        key: &MasterKey,
        skills: &Skills,
        hub: Option<&LocalHub>,
    ) -> ! {
        let ops = Ops::new(config, key, skills, hub, Arc::clone(&self.bus));
        std::thread::scope(|scope| {
            scope.spawn(|| Follower::new(self.log.clone()).follow(&self.bus, FOLLOW_POLL));
            for door in std::iter::once(&self.local).chain(self.ui.as_ref()) {
                let ops = &ops;
                scope.spawn(move || {
                    loop {
                        match door.listener.accept() {
                            Ok((stream, _)) => match self.slots.try_acquire() {
                                Ok(slot) => {
                                    scope.spawn(move || {
                                        self.serve_door(&stream, door.provider.as_ref(), ops);
                                        drop(slot);
                                    });
                                }
                                Err(full) => {
                                    if full.first_of_episode {
                                        tracing::warn!(
                                            max = API_MAX_CONNECTIONS,
                                            "API connection cap reached: closing new connections until one ends"
                                        );
                                    }
                                }
                            },
                            Err(source) => tracing::error!(%source, "API accept failed"),
                        }
                    }
                });
            }
            loop {
                std::thread::park();
            }
        })
    }

    /// Serves one caller of the API socket on `stream` until it closes,
    /// or breaks the protocol.
    pub fn serve_client(&self, stream: &UnixStream, ops: &Ops<'_>) {
        self.serve_door(stream, self.local.provider.as_ref(), ops);
    }

    /// Serves one caller on `stream`, named by `provider`, until it
    /// closes, or breaks the protocol.
    fn serve_door(&self, stream: &UnixStream, provider: &dyn Provider, ops: &Ops<'_>) {
        let connection =
            crate::approval::random_hex("api-").unwrap_or_else(|_| String::from("api-unnamed"));
        let mut audit = Audit::new(self.log.clone(), connection);
        if let Err(error) = self.session(stream, provider, ops, &mut audit) {
            tracing::info!(%error, "API connection ended");
        }
    }

    /// The handshake, then the caller's requests.
    fn session(
        &self,
        stream: &UnixStream,
        provider: &dyn Provider,
        ops: &Ops<'_>,
        audit: &mut Audit,
    ) -> Result<(), Error> {
        // The kernel's word first: a peer that is not the socket's
        // owner gets no byte read, let alone answered. The provider still
        // names the caller after `hello`.
        if !crate::api_auth::from_owner(stream, self.owner) {
            audit.record(None, ApiRequest::Hello, ApiOutcome::Refused);
            return Ok(());
        }
        let io = |source| Error::SessionIo { source };
        stream.set_read_timeout(Some(HELLO_TIMEOUT)).map_err(io)?;
        stream
            .set_write_timeout(Some(API_WRITE_TIMEOUT))
            .map_err(io)?;
        let writer = Mutex::new(stream.try_clone().map_err(io)?);
        let Some(first) = read_frame_or_eof(stream).map_err(session_error)? else {
            return Ok(());
        };
        let request = decode(&first)?;
        let UiRequest::Hello {
            schema_version,
            token,
            ..
        } = &request
        else {
            audit.record(None, ApiRequest::of(&request), ApiOutcome::Refused);
            if let UiRequest::Action { request_id, .. } = &request {
                let refused = UiActionResult::Refused {
                    reason: ActionRefusal::NotConnected,
                };
                send(
                    &writer,
                    &UiResponse::ActionResult {
                        request_id: request_id.clone(),
                        result: refused,
                    },
                )?;
            }
            return Ok(());
        };
        let answer = negotiate(*schema_version);
        if matches!(answer, UiResponse::Incompatible { .. }) {
            audit.record(None, ApiRequest::Hello, ApiOutcome::Refused);
            return send(&writer, &answer);
        }
        let Some(caller) = admit(provider, &ops.owner_grants(), stream, token.as_ref()) else {
            audit.record(None, ApiRequest::Hello, ApiOutcome::Refused);
            return Ok(());
        };
        send(&writer, &answer)?;
        audit.record(Some(&caller), ApiRequest::Hello, ApiOutcome::Served);
        stream.set_read_timeout(None).map_err(io)?;
        let ended = AtomicBool::new(false);
        let (bus, out, done) = (self.bus.as_ref(), &writer, &ended);
        std::thread::scope(|scope| {
            let served = requests(stream, ops, audit, &caller, out, |after| {
                scope.spawn(move || follow(bus, out, done, after));
            });
            ended.store(true, Ordering::Relaxed);
            served
        })
    }
}

/// The caller's requests after the handshake; `subscribe` starts the
/// stream, once.
fn requests(
    stream: &UnixStream,
    ops: &Ops<'_>,
    audit: &mut Audit,
    caller: &Caller,
    writer: &Mutex<UnixStream>,
    mut subscribe: impl FnMut(Option<u64>),
) -> Result<(), Error> {
    let mut replies = Replies::default();
    let mut subscribed = false;
    while let Some(frame) = read_frame_or_eof(stream).map_err(session_error)? {
        match decode(&frame)? {
            UiRequest::Hello { .. } => {
                audit.record(Some(caller), ApiRequest::Hello, ApiOutcome::Refused);
                return Ok(());
            }
            UiRequest::Subscribe { after_seq } => {
                let outcome = if subscribed {
                    ApiOutcome::Refused
                } else {
                    ApiOutcome::Served
                };
                audit.record(Some(caller), ApiRequest::Subscribe, outcome);
                if !subscribed {
                    subscribed = true;
                    subscribe(after_seq);
                }
            }
            UiRequest::Action { request_id, action } => {
                let (result, outcome) = ops.handle(caller, &action, &mut replies);
                audit.record(Some(caller), ApiRequest::of_action(&action), outcome);
                send(writer, &UiResponse::ActionResult { request_id, result })?;
            }
        }
    }
    Ok(())
}

/// Sends the stream's events after `after`, and each new one as it
/// arrives, until the connection ends or a write fails.
fn follow(bus: &Bus, writer: &Mutex<UnixStream>, ended: &AtomicBool, after: Option<u64>) {
    let mut mark = after;
    while !ended.load(Ordering::Relaxed) {
        for event in bus.wait_after(mark, STREAM_WAIT) {
            mark = Some(event.seq);
            if send(writer, &UiResponse::Event(event)).is_err() {
                return;
            }
        }
    }
}

/// One request frame.
fn decode(frame: &[u8]) -> Result<UiRequest, Error> {
    serde_json::from_slice(frame).map_err(|error| Error::SessionProtocol {
        reason: format!("API frame is not a request: {error}"),
    })
}

/// Writes one response frame.
fn send(writer: &Mutex<UnixStream>, response: &UiResponse) -> Result<(), Error> {
    let payload = serde_json::to_vec(response).map_err(|error| Error::SessionProtocol {
        reason: format!("API response encode failed: {error}"),
    })?;
    let stream = writer.lock().unwrap_or_else(PoisonError::into_inner);
    write_frame(&stream, &payload).map_err(session_error)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Path;

    use hatchling_core::SocketPath;
    use hatchling_core::framing::read_frame;
    use hatchling_core::messages::{ScheduleGrant, ScheduleOp, ScheduleSpec};
    use hatchling_core::subagent::{ActivityEvent, SubagentEvent};
    use hatchling_core::ui::{
        ApiToken, ChatAuthor, ChatMessage, UI_SCHEMA_VERSION, UiAction, UiEventBody,
    };

    use super::*;
    use crate::api_auth::tests::{Scratch, owner_config, real_uid, scratch};
    use crate::config::{GoalsAction, SkillConfig};
    use crate::test_socket::TestSocket;

    fn goals() -> ScheduleGrant {
        ScheduleGrant::Goals {
            action: String::from("query-goals"),
        }
    }

    /// A socket path under `/tmp`, unique per test and process.
    fn socket_path(name: &str) -> SocketPath {
        format!("/tmp/hlapi-{}-{name}.sock", std::process::id())
            .parse()
            .unwrap()
    }

    /// The owner's config, scheduling (and so the activity log) under `dir`.
    fn config_in(dir: &Scratch) -> Config {
        let mut config = owner_config(Some(&dir.0.join("schedules").to_string_lossy()));
        config.goals.grants = vec![GoalsAction::QueryGoals];
        config
    }

    fn api_with(provider: Box<dyn Provider>, log: PathBuf, name: &str) -> (Api, TestSocket) {
        let socket = socket_path(name);
        let _ = std::fs::remove_file(socket.as_path());
        let guard = TestSocket::new(socket.as_path().to_string_lossy());
        let listener = UnixListener::bind(socket.as_path()).unwrap();
        (Api::new(listener, provider, log), guard)
    }

    fn write(stream: &UnixStream, request: &UiRequest) {
        write_frame(stream, &serde_json::to_vec(request).unwrap()).unwrap();
    }

    fn read(stream: &UnixStream) -> UiResponse {
        serde_json::from_slice(&read_frame(stream).unwrap()).unwrap()
    }

    /// Whether the other end closed with nothing more to say.
    fn closed(stream: &UnixStream) -> bool {
        matches!(read_frame_or_eof(stream), Ok(None))
    }

    /// Whether the broker answered nothing and hung up: the end of the
    /// stream, or a reset when it closed with the caller's bytes unread.
    fn hung_up_unanswered(stream: &UnixStream) -> bool {
        match read_frame_or_eof(stream) {
            Ok(None) => true,
            Err(hatchling_core::Error::FrameIo { source }) => {
                source.kind() == std::io::ErrorKind::ConnectionReset
            }
            Ok(Some(_)) | Err(_) => false,
        }
    }

    fn hello(token: Option<&str>) -> UiRequest {
        UiRequest::Hello {
            schema_version: UI_SCHEMA_VERSION,
            client: String::from("test-script"),
            token: token.map(|text| ApiToken::new(text.to_owned())),
        }
    }

    fn action(id: &str, action: UiAction) -> UiRequest {
        UiRequest::Action {
            request_id: id.to_owned(),
            action,
        }
    }

    fn schedule(grants: Vec<ScheduleGrant>) -> UiAction {
        UiAction::ManageSchedule {
            op: ScheduleOp::Create {
                when: ScheduleSpec::Cron {
                    expr: String::from("0 8 * * *"),
                    tz: String::new(),
                },
                task: String::from("Summarize."),
                grants,
                channel: String::from("local"),
            },
        }
    }

    /// The API calls recorded in `log`: who, what kind, what outcome.
    fn recorded(log: &Path) -> Vec<(String, ApiRequest, ApiOutcome)> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter_map(
                |line| match serde_json::from_str::<ActivityEvent>(line).ok()?.kind {
                    SubagentEvent::ApiCall {
                        caller,
                        request,
                        outcome,
                        ..
                    } => Some((caller, request, outcome)),
                    SubagentEvent::Spawned { .. }
                    | SubagentEvent::Progress { .. }
                    | SubagentEvent::Finished { .. }
                    | SubagentEvent::Killed { .. }
                    | SubagentEvent::Failed { .. }
                    | SubagentEvent::HarnessRun { .. } => None,
                },
            )
            .collect()
    }

    /// Runs `script` as a client of `api`, served on the other end of a
    /// socket pair, and returns once both ends are done.
    fn session(api: &Api, ops: &Ops<'_>, script: impl FnOnce(&UnixStream) + Send) {
        let (client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(move || {
                api.serve_client(&server, ops);
                drop(server);
            });
            script(&client);
            drop(client);
            worker.join().unwrap();
        });
    }

    /// Tells the stream to stop when dropped, so a failing assertion
    /// ends the test instead of leaving the stream running.
    /// How long a stream test waits for something that should happen at
    /// once: only a broken stream gets near it, so it never paces a pass.
    const PATIENCE: Duration = Duration::from_secs(15);

    struct StopOnDrop<'a>(&'a AtomicBool);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    /// A provider that names callers by their token, as a hosted
    /// deployment's would.
    struct Tokens(Vec<(String, Caller)>);

    impl Provider for Tokens {
        fn identify(&self, _stream: &UnixStream, token: Option<&ApiToken>) -> Option<Caller> {
            let token = token?;
            self.0
                .iter()
                .find(|(known, _)| known == token.expose())
                .map(|(_, caller)| caller.clone())
        }
    }

    #[test]
    fn the_api_is_off_without_a_socket_and_never_serves_the_agents_uid() {
        let dir = scratch("open");
        let config = config_in(&dir);
        assert!(open_api(&config, &ApiConfig::default()).unwrap().is_none());
        let socket = socket_path("open");
        let _ = std::fs::remove_file(socket.as_path());
        let _guard = TestSocket::new(socket.as_path().to_string_lossy());
        let settings = ApiConfig {
            socket: Some(socket.clone()),
            ui_socket: None,
            grants: BTreeSet::new(),
        };
        let api = open_api(&config, &settings).unwrap().unwrap();
        let shown = format!("{api:?}");
        assert!(shown.contains("schedules.activity"));
        let meta = std::fs::metadata(socket.as_path()).unwrap();
        assert_eq!(meta.mode() & 0o777, API_SOCKET_MODE);
        assert_eq!(API_SOCKET_MODE, 0o600);
        drop(api);
        let mut agents = config;
        agents.agent_uid = real_uid();
        // Refused before anything is bound: no socket for the agent's uid,
        // not even for a moment.
        std::fs::remove_file(socket.as_path()).unwrap();
        let refused = open_api(&agents, &settings);
        assert!(
            matches!(
                refused,
                Err(Error::InvalidConfig {
                    key: "HATCHLING_API_SOCKET",
                    ..
                })
            ),
            "{refused:?}"
        );
        assert!(!socket.as_path().exists(), "no socket was bound");
    }

    /// a peer that is not the socket's owner is closed before a
    /// byte is read: even an action before hello gets no answer.
    #[test]
    fn a_peer_that_is_not_the_owner_gets_no_byte_read() {
        let dir = scratch("foreign");
        let config = config_in(&dir);
        let log = config.scheduler.activity_log();
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let (mut foreign, _socket) = api_with(
            Box::new(PeerProvider::new(real_uid(), BTreeSet::new())),
            log.clone(),
            "foreign",
        );
        assert_eq!(
            foreign.owner,
            real_uid(),
            "the API reads only from its own uid"
        );
        let ops = Ops::new(&config, &key, &skills, None, Arc::clone(foreign.bus()));
        foreign.owner = PeerUid::from_kernel(real_uid().as_u32().wrapping_add(1));
        session(&foreign, &ops, |client| {
            // The broker may hang up before this is written: a failed
            // write is that, not a fault.
            let early = serde_json::to_vec(&action("early", schedule(Vec::new()))).unwrap();
            let _ = write_frame(client, &early);
            assert!(hung_up_unanswered(client), "no reply before admission");
        });
        assert_eq!(
            recorded(&log),
            [(
                String::from("unidentified"),
                ApiRequest::Hello,
                ApiOutcome::Refused
            )]
        );
    }

    #[test]
    fn the_handshake_comes_first_and_a_caller_not_admitted_hears_nothing() {
        let dir = scratch("handshake");
        let config = config_in(&dir);
        let log = config.scheduler.activity_log();
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let (api, _socket) = api_with(
            Box::new(PeerProvider::new(real_uid(), BTreeSet::new())),
            log.clone(),
            "handshake",
        );
        let ops = Ops::new(&config, &key, &skills, None, Arc::clone(api.bus()));
        // An action before hello is refused as not connected, then closed.
        session(&api, &ops, |client| {
            write(client, &action("early", schedule(Vec::new())));
            let refused = read(client);
            assert_eq!(
                refused,
                UiResponse::ActionResult {
                    request_id: String::from("early"),
                    result: UiActionResult::Refused {
                        reason: ActionRefusal::NotConnected
                    }
                }
            );
            assert!(closed(client));
        });
        // Subscribing before hello: closed without a word.
        session(&api, &ops, |client| {
            write(client, &UiRequest::Subscribe { after_seq: None });
            assert!(closed(client));
        });
        // Another version: the sentence for the person, then closed.
        session(&api, &ops, |client| {
            write(
                client,
                &UiRequest::Hello {
                    schema_version: UI_SCHEMA_VERSION + 1,
                    client: String::from("newer"),
                    token: None,
                },
            );
            assert!(
                matches!(read(client), UiResponse::Incompatible { message, .. } if message.contains("restart the broker"))
            );
            assert!(closed(client));
        });
        // Bytes that are no request: closed.
        session(&api, &ops, |client| {
            write_frame(client, b"not json").unwrap();
            assert!(closed(client));
        });
        // Leaving before hello is no error.
        session(&api, &ops, |_client| {});
        assert_eq!(
            recorded(&log),
            [
                (
                    String::from("unidentified"),
                    ApiRequest::ManageSchedule,
                    ApiOutcome::Refused
                ),
                (
                    String::from("unidentified"),
                    ApiRequest::Subscribe,
                    ApiOutcome::Refused
                ),
                (
                    String::from("unidentified"),
                    ApiRequest::Hello,
                    ApiOutcome::Refused
                ),
            ]
        );
        // A caller the provider does not admit is closed without a byte.
        let (stranger, _other) = api_with(
            Box::new(PeerProvider::new(
                PeerUid::from_kernel(real_uid().as_u32().wrapping_add(1)),
                BTreeSet::new(),
            )),
            log.clone(),
            "stranger",
        );
        session(&stranger, &ops, |client| {
            write(client, &hello(None));
            assert!(closed(client));
        });
        assert_eq!(
            recorded(&log).last(),
            Some(&(
                String::from("unidentified"),
                ApiRequest::Hello,
                ApiOutcome::Refused
            ))
        );
    }

    #[test]
    fn a_session_streams_activity_answers_actions_and_records_every_call() {
        let dir = scratch("session");
        let config = config_in(&dir);
        let log = config.scheduler.activity_log();
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let (api, _socket) = api_with(
            Box::new(PeerProvider::new(real_uid(), BTreeSet::from([goals()]))),
            log.clone(),
            "session",
        );
        let ops = Ops::new(&config, &key, &skills, None, Arc::clone(api.bus()));
        let chat = |text: &str| {
            UiEventBody::ChatMessage(ChatMessage {
                chat_id: String::from("local"),
                message_id: String::from("m"),
                author: ChatAuthor::Agent,
                text: text.to_owned(),
            })
        };
        let first = api.bus().push(String::from("t1"), chat("one"));
        let second = api.bus().push(String::from("t2"), chat("two"));
        session(&api, &ops, |client| {
            write(client, &hello(None));
            assert!(matches!(
                read(client),
                UiResponse::Welcome {
                    schema_version: UI_SCHEMA_VERSION,
                    ..
                }
            ));
            // Resume after the first event: the second is replayed.
            write(
                client,
                &UiRequest::Subscribe {
                    after_seq: Some(first),
                },
            );
            assert!(matches!(read(client), UiResponse::Event(event) if event.seq == second));
            // A new event follows.
            let third = api.bus().push(String::from("t3"), chat("three"));
            assert!(matches!(read(client), UiResponse::Event(event) if event.seq == third));
            // An action within the caller's grants is passed on; one
            // beyond them is refused.
            write(
                client,
                &action(
                    "over",
                    schedule(vec![
                        goals(),
                        ScheduleGrant::ChannelSend {
                            channel: String::from("local"),
                        },
                    ]),
                ),
            );
            let refused = loop {
                if let UiResponse::ActionResult { request_id, result } = read(client) {
                    break (request_id, result);
                }
            };
            assert_eq!(
                refused,
                (
                    String::from("over"),
                    UiActionResult::Refused {
                        reason: ActionRefusal::NotGranted
                    }
                )
            );
            // Subscribing twice changes nothing; a second hello ends it.
            write(client, &UiRequest::Subscribe { after_seq: None });
            write(client, &hello(None));
            let ended = (0..64).find_map(|_| match read_frame_or_eof(client) {
                Ok(None) => Some(true),
                Ok(Some(_)) => None,
                Err(_) => Some(false),
            });
            assert_eq!(ended, Some(true), "a second hello ends the connection");
        });
        let label = format!("uid {}", real_uid());
        assert_eq!(
            recorded(&log),
            [
                (label.clone(), ApiRequest::Hello, ApiOutcome::Served),
                (label.clone(), ApiRequest::Subscribe, ApiOutcome::Served),
                (
                    label.clone(),
                    ApiRequest::ManageSchedule,
                    ApiOutcome::Refused
                ),
                (label.clone(), ApiRequest::Subscribe, ApiOutcome::Refused),
                (label, ApiRequest::Hello, ApiOutcome::Refused),
            ]
        );
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("goals:query-goals"),
            "each record names the caller's grants"
        );
    }

    #[test]
    fn a_token_provider_swaps_in_without_changing_what_the_api_does() {
        let dir = scratch("swap");
        let config = config_in(&dir);
        let log = config.scheduler.activity_log();
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let grants = BTreeSet::from([goals()]);
        let script = |client: &UnixStream| -> Vec<UiResponse> {
            write(client, &hello(Some("team-token")));
            let mut answers = vec![read(client)];
            for (id, request) in [
                ("narrow", schedule(vec![goals()])),
                (
                    "send",
                    UiAction::SendMessage {
                        chat_id: String::from("local"),
                        text: String::from("hi"),
                    },
                ),
            ] {
                write(client, &action(id, request));
                answers.push(read(client));
            }
            answers
        };
        let mut seen = Vec::new();
        let providers: [(&str, Box<dyn Provider>); 2] = [
            (
                "peer",
                Box::new(PeerProvider::new(real_uid(), grants.clone())),
            ),
            (
                "token",
                Box::new(Tokens(vec![(
                    String::from("team-token"),
                    Caller::new(String::from("team"), grants),
                )])),
            ),
        ];
        for (name, provider) in providers {
            let (api, _socket) = api_with(provider, log.clone(), name);
            let ops = Ops::new(&config, &key, &skills, None, Arc::clone(api.bus()));
            session(&api, &ops, |client| seen.push(script(client)));
        }
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen.first(),
            seen.last(),
            "the same answers, whichever provider names the caller"
        );
        let callers: BTreeSet<String> = recorded(&log)
            .into_iter()
            .map(|(caller, _, _)| caller)
            .collect();
        assert!(
            callers.contains("team"),
            "the token provider's caller is the one recorded"
        );
        // Without the token it knows, the token provider admits no one.
        let (api, _socket) = api_with(Box::new(Tokens(Vec::new())), log, "tokenless");
        let ops = Ops::new(&config, &key, &skills, None, Arc::clone(api.bus()));
        session(&api, &ops, |client| {
            write(client, &hello(Some("stolen")));
            assert!(closed(client));
        });
    }

    #[test]
    fn each_door_names_its_callers_with_its_own_provider() {
        let dir = scratch("doors");
        let config = config_in(&dir);
        let log = config.scheduler.activity_log();
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let grants = BTreeSet::new();
        let token = ApiToken::new(String::from("door-token"));
        let peer = PeerProvider::new(real_uid(), grants);
        let (api, _guard) = api_with(Box::new(peer.clone()), log, "doors");
        assert!(!api.serves_ui(), "no UI until one is given");
        let (_ui_client, ui_server) = UnixStream::pair().unwrap();
        let ui_listener = UnixListener::from(std::os::fd::OwnedFd::from(ui_server));
        let api = api.with_ui(ui_listener, Box::new(TokenProvider::new(peer, token)));
        assert!(api.serves_ui());
        let ops = Ops::new(&config, &key, &skills, None, Arc::clone(api.bus()));
        // The UI's provider on a door: the token is wanted, so a hello
        // without it is closed, and one with it is welcomed.
        let ui = api.ui.as_ref().unwrap().provider.as_ref();
        for (token, welcomed) in [(None, false), (Some("door-token"), true)] {
            let (client, server) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let (api, ops) = (&api, &ops);
            std::thread::scope(|scope| {
                // The server's end goes with the session, as it does when
                // the API serves it: a refused caller then sees the close.
                let worker = scope.spawn(move || {
                    api.serve_door(&server, ui, ops);
                    drop(server);
                });
                write(&client, &hello(token));
                if welcomed {
                    assert!(matches!(read(&client), UiResponse::Welcome { .. }));
                } else {
                    assert!(closed(&client));
                }
                drop(client);
                worker.join().unwrap();
            });
        }
    }

    #[test]
    fn the_ui_socket_is_served_beside_the_api_and_wants_its_token() {
        let dir = scratch("ui-serve");
        let config: &'static Config = Box::leak(Box::new(config_in(&dir)));
        let key: &'static MasterKey = Box::leak(Box::new(MasterKey::generate().unwrap()));
        let skills: &'static Skills =
            Box::leak(Box::new(Skills::load(&SkillConfig::disabled()).unwrap()));
        let (api_socket, ui_socket) = (socket_path("ui-serve-api"), socket_path("ui-serve-ui"));
        let mut guards = Vec::new();
        for socket in [&api_socket, &ui_socket] {
            let _ = std::fs::remove_file(socket.as_path());
            guards.push(TestSocket::new(socket.as_path().to_string_lossy()));
        }
        let token_path = ui_token_path(&ui_socket);
        guards.push(TestSocket::new(token_path.to_string_lossy()));
        // A UI socket alone: refused, it's served beside the API.
        let alone = ApiConfig {
            socket: None,
            ui_socket: Some(ui_socket.clone()),
            grants: BTreeSet::new(),
        };
        assert!(matches!(
            open_api(config, &alone),
            Err(Error::InvalidConfig {
                key: "HATCHLING_UI_SOCKET",
                ..
            })
        ));
        let settings = ApiConfig {
            socket: Some(api_socket.clone()),
            ui_socket: Some(ui_socket.clone()),
            grants: BTreeSet::new(),
        };
        let api: &'static Api = Box::leak(Box::new(open_api(config, &settings).unwrap().unwrap()));
        assert!(api.serves_ui());
        let token = std::fs::read_to_string(&token_path).unwrap();
        std::thread::spawn(move || api.serve(config, key, skills, None));
        let hello_on = |socket: &SocketPath, token: Option<&str>| {
            let client = UnixStream::connect(socket.as_path()).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            write(&client, &hello(token));
            client
        };
        // The UI socket, with the token: welcomed; without, or with
        // another: closed without a word.
        let welcomed = hello_on(&ui_socket, Some(&token));
        assert!(matches!(read(&welcomed), UiResponse::Welcome { .. }));
        for wrong in [None, Some("not-the-token")] {
            assert!(closed(&hello_on(&ui_socket, wrong)));
        }
        // The API socket is unchanged: no token needed.
        let api_client = hello_on(&api_socket, None);
        assert!(matches!(read(&api_client), UiResponse::Welcome { .. }));
        drop(guards);
        drop(dir);
    }

    #[test]
    fn the_served_api_records_its_own_calls_onto_its_stream() {
        let dir = scratch("serve");
        let config: &'static Config = Box::leak(Box::new(config_in(&dir)));
        let key: &'static MasterKey = Box::leak(Box::new(MasterKey::generate().unwrap()));
        let skills: &'static Skills =
            Box::leak(Box::new(Skills::load(&SkillConfig::disabled()).unwrap()));
        let socket = socket_path("serve");
        let _ = std::fs::remove_file(socket.as_path());
        let _guard = TestSocket::new(socket.as_path().to_string_lossy());
        let settings = ApiConfig {
            socket: Some(socket.clone()),
            ui_socket: None,
            grants: BTreeSet::new(),
        };
        let api: &'static Api = Box::leak(Box::new(open_api(config, &settings).unwrap().unwrap()));
        // Served for the rest of the test process, as the broker serves it.
        std::thread::spawn(move || api.serve(config, key, skills, None));
        let client = UnixStream::connect(socket.as_path()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write(&client, &hello(None));
        assert!(matches!(read(&client), UiResponse::Welcome { .. }));
        write(&client, &UiRequest::Subscribe { after_seq: None });
        // The hello and the subscribe were recorded in the activity log,
        // which the stream follows: both come back as activity.
        let mut kinds = Vec::new();
        while kinds.len() < 2 {
            if let UiResponse::Event(event) = read(&client) {
                if let UiEventBody::Activity { event, .. } = event.body {
                    if let SubagentEvent::ApiCall {
                        request, outcome, ..
                    } = event.kind
                    {
                        kinds.push((request, outcome));
                    }
                }
            }
        }
        assert_eq!(
            kinds,
            [
                (ApiRequest::Hello, ApiOutcome::Served),
                (ApiRequest::Subscribe, ApiOutcome::Served)
            ]
        );
        assert_eq!(API_MAX_CONNECTIONS, 8);
        drop(dir);
    }

    #[test]
    fn a_session_ends_cleanly_when_the_caller_leaves() {
        let dir = scratch("direct");
        let config = config_in(&dir);
        let log = config.scheduler.activity_log();
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let (api, _socket) = api_with(
            Box::new(PeerProvider::new(real_uid(), BTreeSet::new())),
            log.clone(),
            "direct",
        );
        let ops = Ops::new(&config, &key, &skills, None, Arc::clone(api.bus()));
        let (client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut audit = Audit::new(log, String::from("api-direct"));
        std::thread::scope(|scope| {
            let worker =
                scope.spawn(|| api.session(&server, api.local.provider.as_ref(), &ops, &mut audit));
            write(&client, &hello(None));
            let welcomed = matches!(read(&client), UiResponse::Welcome { .. });
            assert!(welcomed);
            drop(client);
            assert!(worker.join().unwrap().is_ok());
        });
    }

    #[test]
    fn requests_start_the_stream_once_and_answer_each_action() {
        let dir = scratch("requests");
        let config = config_in(&dir);
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let ops = Ops::new(&config, &key, &skills, None, Arc::new(Bus::starting_at(1)));
        let caller = Caller::new(String::from("uid 1000"), BTreeSet::new());
        let mut audit = Audit::new(
            config.scheduler.activity_log(),
            String::from("api-requests"),
        );
        let (client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let writer = Mutex::new(server.try_clone().unwrap());
        let mut starts = Vec::new();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                requests(&server, &ops, &mut audit, &caller, &writer, |after| {
                    starts.push(after);
                })
            });
            write(&client, &UiRequest::Subscribe { after_seq: Some(4) });
            write(&client, &UiRequest::Subscribe { after_seq: None });
            write(
                &client,
                &action(
                    "stop",
                    UiAction::StopBrowser {
                        session_id: String::from("s"),
                    },
                ),
            );
            // The caller holds no grants, so every action is refused as
            // not granted, before its backend is looked for.
            assert_eq!(
                read(&client),
                UiResponse::ActionResult {
                    request_id: String::from("stop"),
                    result: UiActionResult::Refused {
                        reason: ActionRefusal::NotGranted
                    }
                }
            );
            drop(client);
            assert!(worker.join().unwrap().is_ok());
        });
        assert_eq!(
            starts,
            [Some(4)],
            "the stream starts once, from the first mark"
        );
    }

    #[test]
    fn the_stream_sends_what_it_holds_then_what_arrives_until_told_to_stop() {
        let bus = Bus::starting_at(1);
        let chat = |text: &str| {
            UiEventBody::ChatMessage(ChatMessage {
                chat_id: String::from("local"),
                message_id: String::from("m"),
                author: ChatAuthor::Agent,
                text: text.to_owned(),
            })
        };
        bus.push(String::from("t1"), chat("one"));
        bus.push(String::from("t2"), chat("two"));
        let (client, server) = UnixStream::pair().unwrap();
        // Each read returns as soon as its frame arrives: the timeout only
        // bounds a broken stream, so it is generous for a loaded machine.
        client.set_read_timeout(Some(PATIENCE)).unwrap();
        let writer = Mutex::new(server);
        let ended = AtomicBool::new(false);
        let seq = |response: UiResponse| {
            if let UiResponse::Event(event) = response {
                event.seq
            } else {
                0
            }
        };
        std::thread::scope(|scope| {
            let _stop = StopOnDrop(&ended);
            let worker = scope.spawn(|| follow(&bus, &writer, &ended, Some(1)));
            assert_eq!(seq(read(&client)), 2, "only what is after the mark");
            bus.push(String::from("t3"), chat("three"));
            assert_eq!(seq(read(&client)), 3);
            ended.store(true, Ordering::Relaxed);
            worker.join().unwrap();
        });
        // A caller that is gone ends the stream at the next event, by the
        // failed write: the watchdog, which would otherwise stop it, never
        // has to. That is asserted as a signal, not a wall-clock bound.
        // The caller hangs up with a shutdown, not only a drop: a child
        // that a parallel test forked may hold a copy of this end, and a
        // shutdown fails the next write whoever holds one.
        client.shutdown(std::net::Shutdown::Both).unwrap();
        drop(client);
        let ended = AtomicBool::new(false);
        let finished = AtomicBool::new(false);
        let watchdog_fired = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let _stop = StopOnDrop(&ended);
            let worker = scope.spawn(|| {
                follow(&bus, &writer, &ended, Some(3));
                finished.store(true, Ordering::SeqCst);
            });
            scope.spawn(|| {
                let give_up = std::time::Instant::now() + PATIENCE;
                while !finished.load(Ordering::SeqCst) && std::time::Instant::now() < give_up {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if !finished.load(Ordering::SeqCst) {
                    watchdog_fired.store(true, Ordering::SeqCst);
                    ended.store(true, Ordering::Relaxed);
                }
            });
            bus.push(String::from("t4"), chat("four"));
            worker.join().unwrap();
        });
        assert!(
            !watchdog_fired.load(Ordering::SeqCst),
            "the failed write ended the stream, not the watchdog"
        );
    }

    #[test]
    fn the_broker_stops_at_start_up_when_its_vault_cannot_open() {
        // The vault under `/nonexistent` cannot be opened: start-up ends
        // with an error before any socket is bound or any API opened.
        assert!(crate::run_serve(&owner_config(None)).is_err());
    }
}
