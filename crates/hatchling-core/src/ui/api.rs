//! The local API's part of the protocol.
//!
//! Hatchling's API speaks this same protocol to external agents, scripts
//! and tools. Two things are its own: the [`ApiToken`] a caller may
//! present in its `Hello`, for a token provider (a hosted deployment's;
//! the local provider identifies callers by their socket instead), and
//! what the activity feed records about each call: the [`ApiRequest`]
//! kind and the [`ApiOutcome`], never the request's content.

use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{UiAction, UiRequest};

/// A bearer token, presented in `Hello` for a token provider.
///
/// Opaque to the protocol. Its `Debug` never shows it, so no log line or
/// panic message can carry one.
// Its memory is wiped when it's dropped; on the wire it's a plain string.
// (A plain comment, so the generated protocol schema doesn't change.)
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ApiToken(#[cfg_attr(feature = "schema", schemars(with = "String"))] Zeroizing<String>);

impl ApiToken {
    /// The token `text`, taken whole: its buffer is the one wiped.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    /// The token's text, for the provider that checks it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiToken(<redacted>)")
    }
}

/// What one API call asked for: the request's kind, never its content
/// (a message's text, a file, a schedule's task).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApiRequest {
    /// Opening the connection.
    Hello,
    /// Starting or resuming the event stream.
    Subscribe,
    /// [`UiAction::SendMessage`].
    SendMessage,
    /// [`UiAction::Decide`].
    Decide,
    /// [`UiAction::ManageSchedule`].
    ManageSchedule,
    /// [`UiAction::ManageSites`].
    ManageSites,
    /// [`UiAction::InvokeSkill`].
    InvokeSkill,
    /// [`UiAction::OpenArtifact`].
    OpenArtifact,
    /// [`UiAction::ShareArtifact`].
    ShareArtifact,
    /// [`UiAction::AttachArtifact`].
    AttachArtifact,
    /// [`UiAction::SaveFile`].
    SaveFile,
    /// [`UiAction::TakeControl`].
    TakeControl,
    /// [`UiAction::ReturnControl`].
    ReturnControl,
    /// [`UiAction::StopBrowser`].
    StopBrowser,
    /// [`UiAction::BrowserInput`].
    BrowserInput,
}

impl ApiRequest {
    /// The kind of `request`.
    #[must_use]
    pub const fn of(request: &UiRequest) -> Self {
        match request {
            UiRequest::Hello { .. } => Self::Hello,
            UiRequest::Subscribe { .. } => Self::Subscribe,
            UiRequest::Action { action, .. } => Self::of_action(action),
        }
    }

    /// The kind of `action`.
    #[must_use]
    pub const fn of_action(action: &UiAction) -> Self {
        match action {
            UiAction::SendMessage { .. } => Self::SendMessage,
            UiAction::Decide { .. } => Self::Decide,
            UiAction::ManageSchedule { .. } => Self::ManageSchedule,
            UiAction::ManageSites { .. } => Self::ManageSites,
            UiAction::InvokeSkill { .. } => Self::InvokeSkill,
            UiAction::OpenArtifact { .. } => Self::OpenArtifact,
            UiAction::ShareArtifact { .. } => Self::ShareArtifact,
            UiAction::AttachArtifact { .. } => Self::AttachArtifact,
            UiAction::SaveFile { .. } => Self::SaveFile,
            UiAction::TakeControl { .. } => Self::TakeControl,
            UiAction::ReturnControl { .. } => Self::ReturnControl,
            UiAction::StopBrowser { .. } => Self::StopBrowser,
            UiAction::BrowserInput { .. } => Self::BrowserInput,
        }
    }
}

