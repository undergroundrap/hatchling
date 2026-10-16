//! The operator socket: what the people who run the machine may see
//! (WO-022).
//!
//! A listener of its own (`HATCHLING_OPERATOR_SOCKET`), beside the user's
//! API and UI sockets and never one of them, serving the operator's own
//! frames ([`OperatorRequest`]): a user's action can't be decoded here,
//! and an operator's query can't be decoded there. It is held to the
//! API's rules (owner-only, by the kernel's peer credentials, and
//! refused when cells run as the broker's uid) and, as the UI socket
//! is, to a token of its own, minted at each start into
//! `<socket>.token`. A hello without it, or with another (the UI's
//! included), is closed unanswered.
//!
//! Every answer is metadata: the broker's version and uptime, the cells
//! running and the API and UI connections open ([`crate::machine`]).
//! Nothing a user said or stored is ever in one.

use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use hatchling_core::framing::{read_frame_or_eof, write_frame};
use hatchling_core::ui::ApiToken;
use hatchling_core::ui::operator::{
    OperatorAnswer, OperatorQuery, OperatorRequest, OperatorResponse, operator_negotiate,
};
use hatchling_core::{PeerUid, SocketPath};
use subtle::ConstantTimeEq as _;

use crate::api::{API_SOCKET_MODE, API_WRITE_TIMEOUT, HELLO_TIMEOUT};
use crate::api_auth::{ApiConfig, from_owner, mint_ui_token, parse_socket_path, ui_token_path};
use crate::connections::ConnectionSlots;
use crate::serve::session_error;
use crate::{Config, Error, bind_socket};

/// Most operator connections served at once.
pub const OPERATOR_MAX_CONNECTIONS: usize = 4;

/// The operator socket's setting, `raw` (`HATCHLING_OPERATOR_SOCKET`):
/// its path, when it is on.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] for a socket that is not an absolute
/// path, lies inside the broker socket's directory, or is the API's or
/// the UI's: each socket holds its callers to its own rule.
pub fn parse_operator_socket(
    config: &Config,
    api: &ApiConfig,
    raw: Option<&str>,
) -> Result<Option<SocketPath>, Error> {
    let key = "HATCHLING_OPERATOR_SOCKET";
    let socket = parse_socket_path(raw, config, key)?;
    if socket.is_some() && (socket == api.socket || socket == api.ui_socket) {
        return Err(Error::InvalidConfig {
            key,
            reason: String::from(
                "the operator socket must differ from the API and UI sockets: each holds its callers to its own rule",
            ),
        });
    }
    Ok(socket)
}

/// The bound operator socket.
pub struct Operator {
    listener: UnixListener,
    token: ApiToken,
    /// The only uid a byte is read from: the socket's owner, the broker.
    owner: PeerUid,
    /// When the broker started serving, for its uptime.
    started: Instant,
    started_at: String,
    slots: ConnectionSlots,
}

impl std::fmt::Debug for Operator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Operator")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// The operator socket, bound, when one is configured, with its token
/// minted.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] when cells run as the broker's own
/// uid (a cell could then ask); the bind errors of [`bind_socket`]; or
/// [`mint_ui_token`]'s.
pub fn open_operator(
    config: &Config,
    socket: Option<&SocketPath>,
) -> Result<Option<Operator>, Error> {
    let Some(socket) = socket else {
        return Ok(None);
    };
    let owner = PeerUid::from_kernel(hatchling_sys::euid());
    if owner == config.agent_uid {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_OPERATOR_SOCKET",
            reason: format!(
                "the operator socket serves uid {owner}, the broker's own, which is also the agent's: \
                 a cell could ask it"
            ),
        });
    }
    let listener = bind_socket(socket, API_SOCKET_MODE)?;
    let token = mint_ui_token(&ui_token_path(socket))?;
    Ok(Some(Operator::new(listener, token, owner)))
}

