//! Agent side of the goals and query protocol.
//!
//! The framing and message vocabulary live in [`hatchling_core`]; this
//! module is the agent's behavior: send one op per frame and require a
//! well-formed reply of the matching kind. Must only be called on a
//! [`Ready`](crate::Ready) cell — the typestate enforces this.
//!
//! Writes ([`create_goal`], [`update_goal`], [`close_goal`],
//! [`append_tracking`]) and reads ([`query`]) are separate session
//! kinds. The feed gets a [`FeedHandle`]: it can only query, because
//! the only request it can build is a [`QueryOp`], and no write can be
//! spelled as one.

use std::os::unix::net::UnixStream;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{GoalsOp, GoalsResult, QueryOp, QueryResult, SessionRequest, SessionResponse};

use crate::Error;

/// What an update or close did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalChange {
    /// The goal changed.
    Changed,
    /// The user has no goal with this id.
    NotFound,
    /// The goal is closed and can no longer change.
    AlreadyClosed,
}

/// One page of query results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPage {
    /// Feed items, each as JSON with its provenance and label.
    pub items_json: Vec<String>,
    /// The cursor to pass as `after` for the next page, or `None` when
    /// this page ends the result.
    pub next: Option<String>,
}

/// Records a new goal the user explicitly asked for, returning the
/// broker-minted id.
///
/// # Errors
///
/// Returns [`Error::GoalsDenied`] without a grant,
/// [`Error::GoalsRejected`] for refused content,
/// [`Error::GoalsLimitReached`] at the store bound,
/// [`Error::GoalsFailed`] on a backend failure, [`Error::GoalsIo`] on
/// transport failure, or [`Error::GoalsProtocol`] on a malformed reply.
pub fn create_goal(stream: &UnixStream, title: &str, detail: &str) -> Result<String, Error> {
    let op = GoalsOp::GoalCreate {
        title: title.to_owned(),
        detail: detail.to_owned(),
    };
    match outcome(&op, write(stream, &op)?)? {
        Done::Created(id) => Ok(id),
        other @ (Done::Change(_) | Done::Appended(_)) => Err(unexpected(&op, &other)),
    }
}

/// Changes the title and/or detail of an open goal.
///
/// # Errors
///
/// As [`create_goal`].
pub fn update_goal(
    stream: &UnixStream,
    id: &str,
    title: Option<&str>,
    detail: Option<&str>,
) -> Result<GoalChange, Error> {
    let op = GoalsOp::GoalUpdate {
        id: id.to_owned(),
        title: title.map(str::to_owned),
        detail: detail.map(str::to_owned),
    };
    change(stream, &op)
}

/// Closes an open goal with the user's outcome, `achieved` or
/// `abandoned`.
///
/// # Errors
///
/// As [`create_goal`].
pub fn close_goal(stream: &UnixStream, id: &str, outcome: &str) -> Result<GoalChange, Error> {
    let op = GoalsOp::GoalClose {
        id: id.to_owned(),
        outcome: outcome.to_owned(),
    };
    change(stream, &op)
}

/// Appends one entry to the user's tracking log, returning its
/// sequence number.
///
/// # Errors
///
/// As [`create_goal`].
pub fn append_tracking(stream: &UnixStream, source: &str, text: &str) -> Result<u64, Error> {
    let op = GoalsOp::TrackingAppend {
        source: source.to_owned(),
        text: text.to_owned(),
    };
    match outcome(&op, write(stream, &op)?)? {
        Done::Appended(seq) => Ok(seq),
        other @ (Done::Created(_) | Done::Change(_)) => Err(unexpected(&op, &other)),
    }
}

/// Runs one read-scoped query.
///
/// # Errors
///
/// Returns [`Error::GoalsDenied`] without a grant, [`Error::GoalsFailed`]
/// on a backend failure, [`Error::GoalsIo`] on transport failure, or
/// [`Error::GoalsProtocol`] on a malformed reply.
pub fn query(stream: &UnixStream, op: &QueryOp) -> Result<QueryPage, Error> {
    let request = SessionRequest::Query(op.clone());
    match exchange(stream, &request)? {
        SessionResponse::Query(QueryResult::Items { items_json, next }) => {
            Ok(QueryPage { items_json, next })
        }
        SessionResponse::Query(QueryResult::Denied) => Err(Error::GoalsDenied {
            action: query_name(op).to_owned(),
        }),
        SessionResponse::Query(QueryResult::QueryError) => Err(Error::GoalsFailed),
        other @ (SessionResponse::Vault(_)
        | SessionResponse::Egress(_)
        | SessionResponse::Channel(_)
        | SessionResponse::Memory(_)
        | SessionResponse::Skill(_)
        | SessionResponse::Goals(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_)
        | SessionResponse::Browser(_)) => Err(wrong_kind(query_name(op), &other)),
    }
}

