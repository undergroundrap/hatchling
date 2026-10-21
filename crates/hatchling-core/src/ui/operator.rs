//! The operator's protocol: what the people who run the machine may ask
//! the broker.
//!
//! It is served on its own socket (`HATCHLING_OPERATOR_SOCKET`), never on
//! the user's API or UI sockets, and its frames are their own types:
//! a user's socket can't decode an [`OperatorRequest`], and the operator's
//! can't decode a user's [`UiRequest`](super::UiRequest). The split is
//! the broker's, not a UI's.
//!
//! Every answer is metadata about the machine: never chat text, a task,
//! memory, an artifact, a secret, a surrogate or a body.

use serde::{Deserialize, Serialize};

use super::{ApiToken, UI_SCHEMA_VERSION, UiResponse, negotiate};

/// A frame the operator's client sends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorRequest {
    /// The first frame on a connection: the client's protocol version
    /// and the operator token.
    Hello {
        /// The client's [`UI_SCHEMA_VERSION`]: the operator's types are
        /// part of the same schema.
        schema_version: u16,
        /// Who is connecting, for the broker's log.
        client: String,
        /// The operator token, from `<socket>.token`.
        token: Option<ApiToken>,
    },
    /// One query, answered by one [`OperatorResponse::Answer`] carrying
    /// the same `request_id`.
    Query {
        /// The client's id for this request.
        request_id: String,
        /// What the operator asks.
        query: OperatorQuery,
    },
}

/// What an operator may ask: read-only, and metadata only.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "query", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorQuery {
    /// The broker itself: its version, and how long it has run. (Braced,
    /// so a field it doesn't have is refused rather than ignored.)
    Broker {},
    /// The cells running now: each one's id, kind, start and limits.
    Cells {},
    /// The API and UI connections open now: each one's socket and when it
    /// was admitted.
    Sessions {},
    /// The broker's recent egress decisions: when, for which cell, where
    /// to, and the verdict.
    Egress {},
}

/// A frame the operator's client receives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperatorResponse {
    /// The versions match: the connection is open.
    Welcome {
        /// The broker's [`UI_SCHEMA_VERSION`].
        schema_version: u16,
    },
    /// The versions don't match; the broker then closes the connection.
    Incompatible {
        /// The client's version.
        frontend: u16,
        /// The broker's version.
        backend: u16,
        /// Which side to update, in a sentence.
        message: String,
    },
    /// The answer to one query.
    Answer {
        /// The query's `request_id`.
        request_id: String,
        /// The answer.
        answer: OperatorAnswer,
    },
}

/// An answer to an [`OperatorQuery`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum OperatorAnswer {
    /// [`OperatorQuery::Broker`].
    Broker {
        /// The broker's version, as its crate names it.
        version: String,
        /// When the broker started, as an RFC 3339 timestamp.
        started_at: String,
        /// Whole seconds it has run.
        uptime_secs: u64,
    },
    /// [`OperatorQuery::Cells`], oldest first.
    Cells {
        /// The cells running.
        cells: Vec<OperatorCell>,
    },
    /// [`OperatorQuery::Sessions`], oldest first.
    Sessions {
        /// The connections open.
        sessions: Vec<OperatorSession>,
    },
    /// [`OperatorQuery::Egress`], oldest first: the most recent ones the
    /// broker keeps.
    Egress {
        /// The decisions.
        decisions: Vec<EgressDecision>,
    },
}

/// One egress decision: never the request's path, headers or body, nor
/// the answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EgressDecision {
    /// When, as an RFC 3339 timestamp.
    pub at: String,
    /// The cell that asked.
    pub cell_id: String,
    /// Where it asked to go, as it named it (the cell's own text, so
    /// bounded, and shown as text).
    pub destination: String,
    /// What the broker decided.
    pub verdict: EgressVerdict,
}

/// What the broker decided about one egress request, and why.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EgressVerdict {
    /// Forwarded, and answered.
    Allowed,
    /// Refused: the cell holds no grant for the destination.
    NoGrant,
    /// Refused: the destination isn't on the operator's allowlist, or
    /// has no credential.
    NotAllowed,
    /// Refused: the request's surrogate was bad, expired, or another
    /// peer's or destination's.
    BadSurrogate,
    /// Refused by the model request policy: it would have the model
    /// fetch for the cell.
    Policy,
    /// Allowed, but the request failed upstream or on the way.
    Failed,
}

/// One open API or UI connection, as an operator sees it: when and where,
/// never what it said.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OperatorSession {
    /// The connection's id, as the activity log names its calls.
    pub connection: String,
    /// The socket it came in on.
    pub socket: SessionSocket,
    /// When it was admitted, as an RFC 3339 timestamp.
    pub opened_at: String,
}