impl Operator {
    /// The operator socket on `listener`, admitting `owner` with `token`.
    #[must_use]
    pub fn new(listener: UnixListener, token: ApiToken, owner: PeerUid) -> Self {
        Self {
            listener,
            token,
            owner,
            started: Instant::now(),
            started_at: hatchling_scheduler::Timestamp::now().to_string(),
            slots: ConnectionSlots::new(OPERATOR_MAX_CONNECTIONS),
        }
    }

    /// Serves operators, each on its own thread, forever.
    pub fn serve(&self) -> ! {
        std::thread::scope(|scope| {
            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => match self.slots.try_acquire() {
                        Ok(slot) => {
                            scope.spawn(move || {
                                self.serve_operator(&stream);
                                drop(slot);
                            });
                        }
                        Err(full) => {
                            if full.first_of_episode {
                                tracing::warn!(
                                    max = OPERATOR_MAX_CONNECTIONS,
                                    "operator connection cap reached: closing new connections until one ends"
                                );
                            }
                        }
                    },
                    Err(source) => tracing::error!(%source, "operator accept failed"),
                }
            }
        })
    }

    /// Serves one operator on `stream` until it closes, or breaks the
    /// protocol.
    pub fn serve_operator(&self, stream: &UnixStream) {
        match self.session(stream) {
            Ok(Admission::Admitted) => tracing::info!("operator connection ended"),
            Ok(Admission::Refused) => tracing::warn!("operator connection refused"),
            Err(error) => tracing::info!(%error, "operator connection ended"),
        }
    }

    /// The handshake, then the operator's queries.
    fn session(&self, stream: &UnixStream) -> Result<Admission, Error> {
        if !from_owner(stream, self.owner) {
            return Ok(Admission::Refused);
        }
        let io = |source| Error::SessionIo { source };
        stream.set_read_timeout(Some(HELLO_TIMEOUT)).map_err(io)?;
        stream
            .set_write_timeout(Some(API_WRITE_TIMEOUT))
            .map_err(io)?;
        let Some(first) = read_frame_or_eof(stream).map_err(session_error)? else {
            return Ok(Admission::Refused);
        };
        let OperatorRequest::Hello {
            schema_version,
            token,
            ..
        } = decode(&first)?
        else {
            return Ok(Admission::Refused);
        };
        let answer = operator_negotiate(schema_version);
        if matches!(answer, OperatorResponse::Incompatible { .. }) {
            send(stream, &answer)?;
            return Ok(Admission::Refused);
        }
        if !self.admits(token.as_ref()) {
            return Ok(Admission::Refused);
        }
        send(stream, &answer)?;
        stream.set_read_timeout(None).map_err(io)?;
        while let Some(frame) = read_frame_or_eof(stream).map_err(session_error)? {
            let OperatorRequest::Query { request_id, query } = decode(&frame)? else {
                return Err(Error::SessionProtocol {
                    reason: String::from("a second operator hello"),
                });
            };
            let answer = self.answer(query, self.started.elapsed());
            send(stream, &OperatorResponse::Answer { request_id, answer })?;
        }
        Ok(Admission::Admitted)
    }

    /// Whether `token` is the operator's, compared in constant time.
    fn admits(&self, token: Option<&ApiToken>) -> bool {
        let presented = token.map(ApiToken::expose).unwrap_or_default();
        bool::from(presented.as_bytes().ct_eq(self.token.expose().as_bytes()))
    }

    /// The answer to `query`, `uptime` after the start.
    fn answer(&self, query: OperatorQuery, uptime: Duration) -> OperatorAnswer {
        match query {
            OperatorQuery::Broker {} => OperatorAnswer::Broker {
                version: String::from(env!("CARGO_PKG_VERSION")),
                started_at: self.started_at.clone(),
                uptime_secs: uptime.as_secs(),
            },
            OperatorQuery::Cells {} => OperatorAnswer::Cells {
                cells: crate::machine::cells(),
            },
            OperatorQuery::Sessions {} => OperatorAnswer::Sessions {
                sessions: crate::machine::sessions(),
            },
        }
    }
}

/// How a connection's handshake went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    Admitted,
    Refused,
}