/// The feed's read-only handle on a broker session.
///
/// Built from the session by whoever owns it, then handed to the feed.
/// The feed gets no stream, so the only request it can send is the one
/// [`query`](Self::query) builds: a [`QueryOp`], a vocabulary with no
/// write in it.
#[derive(Debug, Clone, Copy)]
pub struct FeedHandle<'s> {
    stream: &'s UnixStream,
}

impl<'s> FeedHandle<'s> {
    /// A read-only handle on `stream`.
    #[must_use]
    pub const fn new(stream: &'s UnixStream) -> Self {
        Self { stream }
    }

    /// Runs one read-scoped query.
    ///
    /// # Errors
    ///
    /// As the free function [`query`](super::goals::query).
    pub fn query(&self, op: &QueryOp) -> Result<QueryPage, Error> {
        query(self.stream, op)
    }
}

/// Sends an update or close and maps the three outcomes.
fn change(stream: &UnixStream, op: &GoalsOp) -> Result<GoalChange, Error> {
    match outcome(op, write(stream, op)?)? {
        Done::Change(change) => Ok(change),
        other @ (Done::Created(_) | Done::Appended(_)) => Err(unexpected(op, &other)),
    }
}

/// A successful goals reply, before it is matched against the op.
#[derive(Debug)]
enum Done {
    Created(String),
    Change(GoalChange),
    Appended(u64),
}

/// Classifies a goals reply: a success, or the error for a failure
/// every write shares (denied, rejected, limit, backend failure). Each
/// variant is named so a new `GoalsResult` breaks compilation here
/// instead of being silently accepted.
fn outcome(op: &GoalsOp, result: GoalsResult) -> Result<Done, Error> {
    match result {
        GoalsResult::GoalCreated { id } => Ok(Done::Created(id)),
        GoalsResult::GoalChanged => Ok(Done::Change(GoalChange::Changed)),
        GoalsResult::GoalNotFound => Ok(Done::Change(GoalChange::NotFound)),
        GoalsResult::GoalAlreadyClosed => Ok(Done::Change(GoalChange::AlreadyClosed)),
        GoalsResult::TrackingAppended { seq } => Ok(Done::Appended(seq)),
        GoalsResult::Denied => Err(Error::GoalsDenied {
            action: action_name(op).to_owned(),
        }),
        GoalsResult::Rejected => Err(Error::GoalsRejected {
            action: action_name(op).to_owned(),
        }),
        GoalsResult::LimitReached => Err(Error::GoalsLimitReached),
        GoalsResult::GoalsError => Err(Error::GoalsFailed),
    }
}

/// The protocol violation for a success that belongs to another op
/// (e.g. `tracking-appended` in answer to a create).
fn unexpected(op: &GoalsOp, done: &Done) -> Error {
    Error::GoalsProtocol {
        reason: format!(
            "broker gave an unexpected reply to {}: {done:?}",
            action_name(op)
        ),
    }
}

/// Sends one goals op and requires a goals reply.
fn write(stream: &UnixStream, op: &GoalsOp) -> Result<GoalsResult, Error> {
    match exchange(stream, &SessionRequest::Goals(op.clone()))? {
        SessionResponse::Goals(result) => Ok(result),
        other @ (SessionResponse::Vault(_)
        | SessionResponse::Egress(_)
        | SessionResponse::Channel(_)
        | SessionResponse::Memory(_)
        | SessionResponse::Skill(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_)
        | SessionResponse::Browser(_)) => Err(wrong_kind(action_name(op), &other)),
    }
}

/// One request frame out, one reply frame in. EOF mid-op is a
/// transport failure; a reply that is not a well-formed
/// `SessionResponse` is a protocol violation. Fail-closed throughout.
fn exchange(stream: &UnixStream, request: &SessionRequest) -> Result<SessionResponse, Error> {
    let payload = serde_json::to_vec(request).map_err(|e| Error::GoalsProtocol {
        reason: format!("failed to encode goals request: {e}"),
    })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    serde_json::from_slice(&reply).map_err(|e| Error::GoalsProtocol {
        reason: format!("broker reply is not a valid session response: {e}"),
    })
}

/// The protocol violation for a reply of another session kind.
fn wrong_kind(action: &str, reply: &SessionResponse) -> Error {
    Error::GoalsProtocol {
        reason: format!("broker gave a reply of another kind to {action}: {reply:?}"),
    }
}