/// The socket a session came in on.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SessionSocket {
    /// `HATCHLING_API_SOCKET`.
    Api,
    /// `HATCHLING_UI_SOCKET`.
    Ui,
}

/// One running cell, as an operator sees it: never what it was asked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OperatorCell {
    /// The id the broker gave it.
    pub cell_id: String,
    /// What it is for.
    pub kind: CellKind,
    /// When it started, as an RFC 3339 timestamp.
    pub started_at: String,
    /// What it is held to.
    pub limits: CellLimits,
}

/// What a cell is for.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    /// The broker's own agent.
    Agent,
    /// A run: a scheduled task's, a hook's, or a chat message's.
    Run,
    /// A run a parent started for part of its task.
    Subagent,
    /// A signed skill.
    Skill,
    /// A coding CLI (a harness).
    Harness,
    /// A browser the agent drives (WO-029).
    Browser,
}

/// The limits a cell is held to; `None` is unlimited, or the broker's own
/// bound. Memory and CPU apply only under a cgroup.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CellLimits {
    /// Processes, threads included.
    pub processes: Option<u64>,
    /// A program's address space, in bytes.
    pub address_space: Option<u64>,
    /// The largest file a program may write, in bytes.
    pub file_size: Option<u64>,
    /// Files a program may hold open at once.
    pub open_files: Option<u64>,
    /// Whole-cell memory, in bytes.
    pub memory: Option<u64>,
    /// CPU, in thousandths of a core.
    pub cpu_millis: Option<u64>,
}

