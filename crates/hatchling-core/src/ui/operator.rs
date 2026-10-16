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