/// The wire action name of a goals op, for errors.
const fn action_name(op: &GoalsOp) -> &'static str {
    match op {
        GoalsOp::GoalCreate { .. } => "goal-create",
        GoalsOp::GoalUpdate { .. } => "goal-update",
        GoalsOp::GoalClose { .. } => "goal-close",
        GoalsOp::TrackingAppend { .. } => "tracking-append",
    }
}

/// The grant name of a query, for errors.
const fn query_name(op: &QueryOp) -> &'static str {
    match op {
        QueryOp::Goals { .. } => "query-goals",
        QueryOp::Tracking { .. } => "query-tracking",
        QueryOp::Memory { .. } => "user:recall",
    }
}

/// Maps a frame-layer failure onto the goals vocabulary: transport
/// failures are [`GoalsIo`](Error::GoalsIo), everything else is a
/// protocol violation.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::GoalsIo { source };
    }
    Error::GoalsProtocol {
        reason: format!("goals frame failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use super::*;

    /// A socket pair whose broker end answers one frame with `reply`
    /// and hands back the request it read. Bounded reads: a broken
    /// exchange fails the test instead of hanging it.
    fn answer_once(reply: &SessionResponse) -> (UnixStream, std::thread::JoinHandle<Vec<u8>>) {
        let (agent, broker) = UnixStream::pair().unwrap();
        for end in [&agent, &broker] {
            end.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        let payload = serde_json::to_vec(reply).unwrap();
        let handle = std::thread::spawn(move || {
            let request = read_frame(&broker).unwrap();
            write_frame(&broker, &payload).unwrap();
            request
        });
        (agent, handle)
    }

    fn sent(handle: std::thread::JoinHandle<Vec<u8>>) -> SessionRequest {
        serde_json::from_slice(&handle.join().unwrap()).unwrap()
    }

    #[test]
    fn create_sends_a_goals_request_and_returns_the_id() {
        let (agent, broker) = answer_once(&SessionResponse::Goals(GoalsResult::GoalCreated {
            id: String::from("g1"),
        }));
        assert_eq!(create_goal(&agent, "run", "soon").unwrap(), "g1");
        assert_eq!(
            sent(broker),
            SessionRequest::Goals(GoalsOp::GoalCreate {
                title: String::from("run"),
                detail: String::from("soon"),
            })
        );
    }

    #[test]
    fn the_shared_failures_map_to_their_errors() {
        type Case = (GoalsResult, fn(&Error) -> bool);
        let cases: [Case; 4] = [
            (
                GoalsResult::Denied,
                |e| matches!(e, Error::GoalsDenied { action } if action == "tracking-append"),
            ),
            (
                GoalsResult::Rejected,
                |e| matches!(e, Error::GoalsRejected { action } if action == "tracking-append"),
            ),
            (GoalsResult::LimitReached, |e| {
                matches!(e, Error::GoalsLimitReached)
            }),
            (GoalsResult::GoalsError, |e| matches!(e, Error::GoalsFailed)),
        ];
        for (reply, expected) in cases {
            let (agent, broker) = answer_once(&SessionResponse::Goals(reply.clone()));
            let error = append_tracking(&agent, "user", "t").unwrap_err();
            assert!(expected(&error), "{reply:?} gave {error:?}");
            let _ = sent(broker);
        }
    }

    #[test]
    fn a_reply_meant_for_another_op_is_a_protocol_violation() {
        let (agent, broker) = answer_once(&SessionResponse::Goals(GoalsResult::TrackingAppended {
            seq: 1,
        }));
        let error = close_goal(&agent, "g1", "achieved").unwrap_err();
        assert!(
            matches!(&error, Error::GoalsProtocol { reason } if reason.contains("goal-close")),
            "{error:?}"
        );
        let _ = sent(broker);
        let (agent, broker) = answer_once(&SessionResponse::Query(QueryResult::Denied));
        let error = update_goal(&agent, "g1", Some("t"), None).unwrap_err();
        assert!(
            matches!(&error, Error::GoalsProtocol { reason } if reason.contains("goal-update")),
            "{error:?}"
        );
        let _ = sent(broker);
    }

    #[test]
    fn update_and_close_report_all_three_outcomes() {
        for (reply, expected) in [
            (GoalsResult::GoalChanged, GoalChange::Changed),
            (GoalsResult::GoalNotFound, GoalChange::NotFound),
            (GoalsResult::GoalAlreadyClosed, GoalChange::AlreadyClosed),
        ] {
            let (agent, broker) = answer_once(&SessionResponse::Goals(reply.clone()));
            assert_eq!(
                update_goal(&agent, "g1", None, Some("d")).unwrap(),
                expected
            );
            assert_eq!(
                sent(broker),
                SessionRequest::Goals(GoalsOp::GoalUpdate {
                    id: String::from("g1"),
                    title: None,
                    detail: Some(String::from("d")),
                })
            );
            let (agent, broker) = answer_once(&SessionResponse::Goals(reply));
            assert_eq!(close_goal(&agent, "g1", "abandoned").unwrap(), expected);
            let _ = sent(broker);
        }
    }

    #[test]
    fn the_feed_handle_only_ever_sends_a_query() {
        let op = QueryOp::Goals {
            status: String::from("open"),
            updated_since_secs: None,
            after: None,
        };
        let (agent, broker) = answer_once(&SessionResponse::Query(QueryResult::Items {
            items_json: vec![String::from("{}")],
            next: Some(String::from("g1")),
        }));
        let page = FeedHandle::new(&agent).query(&op).unwrap();
        assert_eq!(page.items_json, vec![String::from("{}")]);
        assert_eq!(page.next.as_deref(), Some("g1"));
        assert_eq!(sent(broker), SessionRequest::Query(op.clone()));
        for (reply, denied) in [
            (QueryResult::Denied, true),
            (QueryResult::QueryError, false),
        ] {
            let (agent, broker) = answer_once(&SessionResponse::Query(reply));
            let error = FeedHandle::new(&agent).query(&op).unwrap_err();
            if denied {
                assert!(matches!(&error, Error::GoalsDenied { action } if action == "query-goals"));
            } else {
                assert!(matches!(error, Error::GoalsFailed));
            }
            let _ = sent(broker);
        }
        let (agent, broker) = answer_once(&SessionResponse::Goals(GoalsResult::GoalChanged));
        assert!(matches!(
            FeedHandle::new(&agent).query(&op),
            Err(Error::GoalsProtocol { .. })
        ));
        let _ = sent(broker);
    }

    #[test]
    fn the_feed_handle_exposes_no_write_surface() {
        // Negative acceptance, agent side: the handle's public surface is
        // `new` and `query`, and its impl never builds a goals request.
        let source = include_str!("goals.rs");
        let code = source.split("#[cfg(test)]").next().unwrap_or(source);
        let handle_impl = code
            .split("impl<'s> FeedHandle<'s> {")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .unwrap();
        let public: Vec<&str> = handle_impl
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub fn "))
            .chain(
                handle_impl
                    .lines()
                    .filter_map(|line| line.trim().strip_prefix("pub const fn ")),
            )
            .filter_map(|rest| rest.split('(').next())
            .collect();
        assert_eq!(public, vec!["query", "new"]);
        for write in ["GoalsOp", "SessionRequest::Goals", "write(", "exchange("] {
            let hit = handle_impl
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .any(|line| line.contains(write));
            assert!(!hit, "the feed handle touches `{write}`");
        }
    }

    #[test]
    fn query_names_match_the_grants_that_gate_them() {
        let goals = QueryOp::Goals {
            status: String::new(),
            updated_since_secs: None,
            after: None,
        };
        let tracking = QueryOp::Tracking {
            since_secs: None,
            until_secs: None,
            source: None,
            after: None,
        };
        let memory = QueryOp::Memory {
            tag: String::new(),
            after: None,
        };
        assert_eq!(query_name(&goals), "query-goals");
        assert_eq!(query_name(&tracking), "query-tracking");
        assert_eq!(query_name(&memory), "user:recall");
    }

    #[test]
    fn a_dead_broker_is_a_transport_error() {
        let (agent, broker) = UnixStream::pair().unwrap();
        drop(broker);
        assert!(matches!(
            create_goal(&agent, "t", ""),
            Err(Error::GoalsIo { .. })
        ));
    }

    #[test]
    fn a_garbled_reply_is_a_protocol_violation() {
        let (agent, broker) = UnixStream::pair().unwrap();
        // Both ends bounded: if the request never arrives, the broker
        // thread fails instead of blocking the test forever.
        for end in [&agent, &broker] {
            end.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        let handle = std::thread::spawn(move || {
            let _ = read_frame(&broker).unwrap();
            write_frame(&broker, b"{not json").unwrap();
        });
        assert!(matches!(
            append_tracking(&agent, "user", "t"),
            Err(Error::GoalsProtocol { .. })
        ));
        handle.join().unwrap();
    }
}