/// The broker's answer to an operator's hello with `frontend` as its
/// version: [`OperatorResponse::Welcome`] when it can serve it, else
/// [`OperatorResponse::Incompatible`].
#[must_use]
pub fn operator_negotiate(frontend: u16) -> OperatorResponse {
    match negotiate(frontend) {
        UiResponse::Incompatible {
            frontend,
            backend,
            message,
        } => OperatorResponse::Incompatible {
            frontend,
            backend,
            message,
        },
        // `negotiate` answers only Welcome or Incompatible.
        UiResponse::Welcome { .. } | UiResponse::Event(_) | UiResponse::ActionResult { .. } => {
            OperatorResponse::Welcome {
                schema_version: UI_SCHEMA_VERSION,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ui::{UiAction, UiRequest};

    #[test]
    fn operator_frames_have_their_wire_shape() {
        let hello = OperatorRequest::Hello {
            schema_version: 4,
            client: String::from("console"),
            token: Some(ApiToken::new(String::from("ab12"))),
        };
        let wire =
            json!({"type": "hello", "schema_version": 4, "client": "console", "token": "ab12"});
        assert_eq!(serde_json::to_value(&hello).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<OperatorRequest>(wire).unwrap(),
            hello
        );
        let query = json!({"type": "query", "request_id": "q1", "query": {"query": "broker"}});
        assert_eq!(
            serde_json::from_value::<OperatorRequest>(query).unwrap(),
            OperatorRequest::Query {
                request_id: String::from("q1"),
                query: OperatorQuery::Broker {},
            }
        );
        let answer = OperatorResponse::Answer {
            request_id: String::from("q1"),
            answer: OperatorAnswer::Broker {
                version: String::from("0.4.0"),
                started_at: String::from("2026-10-16T09:00:00Z"),
                uptime_secs: 61,
            },
        };
        assert_eq!(
            serde_json::to_value(&answer).unwrap(),
            json!({"type": "answer", "request_id": "q1", "answer": {
                "answer": "broker", "version": "0.4.0",
                "started_at": "2026-10-16T09:00:00Z", "uptime_secs": 61}})
        );
    }

    #[test]
    fn a_cell_has_its_wire_shape() {
        let cells = OperatorResponse::Answer {
            request_id: String::from("q2"),
            answer: OperatorAnswer::Cells {
                cells: vec![OperatorCell {
                    cell_id: String::from("run-1f2e"),
                    kind: CellKind::Subagent,
                    started_at: String::from("2026-10-16T09:00:00Z"),
                    limits: CellLimits {
                        processes: Some(4096),
                        memory: Some(1 << 30),
                        ..CellLimits::default()
                    },
                }],
            },
        };
        let wire = json!({"type": "answer", "request_id": "q2", "answer": {"answer": "cells", "cells": [{
            "cell_id": "run-1f2e", "kind": "subagent", "started_at": "2026-10-16T09:00:00Z",
            "limits": {"processes": 4096, "address_space": null, "file_size": null,
                       "open_files": null, "memory": 1_073_741_824_u64, "cpu_millis": null}}]}});
        assert_eq!(serde_json::to_value(&cells).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<OperatorResponse>(wire).unwrap(),
            cells
        );
        assert_eq!(
            serde_json::from_value::<OperatorRequest>(
                json!({"type": "query", "request_id": "q2", "query": {"query": "cells"}})
            )
            .unwrap(),
            OperatorRequest::Query {
                request_id: String::from("q2"),
                query: OperatorQuery::Cells {},
            }
        );
        for (kind, name) in [
            (CellKind::Agent, "agent"),
            (CellKind::Run, "run"),
            (CellKind::Subagent, "subagent"),
            (CellKind::Skill, "skill"),
            (CellKind::Harness, "harness"),
            (CellKind::Browser, "browser"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
        }
    }

    #[test]
    fn a_session_has_its_wire_shape() {
        let sessions = OperatorAnswer::Sessions {
            sessions: vec![OperatorSession {
                connection: String::from("api-0f3c"),
                socket: SessionSocket::Ui,
                opened_at: String::from("2026-10-16T09:00:00Z"),
            }],
        };
        let wire = json!({"answer": "sessions", "sessions": [{
            "connection": "api-0f3c", "socket": "ui", "opened_at": "2026-10-16T09:00:00Z"}]});
        assert_eq!(serde_json::to_value(&sessions).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<OperatorAnswer>(wire).unwrap(),
            sessions
        );
        assert_eq!(
            serde_json::to_value(SessionSocket::Api).unwrap(),
            json!("api")
        );
        assert_eq!(
            serde_json::from_value::<OperatorQuery>(json!({"query": "sessions"})).unwrap(),
            OperatorQuery::Sessions {}
        );
    }

    #[test]
    fn an_egress_decision_has_its_wire_shape() {
        let decisions = OperatorAnswer::Egress {
            decisions: vec![EgressDecision {
                at: String::from("2026-10-16T09:00:00Z"),
                cell_id: String::from("run-1f2e"),
                destination: String::from("localhost:18089"),
                verdict: EgressVerdict::NoGrant,
            }],
        };
        let wire = json!({"answer": "egress", "decisions": [{"at": "2026-10-16T09:00:00Z",
            "cell_id": "run-1f2e", "destination": "localhost:18089", "verdict": "no_grant"}]});
        assert_eq!(serde_json::to_value(&decisions).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<OperatorAnswer>(wire).unwrap(),
            decisions
        );
        for (verdict, name) in [
            (EgressVerdict::Allowed, "allowed"),
            (EgressVerdict::NoGrant, "no_grant"),
            (EgressVerdict::NotAllowed, "not_allowed"),
            (EgressVerdict::BadSurrogate, "bad_surrogate"),
            (EgressVerdict::Policy, "policy"),
            (EgressVerdict::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(verdict).unwrap(), json!(name));
        }
        assert_eq!(
            serde_json::from_value::<OperatorQuery>(json!({"query": "egress"})).unwrap(),
            OperatorQuery::Egress {}
        );
    }

    #[test]
    fn neither_side_decodes_the_others_requests() {
        let action = serde_json::to_value(UiRequest::Action {
            request_id: String::from("a1"),
            action: UiAction::SendMessage {
                chat_id: String::from("local"),
                text: String::from("hi"),
            },
        })
        .unwrap();
        assert!(serde_json::from_value::<OperatorRequest>(action).is_err());
        let subscribe = json!({"type": "subscribe", "after_seq": null});
        assert!(serde_json::from_value::<OperatorRequest>(subscribe).is_err());
        let query = json!({"type": "query", "request_id": "q1", "query": {"query": "broker"}});
        assert!(serde_json::from_value::<UiRequest>(query).is_err());
        // A field a query doesn't have is refused, not ignored.
        let padded = json!({"type": "query", "request_id": "q1", "query": {"query": "broker", "chat_id": "local"}});
        assert!(serde_json::from_value::<OperatorRequest>(padded).is_err());
    }

    #[test]
    fn an_operator_hello_is_welcomed_only_at_the_brokers_version() {
        assert_eq!(
            operator_negotiate(UI_SCHEMA_VERSION),
            OperatorResponse::Welcome {
                schema_version: UI_SCHEMA_VERSION
            }
        );
        for other in [UI_SCHEMA_VERSION - 1, UI_SCHEMA_VERSION + 1] {
            assert!(matches!(
                operator_negotiate(other),
                OperatorResponse::Incompatible { frontend, backend, ref message }
                    if frontend == other && backend == UI_SCHEMA_VERSION && !message.is_empty()
            ));
        }
    }
}