/// How the API answered one call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApiOutcome {
    /// The API passed the call to its backend and returned the backend's
    /// answer, whatever that answer was.
    Served,
    /// The API refused the call before any backend saw it: an
    /// unidentified caller, grants that do not cover it, a request out
    /// of order, or an action this backend does not offer.
    Refused,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::messages::{ScheduleOp, SkillOp};
    use crate::ui::{Decision, FileContent, FileFormat};

    #[test]
    fn a_token_never_shows_in_debug_and_is_a_plain_string_on_the_wire() {
        let token = ApiToken::new(String::from("s3cret"));
        assert_eq!(format!("{token:?}"), "ApiToken(<redacted>)");
        assert_eq!(token.expose(), "s3cret");
        assert_eq!(serde_json::to_value(&token).unwrap(), json!("s3cret"));
        assert_eq!(
            serde_json::from_value::<ApiToken>(json!("s3cret")).unwrap(),
            token
        );
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one table row per request kind")]
    fn every_request_names_its_kind() {
        let action = |action: UiAction| UiRequest::Action {
            request_id: String::from("r"),
            action,
        };
        let id = || String::from("x");
        let cases = [
            (
                UiRequest::Hello {
                    schema_version: 3,
                    client: id(),
                    token: None,
                },
                ApiRequest::Hello,
            ),
            (
                UiRequest::Subscribe { after_seq: None },
                ApiRequest::Subscribe,
            ),
            (
                action(UiAction::SendMessage {
                    chat_id: id(),
                    text: id(),
                }),
                ApiRequest::SendMessage,
            ),
            (
                action(UiAction::Decide {
                    confirmation_id: id(),
                    revision: 1,
                    digest: id(),
                    decision: Decision::Approve,
                }),
                ApiRequest::Decide,
            ),
            (
                action(UiAction::ManageSchedule {
                    op: ScheduleOp::Cancel { id: id() },
                }),
                ApiRequest::ManageSchedule,
            ),
            (
                action(UiAction::InvokeSkill {
                    op: SkillOp::Invoke {
                        skill: id(),
                        capability: "read".parse().unwrap(),
                        input: String::new(),
                    },
                }),
                ApiRequest::InvokeSkill,
            ),
            (
                action(UiAction::OpenArtifact {
                    artifact_id: id(),
                    revision: 1,
                }),
                ApiRequest::OpenArtifact,
            ),
            (
                action(UiAction::ShareArtifact {
                    artifact_id: id(),
                    revision: 1,
                }),
                ApiRequest::ShareArtifact,
            ),
            (
                action(UiAction::AttachArtifact {
                    chat_id: id(),
                    artifact_id: id(),
                    revision: 1,
                }),
                ApiRequest::AttachArtifact,
            ),
            (
                action(UiAction::SaveFile {
                    chat_id: None,
                    name: id(),
                    format: FileFormat::Text,
                    content: FileContent::Text { text: id() },
                }),
                ApiRequest::SaveFile,
            ),
            (
                action(UiAction::TakeControl { session_id: id() }),
                ApiRequest::TakeControl,
            ),
            (
                action(UiAction::ReturnControl {
                    session_id: id(),
                    frame_sequence: 1,
                }),
                ApiRequest::ReturnControl,
            ),
            (
                action(UiAction::StopBrowser { session_id: id() }),
                ApiRequest::StopBrowser,
            ),
            (
                action(UiAction::BrowserInput {
                    session_id: id(),
                    input: crate::browser::OwnerInput::Scroll { down: true },
                }),
                ApiRequest::BrowserInput,
            ),
        ];
        for (request, kind) in cases {
            assert_eq!(ApiRequest::of(&request), kind, "{request:?}");
        }
        assert_eq!(
            ApiRequest::of_action(&UiAction::StopBrowser { session_id: id() }),
            ApiRequest::StopBrowser
        );
        assert_eq!(
            serde_json::to_value(ApiRequest::ManageSchedule).unwrap(),
            json!("manage_schedule")
        );
        assert_eq!(
            serde_json::to_value(ApiOutcome::Refused).unwrap(),
            json!("refused")
        );
    }
}