/// One operator frame. The client is told nothing of why one fails to
/// decode; the broker's log has it.
fn decode(frame: &[u8]) -> Result<OperatorRequest, Error> {
    serde_json::from_slice(frame).map_err(|error| Error::SessionProtocol {
        reason: format!("operator frame is not an operator request: {error}"),
    })
}

/// Writes one response frame.
fn send(stream: &UnixStream, response: &OperatorResponse) -> Result<(), Error> {
    let payload = serde_json::to_vec(response).map_err(|error| Error::SessionProtocol {
        reason: format!("operator response encode failed: {error}"),
    })?;
    write_frame(stream, &payload).map_err(session_error)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt as _;

    use hatchling_core::ui::{UI_SCHEMA_VERSION, UiAction, UiRequest};

    use super::*;
    use crate::api_auth::tests::{owner_config, real_uid};
    use crate::test_socket::TestSocket;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// An operator socket for the test `name`: each test binds its own.
    fn operator(name: &str, owner: PeerUid) -> (Operator, TestSocket) {
        let path = format!("/tmp/hlop-{}-{name}.sock", std::process::id());
        let _ = std::fs::remove_file(&path);
        let guard = TestSocket::new(path.clone());
        let listener = UnixListener::bind(&path).unwrap();
        (
            Operator::new(listener, ApiToken::new(String::from(TOKEN)), owner),
            guard,
        )
    }

    /// Runs `script` as a client of `operator`, served on the other end
    /// of a socket pair, and returns once both ends are done.
    fn session(operator: &Operator, script: impl FnOnce(&UnixStream) + Send) {
        let (client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(move || {
                operator.serve_operator(&server);
                drop(server);
            });
            script(&client);
            drop(client);
            worker.join().unwrap();
        });
    }

    fn write(client: &UnixStream, value: &impl serde::Serialize) {
        // The broker may hang up first: a failed write is that.
        let _ = write_frame(client, &serde_json::to_vec(value).unwrap());
    }

    fn read(client: &UnixStream) -> Option<OperatorResponse> {
        read_frame_or_eof(client)
            .ok()
            .flatten()
            .map(|frame| serde_json::from_slice(&frame).unwrap())
    }

    fn hello(token: Option<&str>) -> OperatorRequest {
        OperatorRequest::Hello {
            schema_version: UI_SCHEMA_VERSION,
            client: String::from("test"),
            token: token.map(|token| ApiToken::new(token.to_owned())),
        }
    }

    fn broker_query(id: &str) -> OperatorRequest {
        OperatorRequest::Query {
            request_id: id.to_owned(),
            query: OperatorQuery::Broker {},
        }
    }

    #[test]
    fn the_operator_socket_is_its_own_absolute_path_outside_the_cells() {
        let config = owner_config(None);
        let api = ApiConfig {
            socket: Some("/run/hatchling/api.sock".parse().unwrap()),
            ui_socket: Some("/run/hatchling/ui.sock".parse().unwrap()),
            grants: std::collections::BTreeSet::new(),
        };
        assert_eq!(parse_operator_socket(&config, &api, None).unwrap(), None);
        assert_eq!(
            parse_operator_socket(&config, &api, Some("/run/hatchling/operator.sock")).unwrap(),
            Some("/run/hatchling/operator.sock".parse().unwrap())
        );
        // No scheduler is needed: the operator socket writes no activity.
        assert!(!config.scheduler.enabled());
        for refused in [
            "relative.sock",
            "/tmp/hatchling-test-api-dummy/operator.sock",
            "/run/hatchling/api.sock",
            "/run/hatchling/ui.sock",
        ] {
            assert!(
                matches!(
                    parse_operator_socket(&config, &api, Some(refused)),
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_OPERATOR_SOCKET",
                        ..
                    })
                ),
                "{refused}"
            );
        }
    }

    #[test]
    fn the_socket_and_its_token_are_the_owners_and_never_the_agents_uid() {
        let config = owner_config(None);
        assert!(open_operator(&config, None).unwrap().is_none());
        let path = format!("/tmp/hlop-open-{}.sock", std::process::id());
        let socket: SocketPath = path.parse().unwrap();
        let _ = std::fs::remove_file(&path);
        let _guard = TestSocket::new(path.clone());
        let token_path = ui_token_path(&socket);
        let opened = open_operator(&config, Some(&socket)).unwrap().unwrap();
        let shown = format!("{opened:?}");
        assert!(shown.starts_with("Operator"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            API_SOCKET_MODE
        );
        let token = std::fs::metadata(&token_path).unwrap();
        assert_eq!(token.mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::read_to_string(&token_path).unwrap(),
            opened.token.expose()
        );
        drop(opened);
        let _ = std::fs::remove_file(&token_path);
        std::fs::remove_file(&path).unwrap();
        let mut agents = config;
        agents.agent_uid = real_uid();
        assert!(matches!(
            open_operator(&agents, Some(&socket)),
            Err(Error::InvalidConfig {
                key: "HATCHLING_OPERATOR_SOCKET",
                ..
            })
        ));
        assert!(!std::path::Path::new(&path).exists(), "no socket was bound");
    }

    #[test]
    fn a_hello_without_the_operator_token_is_closed_unanswered() {
        let (operator, _socket) = operator("tokenless", real_uid());
        let ui_token = "fedcba9876543210fedcba9876543210";
        // A token one character short is still not the token.
        let short = "123456789abcdef0123456789abcdef";
        for token in [None, Some(ui_token), Some(short), Some("")] {
            session(&operator, |client| {
                write(client, &hello(token));
                write(client, &broker_query("q"));
                assert_eq!(read(client), None, "{token:?}");
            });
        }
        // Nor does anything before a hello get an answer.
        session(&operator, |client| {
            write(client, &broker_query("early"));
            assert_eq!(read(client), None);
        });
    }

    #[test]
    fn a_peer_that_is_not_the_owner_gets_no_byte_read() {
        let (operator, _socket) = operator(
            "foreign",
            PeerUid::from_kernel(real_uid().as_u32().wrapping_add(1)),
        );
        session(&operator, |client| {
            write(client, &hello(Some(TOKEN)));
            assert_eq!(read(client), None);
        });
    }

    #[test]
    fn the_operator_is_welcomed_and_answered_with_metadata_only() {
        let (operator, _socket) = operator("welcomed", real_uid());
        session(&operator, |client| {
            write(client, &hello(Some(TOKEN)));
            assert_eq!(
                read(client),
                Some(OperatorResponse::Welcome {
                    schema_version: UI_SCHEMA_VERSION
                })
            );
            for id in ["q1", "q2"] {
                write(client, &broker_query(id));
                let answered = read(client);
                let Some(OperatorResponse::Answer { request_id, answer }) = answered else {
                    unreachable!("no answer to {id}: {answered:?}");
                };
                assert_eq!(request_id, id);
                let OperatorAnswer::Broker {
                    version,
                    started_at,
                    uptime_secs,
                } = answer
                else {
                    unreachable!("not the broker's answer");
                };
                assert_eq!(version, env!("CARGO_PKG_VERSION"));
                assert_eq!(started_at, operator.started_at);
                assert!(started_at.parse::<hatchling_scheduler::Timestamp>().is_ok());
                assert!(uptime_secs < 60);
            }
        });
    }

    #[test]
    fn a_users_action_or_a_second_hello_ends_the_operators_session() {
        let (operator, _socket) = operator("user-frames", real_uid());
        let action = UiRequest::Action {
            request_id: String::from("a1"),
            action: UiAction::SendMessage {
                chat_id: String::from("local"),
                text: String::from("hi"),
            },
        };
        let subscribe = UiRequest::Subscribe { after_seq: None };
        for after in [
            serde_json::to_value(&action).unwrap(),
            serde_json::to_value(&subscribe).unwrap(),
            serde_json::to_value(hello(Some(TOKEN))).unwrap(),
        ] {
            session(&operator, |client| {
                write(client, &hello(Some(TOKEN)));
                assert!(matches!(
                    read(client),
                    Some(OperatorResponse::Welcome { .. })
                ));
                write(client, &after);
                write(client, &broker_query("after"));
                assert_eq!(read(client), None, "{after}");
            });
        }
    }

    #[test]
    fn an_operator_at_another_version_is_told_so_and_closed() {
        let (operator, _socket) = operator("versions", real_uid());
        session(&operator, |client| {
            write(
                client,
                &OperatorRequest::Hello {
                    schema_version: UI_SCHEMA_VERSION + 1,
                    client: String::from("newer"),
                    token: Some(ApiToken::new(String::from(TOKEN))),
                },
            );
            assert!(matches!(
                read(client),
                Some(OperatorResponse::Incompatible { frontend, .. }) if frontend == UI_SCHEMA_VERSION + 1
            ));
            write(client, &broker_query("q"));
            assert_eq!(read(client), None);
        });
    }

    #[test]
    fn the_operator_sees_each_running_cell_and_not_once_it_ends() {
        use hatchling_core::ui::operator::CellKind;
        let (operator, _socket) = operator("cells", real_uid());
        let mine = |answer: OperatorAnswer| -> Vec<(String, CellKind)> {
            let OperatorAnswer::Cells { cells } = answer else {
                return Vec::new();
            };
            cells
                .into_iter()
                .filter(|cell| cell.cell_id.starts_with("op-cells-"))
                .map(|cell| (cell.cell_id, cell.kind))
                .collect()
        };
        let agent = crate::machine::Running::start(
            "op-cells-agent",
            CellKind::Agent,
            &crate::CellLimits::default(),
        );
        let run = crate::machine::Running::start(
            "op-cells-run",
            CellKind::Run,
            &crate::CellLimits::default(),
        );
        let ask = || operator.answer(OperatorQuery::Cells {}, Duration::ZERO);
        assert_eq!(
            mine(ask()),
            [
                (String::from("op-cells-agent"), CellKind::Agent),
                (String::from("op-cells-run"), CellKind::Run)
            ]
        );
        drop(run);
        assert_eq!(
            mine(ask()),
            [(String::from("op-cells-agent"), CellKind::Agent)]
        );
        // Over the socket, the same answer.
        session(&operator, |client| {
            write(client, &hello(Some(TOKEN)));
            assert!(matches!(
                read(client),
                Some(OperatorResponse::Welcome { .. })
            ));
            write(
                client,
                &OperatorRequest::Query {
                    request_id: String::from("c1"),
                    query: OperatorQuery::Cells {},
                },
            );
            let Some(OperatorResponse::Answer { request_id, answer }) = read(client) else {
                unreachable!("no answer");
            };
            assert_eq!(request_id, "c1");
            assert_eq!(
                mine(answer),
                [(String::from("op-cells-agent"), CellKind::Agent)]
            );
        });
        drop(agent);
        assert!(mine(ask()).is_empty());
    }

    #[test]
    fn the_operator_sees_each_open_session_and_not_once_it_closes() {
        use hatchling_core::ui::operator::SessionSocket;
        let (operator, _socket) = operator("sessions", real_uid());
        let mine = |answer: OperatorAnswer| -> Vec<String> {
            let OperatorAnswer::Sessions { sessions } = answer else {
                return Vec::new();
            };
            sessions
                .into_iter()
                .filter(|open| open.connection.starts_with("op-sessions-"))
                .map(|open| open.connection)
                .collect()
        };
        let open = crate::machine::Open::start("op-sessions-ui", SessionSocket::Ui);
        let ask = || operator.answer(OperatorQuery::Sessions {}, Duration::ZERO);
        assert_eq!(mine(ask()), [String::from("op-sessions-ui")]);
        drop(open);
        assert!(mine(ask()).is_empty());
    }

    #[test]
    fn answers_carry_the_brokers_start_and_uptime() {
        let (operator, _socket) = operator("answers", real_uid());
        assert_eq!(
            operator.answer(OperatorQuery::Broker {}, Duration::from_millis(61_900)),
            OperatorAnswer::Broker {
                version: String::from(env!("CARGO_PKG_VERSION")),
                started_at: operator.started_at.clone(),
                uptime_secs: 61,
            }
        );
        assert!(operator.admits(Some(&ApiToken::new(String::from(TOKEN)))));
        assert!(!operator.admits(None));
    }
}
