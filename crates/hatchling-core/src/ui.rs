//! The frontend wire protocol: the frames the UI receives and
//! the actions it sends.
//!
//! One schema, two generated sides. These types, with every session
//! type in [`crate::messages`] and [`crate::subagent`], are the source
//! of truth: `hatchling-dev protocol-schema` derives
//! `frontend/protocol/schema.json` from them, and the frontend
//! generates its TypeScript types from that file. Editing either
//! output by hand fails a drift check on its side.
//!
//! # Versioning
//!
//! [`UI_SCHEMA_VERSION`] versions this protocol. A breaking change
//! bumps it, and the two ends must match exactly. A mismatch is a
//! user-facing event, not a bare refusal: [`negotiate`] answers with
//! [`UiResponse::Incompatible`] carrying a sentence a person can act
//! on ([`incompatibility_message`]), and the frontend builds the same
//! sentence from the same templates when it is the newer side.
//!
//! # Replay
//!
//! Every event carries a sequence number, strictly increasing within
//! a session's stream. The client keeps the highest number it has
//! applied and, on reconnect, resumes by sending
//! [`UiRequest::Subscribe`] with that number as `after_seq`. Events are idempotent: an
//! event at or below the client's mark is a duplicate and is dropped.
//! A confirmation is decided at most once: once the client has seen
//! [`UiEventBody::ConfirmationDecided`] for it, replaying its request
//! never presents it again.
//!
//! # Interactive surfaces (v2)
//!
//! The agent's status, artifacts and files, and the live browser, in
//! [`surfaces`]; who drives a shared surface, in [`control`]. A browser
//! session's control moves only by [`UiEventBody::BrowserControl`]
//! handoffs that [`Control::after`] allows.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::PROTOCOL_VERSION;
use crate::messages::{
    BuildFailure, DeliveryRefusal, ScheduleOp, ScheduleResult, SkillOp, SkillResult,
};
use crate::subagent::{ActivityEvent, ChildVerdict, HarnessOutcome, SubagentEvent};

pub mod api;
pub mod control;
pub mod operator;
pub mod surfaces;

pub use api::{ApiOutcome, ApiRequest, ApiToken};
pub use control::{Actor, Control, Handoff};
pub use surfaces::{
    AgentState, ArtifactBody, ArtifactRefusal, ArtifactRevision, BrowserAction, BrowserFrame,
    BrowserSession, BrowserState, ControlRefusal, FileContent, FileFormat, FileRefusal, ImageType,
    InlineImage, UiArtifact,
};

/// The frontend protocol's version. A breaking change bumps it.
///
/// 2: the interactive surfaces.
/// 3: the local API: `Hello`'s token, the `not_granted`
/// refusal, and `api_call` activity.
/// 4: coding-harness runs: `harness_run` activity.
pub const UI_SCHEMA_VERSION: u16 = 4;

/// The incompatibility sentence when the frontend is newer than the
/// backend. `{frontend}` and `{backend}` are replaced by the versions.
pub const FRONTEND_NEWER: &str = "frontend v{frontend} needs broker \u{2265} v{frontend}; broker is v{backend} \u{2014} restart the broker to update";

/// The incompatibility sentence when the backend is newer than the
/// frontend. `{frontend}` and `{backend}` are replaced by the versions.
pub const BACKEND_NEWER: &str = "broker v{backend} needs frontend \u{2265} v{backend}; frontend is v{frontend} \u{2014} reload the frontend to update";

/// The placeholder in the templates for the frontend's version.
const FRONTEND_SLOT: &str = "{frontend}";

/// The placeholder in the templates for the backend's version.
const BACKEND_SLOT: &str = "{backend}";

/// A frame the UI sends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiRequest {
    /// The first frame on a connection: the UI's protocol version.
    Hello {
        /// The UI's [`UI_SCHEMA_VERSION`].
        schema_version: u16,
        /// Who is connecting, for the broker's log.
        client: String,
        /// A token for a token provider (a hosted deployment's). The
        /// local API identifies its callers by their socket and ignores
        /// it; a UI sends `null`.
        token: Option<ApiToken>,
    },
    /// Start the event stream, or resume it after a reconnect.
    Subscribe {
        /// The highest sequence number the client has applied, or
        /// `None` to start from the oldest retained event.
        after_seq: Option<u64>,
    },
    /// A user action, answered by one [`UiResponse::ActionResult`]
    /// carrying the same `request_id`.
    Action {
        /// The client's id for this request.
        request_id: String,
        /// What the user asked for.
        action: UiAction,
    },
}

/// A frame the UI receives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiResponse {
    /// The versions match: the connection is open.
    Welcome {
        /// The backend's [`UI_SCHEMA_VERSION`].
        schema_version: u16,
        /// The backend's session protocol version
        /// ([`PROTOCOL_VERSION`]).
        session_protocol: u16,
        /// The grants this connection holds: what a schedule it creates
        /// may be given, and what the app shows such a schedule may do.
        /// Empty from an older broker, and left off the wire when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        grants: Vec<crate::ScheduleGrant>,
    },
    /// The versions differ. The connection is refused, and `message`
    /// says what to do about it.
    Incompatible {
        /// The UI's version.
        frontend: u16,
        /// The backend's version.
        backend: u16,
        /// A sentence for the person at the UI.
        message: String,
    },
    /// One event on the stream.
    Event(UiEvent),
    /// The answer to one [`UiRequest::Action`].
    ActionResult {
        /// The `request_id` of the action answered.
        request_id: String,
        /// What happened.
        result: UiActionResult,
    },
}

/// One event on the UI's stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UiEvent {
    /// Strictly increasing within a session's stream.
    pub seq: u64,
    /// When the backend emitted it, RFC 3339.
    pub ts: String,
    /// What happened.
    pub body: UiEventBody,
}

impl UiEvent {
    /// An activity event at `seq`, with its state derived by
    /// [`activity_state`] and its time taken from the event.
    #[must_use]
    pub fn activity(seq: u64, event: ActivityEvent) -> Self {
        let state = activity_state(&event.kind);
        Self {
            seq,
            ts: event.ts.clone(),
            body: UiEventBody::Activity { event, state },
        }
    }
}

/// What an event is about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiEventBody {
    /// An activity-monitor event, with its state for the feed.
    Activity {
        /// The event, as the activity-monitor contract defines it.
        event: ActivityEvent,
        /// The row state the feed shows for it.
        state: ActivityState,
    },
    /// A chat message.
    ChatMessage(ChatMessage),
    /// A build or delivery changed state.
    Artifact(ArtifactStatus),
    /// A schedule fired, finished, or was cancelled.
    Schedule(ScheduleNotice),
    /// The agent asks the user to approve or deny something.
    Confirmation(ConfirmationRequest),
    /// A confirmation was decided. It is never presented again.
    ConfirmationDecided {
        /// The confirmation decided.
        confirmation_id: String,
        /// The revision the decision applied to.
        revision: u64,
        /// The decision.
        decision: Decision,
    },
    /// What the agent is doing now (v2).
    AgentStatus {
        /// The state.
        state: AgentState,
        /// What it is about, in the user's words. Untrusted text.
        detail: String,
    },
    /// An artifact was listed or changed: its whole record (v2).
    ArtifactListed(UiArtifact),
    /// A file was sent in a chat, as an artifact revision (v2).
    Attachment {
        /// The chat.
        chat_id: String,
        /// Stable across replays.
        message_id: String,
        /// Who sent it.
        author: ChatAuthor,
        /// The artifact.
        artifact_id: String,
        /// Its revision.
        revision: u32,
        /// What was said with it. Untrusted text.
        text: String,
    },
    /// A browser session appeared or changed (v2).
    BrowserSession(BrowserSession),
    /// A new snapshot of a browser session's page (v2).
    BrowserFrame(BrowserFrame),
    /// One action on a browser session's page, for its log (v2).
    BrowserAction(BrowserAction),
    /// A browser session's control moved by one handoff (v2).
    BrowserControl {
        /// The session.
        session_id: String,
        /// The handoff.
        handoff: Handoff,
        /// The newest frame of the page when it happened. On
        /// [`Handoff::Resumed`] it is a frame the agent captured after
        /// the user handed back: the agent looked again before acting.
        frame_sequence: u64,
    },
    /// A browser session's page asks for a one-time code, or the request
    /// moved on (WO-030).
    BrowserCode {
        /// The session.
        session_id: String,
        /// The request.
        request_id: String,
        /// The origin the code is for, `https://host[:port]`.
        origin: String,
        /// Where it stands.
        state: CodeState,
    },
}

/// The state the activity feed shows for an event. `Interrupted` is a
/// killed child, distinct from `Failed`: a killed child did not fail.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    /// Still running.
    Pending,
    /// Finished and completed its brief.
    Success,
    /// Finished with only part of its brief done.
    Partial,
    /// Died or timed out on its own.
    Failed,
    /// Killed by its parent.
    Interrupted,
}

/// The feed state for an activity event.
///
/// Per the activity-monitor contract: spawned and progress are pending,
/// a finish is its verdict, a kill is interrupted, and a failure is
/// failed. An API call the API served is a success, and one it refused
/// is failed. A run's summary is a record of a session that ended: a
/// success, whatever its tools' endings.
#[must_use]
pub const fn activity_state(event: &SubagentEvent) -> ActivityState {
    match event {
        SubagentEvent::Spawned { .. } | SubagentEvent::Progress { .. } => ActivityState::Pending,
        SubagentEvent::Finished {
            verdict: ChildVerdict::Ok,
            ..
        }
        | SubagentEvent::ApiCall {
            outcome: ApiOutcome::Served,
            ..
        }
        | SubagentEvent::HarnessRun {
            outcome: HarnessOutcome::Done,
            ..
        }
        | SubagentEvent::RunSummary { .. } => ActivityState::Success,
        SubagentEvent::Finished {
            verdict: ChildVerdict::Partial,
            ..
        }
        | SubagentEvent::HarnessRun {
            outcome: HarnessOutcome::NeedsClarification,
            ..
        } => ActivityState::Partial,
        SubagentEvent::Killed { .. } => ActivityState::Interrupted,
        SubagentEvent::Failed { .. }
        | SubagentEvent::ApiCall {
            outcome: ApiOutcome::Refused,
            ..
        }
        | SubagentEvent::HarnessRun {
            outcome: HarnessOutcome::Failed | HarnessOutcome::TimedOut | HarnessOutcome::Refused,
            ..
        } => ActivityState::Failed,
    }
}

/// One chat message. Its time is its event's `ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChatMessage {
    /// The chat it belongs to.
    pub chat_id: String,
    /// Stable across replays.
    pub message_id: String,
    /// Who wrote it.
    pub author: ChatAuthor,
    /// Plain text. The UI renders it as text, never as markup.
    pub text: String,
    /// The run that wrote it, by its cell id, when an agent run did: the
    /// broker's own word, which the activity record's run summary names
    /// too. Never read from the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

/// Who wrote a chat message.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChatAuthor {
    /// The person at the UI.
    User,
    /// The agent.
    Agent,
    /// Hatchling itself (notices, not the agent's words).
    System,
}

/// A build or delivery state change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ArtifactStatus {
    /// A build started.
    Building {
        /// The build.
        build_id: String,
        /// The builder kind (`"document"`, `"page"`, ...).
        build_kind: String,
    },
    /// A build finished.
    Built {
        /// The build.
        build_id: String,
        /// The builder kind.
        build_kind: String,
    },
    /// A build failed, for a typed reason.
    Failed {
        /// The build.
        build_id: String,
        /// The builder kind.
        build_kind: String,
        /// Why.
        failure: BuildFailure,
    },
    /// A build was delivered.
    Delivered {
        /// The build.
        build_id: String,
    },
    /// A delivery was refused, for a typed reason.
    Refused {
        /// The build.
        build_id: String,
        /// Why.
        refusal: DeliveryRefusal,
    },
}

/// A schedule's state change. Its time is its event's `ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ScheduleNotice {
    /// A scheduled run started.
    Fired {
        /// The schedule.
        schedule_id: String,
    },
    /// A scheduled run finished.
    Completed {
        /// The schedule.
        schedule_id: String,
    },
    /// A scheduled run failed.
    Failed {
        /// The schedule.
        schedule_id: String,
        /// Redacted detail.
        detail: String,
    },
    /// The schedule was cancelled.
    Cancelled {
        /// The schedule.
        schedule_id: String,
    },
}

/// The agent asks the user to approve or deny something.
///
/// What is asked is the broker's record of one frozen action (WO-027): its
/// digest, and its fields in the order the owner reviews them, the broker's
/// first and the agent's after, each labeled with who stated it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfirmationRequest {
    /// Stable across replays.
    pub confirmation_id: String,
    /// Bumped when the request changes. A decision names the revision
    /// it applies to, and a stale one is rejected.
    pub revision: u64,
    /// The chat it belongs to, if any.
    pub chat_id: Option<String>,
    /// What is being asked, as plain text: the broker's name for the
    /// operation, never the agent's words.
    pub summary: String,
    /// When the request lapses, RFC 3339, if it does.
    pub expires_at: Option<String>,
    /// The digest of the exact action the approval would run. A decision
    /// names it.
    pub digest: String,
    /// The action's fields, in review order.
    pub fields: Vec<ConfirmationField>,
    /// Whether it can be approved for the rest of the session
    /// ([`Decision::ApproveSession`]) as well as once: a site the agent
    /// asks to read (WO-028).
    #[serde(default)]
    pub session: bool,
    /// Whether it can be approved always ([`Decision::ApproveAlways`]): a
    /// site, where the broker keeps the sites the owner allows.
    #[serde(default)]
    pub always: bool,
    /// What approving it does, in a few words, when that's more than
    /// allowing it once: "Remember it" for something the agent asks to
    /// keep. The broker's, never the agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approve: Option<String>,
}

/// The owner's Feed page as a schedule's delivery target: a run delivering
/// here is kept as a post on the page, and goes nowhere else. Only the owner
/// may point a schedule at it.
pub const FEED_PAGE: &str = "feed";

/// The owner's Ideas page as a schedule's delivery target: a run for it
/// suggests ideas there, and its answer goes nowhere. Only the owner may
/// point a schedule at it.
pub const IDEAS_PAGE: &str = "ideas";

/// Where a suggestion on the Ideas page stands.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SuggestionState {
    /// Waiting for the owner.
    Suggested,
    /// The owner started it: their chat box was filled with it.
    Started,
    /// The owner set it aside.
    Dismissed,
}

/// A suggestion on the owner's Ideas page: the agent's words, all of it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Suggestion {
    /// Its id.
    pub suggestion_id: String,
    /// Its title.
    pub title: String,
    /// What it would do.
    pub summary: String,
    /// Why it would help them, as the agent said.
    pub why: String,
    /// Where it would help.
    pub area: crate::messages::SuggestionArea,
    /// When it was suggested, RFC 3339.
    pub suggested_at: String,
    /// Where it stands.
    pub state: SuggestionState,
}

/// An operation on the owner's Ideas page, from their app.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SuggestionOp {
    /// List the suggestions, the newest first.
    List,
    /// Mark one started: the owner took it to their chat.
    Start {
        /// It, as listed.
        id: String,
    },
    /// Set one aside.
    Dismiss {
        /// It, as listed.
        id: String,
    },
    /// Bring one set aside back.
    Restore {
        /// It, as listed.
        id: String,
    },
}

/// The result of a [`SuggestionOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SuggestionResult {
    /// The suggestions, the newest first.
    Listed {
        /// They.
        suggestions: Vec<Suggestion>,
    },
    /// It was changed: as it now stands.
    Changed {
        /// It.
        suggestion: Suggestion,
    },
    /// No suggestion has the id.
    NotFound,
    /// The broker keeps no Ideas page: scheduled work is off.
    Off,
    /// The broker couldn't read or change the page; its log says why.
    Failed,
}

/// A post on the owner's Feed page: what one scheduled run wrote for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FeedPost {
    /// Its id.
    pub post_id: String,
    /// When it was kept, RFC 3339.
    pub written_at: String,
    /// What the run wrote, as the output gate cleared it: the agent's words,
    /// its first line the title.
    pub text: String,
}

/// A decision the owner made, kept: the card as they were shown it, and what
/// they decided.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DecisionRecord {
    /// The confirmation.
    pub confirmation_id: String,
    /// What was asked, the broker's words.
    pub summary: String,
    /// Its fields, in review order, each labelled with who stated it.
    pub fields: Vec<ConfirmationField>,
    /// What they decided.
    pub decision: Decision,
    /// When, RFC 3339.
    pub decided_at: String,
}

/// One field of what a confirmation asks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfirmationField {
    /// What the field is, in the broker's words.
    pub label: String,
    /// Its value, as plain text, whole: never cut short.
    pub value: String,
    /// Who stated the value.
    pub source: FieldSource,
}

/// Who stated a confirmation field's value.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    /// The broker: it resolved or checked the value itself.
    Broker,
    /// The agent: words it wrote, which the action will use as they are.
    Agent,
}

/// A user's decision on a confirmation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Go ahead.
    Approve,
    /// Go ahead, and for the rest of the session: offered only where the
    /// request says so ([`ConfirmationRequest::session`]).
    ApproveSession,
    /// Go ahead, and always, until the owner revokes it: offered only where
    /// the request says so ([`ConfirmationRequest::always`]).
    ApproveAlways,
    /// Do not.
    Deny,
}

/// An operation on the sites the agent is always allowed to read (WO-028).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SiteOp {
    /// List them.
    List,
    /// Revoke one: the next read of a page on it asks again.
    Revoke {
        /// The site, `https://host[:port]`, as listed.
        site: String,
    },
}

/// A site the agent is always allowed to read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AllowedSite {
    /// The site, `https://host[:port]`.
    pub site: String,
    /// When the owner allowed it, RFC 3339.
    pub allowed_at: String,
}

/// The result of a [`SiteOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SiteResult {
    /// The sites, in the order they were allowed.
    Listed {
        /// The sites.
        sites: Vec<AllowedSite>,
    },
    /// The site was revoked.
    Revoked,
    /// No such site is allowed.
    NotFound,
    /// The broker keeps no sites: the operator hasn't given it a place to.
    Off,
    /// The broker couldn't read or change them; its log says why.
    Failed,
}

/// The model usage the broker counted, as the model server reported it
/// with each reply: never what the agent says it used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageReport {
    /// Whether the broker counts usage: it does wherever scheduling is on,
    /// which every chat and run needs.
    pub counted: bool,
    /// Each of the last seven days on this machine's clock, today first,
    /// days without a call included.
    pub days: Vec<UsageDay>,
    /// Each model's use over those days, the most tokens first.
    pub models: Vec<UsageModel>,
    /// The limits the owner set.
    pub limits: UsageLimits,
    /// The tokens counted against those limits.
    pub spent: UsageSpent,
}

/// The most model tokens the owner allows, read and written together.
/// Past one, the broker refuses every model call until it resets: a day
/// at midnight, a month on the 1st, on this machine's clock.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UsageLimits {
    /// The most tokens in a day; none when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_tokens: Option<u64>,
    /// The most tokens in a calendar month; none when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_tokens: Option<u64>,
}

/// The model Hatchling uses, and the models its server has: what the
/// owner chooses from in the app.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelState {
    /// The model's server, as the operator set it; none when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// The model the operator set (`HATCHLING_SCHEDULER_MODEL_NAME`):
    /// the one used unless the owner chose another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured: Option<String>,
    /// The model the owner chose in the app, used over the configured
    /// one; none when they haven't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<String>,
    /// The models the server has, by their ids, when it lists them: a
    /// local server answering in plain HTTP does. None when it doesn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<Vec<String>>,
    /// The window of the model in use, in tokens: as its server says,
    /// when it's loaded, else as the operator declared it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    /// All the model in use takes, in bytes, when it's loaded and its
    /// server says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// How much of that runs on the CPU, more slowly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_cpu: Option<u64>,
    /// The other server the operator set up, that the owner may switch
    /// to: none when there's one. With [`ModelState::switched`], the
    /// operator's first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<ModelServer>,
    /// Whether the owner switched to the operator's other server: the
    /// endpoint and the configured model above are its.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub switched: bool,
}

/// A model's server the operator set up, and the model they set on it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelServer {
    /// The API's base URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// The model's id on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// How the vault opens.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum VaultTier {
    /// With a key file on this machine, by itself after a restart.
    File,
    /// With a code from an authenticator app.
    Totp,
    /// With a security key.
    HardwareKey,
}

/// Who can open the vault: how, and how many. Never a key's name or id,
/// and never anything that opens it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VaultProtection {
    /// How it opens.
    pub tier: VaultTier,
    /// The security keys that open it.
    pub keys: u32,
    /// The backup codes left.
    pub codes: u32,
    /// Whether the owner never confirmed saving those codes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub codes_unconfirmed: bool,
}

/// What's set up on this machine, for the app's checklist.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SetupState {
    /// Who can open the vault.
    pub vault: VaultProtection,
    /// When the last backup was made, in Unix seconds, as of this start of
    /// the broker: making one restarts it. None when there's been none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_backup: Option<u64>,
    /// Whether the Windows setup made this install, so the checklist points
    /// to the Start menu rather than to a Linux shell.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub windows_setup: bool,
}

/// The tokens counted toward the owner's limits, read and written
/// together.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageSpent {
    /// Today, on this machine's clock.
    pub today: u64,
    /// This calendar month, today included.
    pub month: u64,
    /// The month, `YYYY-MM`.
    pub month_of: String,
}

/// One day's model usage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageDay {
    /// The day, `YYYY-MM-DD` on this machine's clock.
    pub date: String,
    /// Model calls answered.
    pub calls: u32,
    /// Tokens the model read: prompts, history and tool results.
    pub input_tokens: u64,
    /// Tokens the model wrote, thinking included.
    pub output_tokens: u64,
    /// Calls whose reply reported no usage, so their tokens aren't in the
    /// counts.
    pub unreported: u32,
}

/// One model's usage over the report's days.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageModel {
    /// The model, as its server named it in replies.
    pub model: String,
    /// Model calls answered.
    pub calls: u32,
    /// Tokens the model read.
    pub input_tokens: u64,
    /// Tokens the model wrote.
    pub output_tokens: u64,
}

/// An operation on the owner's goals, from their app: the same goals the
/// agent reads in a chat and a feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GoalOp {
    /// List them, open and closed.
    List,
    /// Set a new goal.
    Create {
        /// One line.
        title: String,
        /// What reaching it looks like; may be empty.
        detail: String,
    },
    /// Change an open goal's title or detail.
    Update {
        /// The goal.
        id: String,
        /// A new title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// A new detail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Close an open goal: it's kept, and can't be reopened.
    Close {
        /// The goal.
        id: String,
        /// Whether it was reached.
        achieved: bool,
    },
}

/// Where a goal stands.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GoalState {
    /// Being worked toward.
    Open,
    /// Reached.
    Achieved,
    /// Set aside.
    Abandoned,
}

/// One of the owner's goals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GoalView {
    /// Its id.
    pub id: String,
    /// One line.
    pub title: String,
    /// What reaching it looks like; may be empty.
    pub detail: String,
    /// Where it stands.
    pub state: GoalState,
    /// When it was set, Unix seconds.
    pub created_at: u64,
    /// When it last changed, Unix seconds: its close, once closed.
    pub updated_at: u64,
}

/// The result of a [`GoalOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum GoalResult {
    /// Every goal, open first, the newest first within each.
    Listed {
        /// The goals.
        goals: Vec<GoalView>,
    },
    /// The goal, as it stands now: set, changed or closed.
    Saved {
        /// The goal.
        goal: GoalView,
    },
    /// No goal has the id.
    NotFound,
    /// The goal is closed: it can't be changed or closed again.
    AlreadyClosed,
    /// The title or detail wasn't accepted: a title is one line, and each
    /// has a length it may not pass.
    Rejected,
    /// The most goals one may keep are kept.
    LimitReached,
    /// The broker keeps no goals: the operator hasn't given it a place to.
    Off,
    /// The broker couldn't read or change them; its log says why.
    Failed,
}

/// An operation on what the agent is given from the owner's memory, from
/// their app.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum KeptOp {
    /// List it.
    List,
    /// Forget one: later runs aren't given it.
    Forget {
        /// It, as listed.
        id: String,
    },
}

/// One thing the agent is given from the owner's memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Kept {
    /// Its id.
    pub id: String,
    /// The words kept.
    pub content: String,
    /// When it was kept, Unix seconds.
    pub kept_at: u64,
    /// Whether the owner approved it: what the agent asked to keep, and
    /// they approved, is; anything kept some other way isn't.
    pub approved: bool,
}

/// The result of a [`KeptOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum KeptResult {
    /// Everything the agent is given, the newest first.
    Listed {
        /// It.
        kept: Vec<Kept>,
    },
    /// It was forgotten.
    Forgotten,
    /// Nothing kept has the id.
    NotFound,
    /// The broker keeps no memory: the operator hasn't given it a place to.
    Off,
    /// The broker couldn't read or change it; its log says why.
    Failed,
}

/// An operation on the owner's signed-in profiles (WO-030).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ProfileOp {
    /// List them.
    List,
    /// Sign one out: its kept session is dropped, its sites kept.
    SignOut {
        /// The profile, as listed.
        profile_id: String,
    },
    /// Delete one: its kept session and its sites.
    Delete {
        /// The profile, as listed.
        profile_id: String,
    },
}

/// A signed-in profile: a site the owner signed in to, with the sites its
/// sign-in went through.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SignedInProfile {
    /// Its id.
    pub profile_id: String,
    /// Its sites, `host:port`, the one signed in on first.
    pub sites: Vec<String>,
    /// Whether a session is kept for it: false once it's signed out.
    pub signed_in: bool,
    /// When it was made, RFC 3339.
    pub created_at: String,
    /// When its session was last kept, RFC 3339.
    pub last_used: String,
}

/// The result of a [`ProfileOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ProfileResult {
    /// The profiles, in the order they were made.
    Listed {
        /// The profiles.
        profiles: Vec<SignedInProfile>,
    },
    /// The profile was signed out.
    SignedOut,
    /// The profile was deleted.
    Deleted,
    /// No such profile.
    NotFound,
    /// The broker keeps no profiles: the operator hasn't given it a place
    /// to.
    Off,
    /// The broker couldn't read or change them; its log says why.
    Failed,
}

/// A secret the user types into the app for the broker to keep (WO-030):
/// a site's username or password. It travels as a plain string and is
/// never shown: its `Debug` hides it, and it's wiped when dropped.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct Concealed(String);

impl Concealed {
    /// `secret`, concealed.
    #[must_use]
    pub const fn new(secret: String) -> Self {
        Self(secret)
    }

    /// `bytes` as a secret, when they're text: `None` otherwise, and they
    /// are wiped either way.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        match String::from_utf8(bytes) {
            Ok(secret) => Some(Self(secret)),
            Err(error) => {
                zeroize::Zeroize::zeroize(&mut error.into_bytes());
                None
            }
        }
    }

    /// The secret itself: for the broker's store, and nothing else.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Concealed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Concealed(..)")
    }
}

impl Drop for Concealed {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

/// An operation on the sign-ins the broker fills for a signed-in profile
/// (WO-030): the owner's alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SignInOp {
    /// List them, by label: never a username or a password.
    List,
    /// Keep a sign-in for one profile and one exact origin.
    Store {
        /// The profile it signs in, as listed.
        profile_id: String,
        /// The exact origin it's filled on, `https://host[:port]`: one of
        /// the profile's sites.
        origin: String,
        /// What the owner calls it.
        label: String,
        /// The username.
        username: Concealed,
        /// The password.
        password: Concealed,
    },
    /// Delete one.
    Delete {
        /// The sign-in, as listed.
        sign_in_id: String,
    },
}

/// A sign-in the broker keeps, as the owner's app is told it: its label and
/// where it's filled, never its username or password.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SiteSignIn {
    /// Its id.
    pub sign_in_id: String,
    /// The profile it signs in.
    pub profile_id: String,
    /// The exact origin it's filled on.
    pub origin: String,
    /// What the owner calls it.
    pub label: String,
    /// When it was kept, RFC 3339.
    pub created_at: String,
}

/// Why a sign-in wasn't kept.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SignInRefusal {
    /// No such profile.
    NoProfile,
    /// The origin isn't an `https` origin alone: a path, a query, a user,
    /// or not `https`.
    BadOrigin,
    /// The origin isn't one of the profile's sites.
    NotTheProfiles,
    /// The label is empty, or has a control character.
    BadLabel,
    /// The password is empty.
    NoPassword,
    /// The label, the username or the password is past its bound.
    TooLong,
    /// The broker keeps as many sign-ins as it may: delete one first.
    Full,
}

/// The result of a [`SignInOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SignInResult {
    /// The sign-ins, in the order they were kept.
    Listed {
        /// The sign-ins.
        sign_ins: Vec<SiteSignIn>,
    },
    /// The sign-in was kept.
    Stored {
        /// The sign-in, as listed.
        sign_in: SiteSignIn,
    },
    /// The sign-in was deleted.
    Deleted,
    /// No such sign-in.
    NotFound,
    /// The sign-in wasn't kept, and why.
    Refused {
        /// Why.
        reason: SignInRefusal,
    },
    /// The broker keeps no sign-ins: the operator hasn't given it a place
    /// to.
    Off,
    /// The broker couldn't read or change them; its log says why.
    Failed,
}

/// Where a browser's one-time code stands (WO-030).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CodeState {
    /// The page asks for it, and the owner is asked.
    Asked,
    /// It was typed into the page, and forgotten.
    Filled,
    /// The owner declined.
    Declined,
    /// Nobody answered in time.
    Expired,
}

/// Why a code the user gave wasn't taken.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CodeRefusal {
    /// No such request is waiting: it was answered, it expired, or the
    /// browser asked for another since.
    NoRequest,
    /// The code is empty.
    Empty,
    /// The code is past its bound.
    TooLong,
}

/// A user action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum UiAction {
    /// Send a chat message.
    SendMessage {
        /// The chat.
        chat_id: String,
        /// Plain text.
        text: String,
    },
    /// Start a new conversation in a chat (WO-026): the agent is given
    /// nothing said before it.
    NewConversation {
        /// The chat.
        chat_id: String,
    },
    /// Approve or deny a confirmation, at the revision and the action
    /// digest the user saw.
    Decide {
        /// The confirmation.
        confirmation_id: String,
        /// The revision the user saw.
        revision: u64,
        /// The digest of the action the user saw.
        digest: String,
        /// The decision.
        decision: Decision,
    },
    /// List or revoke the sites the agent is always allowed to read
    /// (WO-028).
    ManageSites {
        /// The operation.
        op: SiteOp,
    },
    /// Create, cancel, or follow up a schedule.
    ManageSchedule {
        /// The schedule operation, as the session protocol defines it.
        op: ScheduleOp,
    },
    /// Invoke a skill.
    InvokeSkill {
        /// The skill operation, as the session protocol defines it.
        op: SkillOp,
    },
    /// Fetch one revision of an artifact to show it (v2).
    OpenArtifact {
        /// The artifact.
        artifact_id: String,
        /// The revision.
        revision: u32,
    },
    /// Share one revision of an artifact, through the share-review gate
    /// (v2).
    ShareArtifact {
        /// The artifact.
        artifact_id: String,
        /// The revision the user reviewed.
        revision: u32,
    },
    /// Send one revision of an artifact in a chat (v2).
    AttachArtifact {
        /// The chat.
        chat_id: String,
        /// The artifact.
        artifact_id: String,
        /// The revision.
        revision: u32,
    },
    /// Save a file from the user's device as a new artifact (v2).
    SaveFile {
        /// The chat it was added in, if any.
        chat_id: Option<String>,
        /// Its name. Untrusted text.
        name: String,
        /// What kind of file it is.
        format: FileFormat,
        /// Its content.
        content: FileContent,
    },
    /// Ask to drive a browser session (v2). Control moves only when the
    /// broker reports the handoff.
    TakeControl {
        /// The session.
        session_id: String,
    },
    /// Hand a browser session back to the agent (v2).
    ReturnControl {
        /// The session.
        session_id: String,
        /// The newest frame the user saw.
        frame_sequence: u64,
    },
    /// Stop a browser session (v2). Allowed whoever drives.
    StopBrowser {
        /// The session.
        session_id: String,
    },
    /// The user's own input to a browser session they drive (v2): a
    /// click at a point of the page, text typed or a key pressed where its
    /// focus is, or a scroll. A frame of the page after it follows.
    BrowserInput {
        /// The session.
        session_id: String,
        /// What the user did.
        input: crate::browser::OwnerInput,
    },
    /// Keep the sign-in of a browser session the user drives, and signed in
    /// on themselves (WO-030): its site, the sites its sign-in went
    /// through, and its session, sealed, used whenever the agent opens one
    /// of those sites again.
    KeepSignedIn {
        /// The session.
        session_id: String,
    },
    /// List, sign out or delete the user's signed-in profiles (WO-030).
    ManageProfiles {
        /// The operation.
        op: ProfileOp,
    },
    /// List, keep or delete the sign-ins the broker fills (WO-030).
    ManageSignIns {
        /// The operation.
        op: SignInOp,
    },
    /// List, set, change or close the owner's goals: the owner's alone.
    ManageGoals {
        /// The operation.
        op: GoalOp,
    },
    /// List or forget what the agent is given from the owner's memory: the
    /// owner's alone.
    ManageMemory {
        /// The operation.
        op: KeptOp,
    },
    /// List the decisions the owner made, the newest first: the owner's
    /// alone.
    ListDecisions {},
    /// Read the model usage the broker counted: the owner's alone.
    ReadUsage {},
    /// Set the limits on model usage, answered with the usage as it now
    /// stands: the owner's alone.
    SetUsageLimits {
        /// The limits, each replacing what was set; an unset one is no
        /// limit.
        limits: UsageLimits,
    },
    /// Read the model Hatchling uses, and the models its server has: the
    /// owner's alone.
    ReadModel {},
    /// Read what's set up on this machine: the owner's alone.
    ReadSetup {},
    /// Choose the model Hatchling uses from the next run on, answered with
    /// the model as it then stands: the owner's alone.
    SetModel {
        /// The model, by its id on the server; none goes back to the one
        /// the operator set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Switch to the other model's server the operator set up, or back to
    /// their first, from the next run on, answered with the model as it
    /// then stands: the owner's alone. Where the owner's words go changes,
    /// so the app asks them first.
    SwitchModelServer {
        /// To the other server; false goes back to the first.
        other: bool,
    },
    /// Read the posts on the owner's Feed page, the newest first: the
    /// owner's alone.
    ReadFeed {},
    /// List, start, dismiss or restore the suggestions on the owner's
    /// Ideas page: the owner's alone.
    ManageSuggestions {
        /// The operation.
        op: SuggestionOp,
    },
    /// Give the one-time code a browser session's page asks for (WO-030):
    /// typed into the page by the broker, used once and not kept.
    GiveCode {
        /// The session.
        session_id: String,
        /// The request, as the stream showed it.
        request_id: String,
        /// The code.
        code: Concealed,
    },
    /// Decline to give a code a browser session's page asks for (WO-030).
    DeclineCode {
        /// The session.
        session_id: String,
        /// The request, as the stream showed it.
        request_id: String,
    },
}

/// What an action did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum UiActionResult {
    /// The message was accepted into the chat.
    MessageAccepted {
        /// Its id, as it will appear in the chat's events.
        message_id: String,
    },
    /// A new conversation started in the chat (WO-026).
    ConversationStarted {
        /// The chat.
        chat_id: String,
    },
    /// The decision was recorded.
    Decided {
        /// The confirmation.
        confirmation_id: String,
        /// The decision recorded.
        decision: Decision,
    },
    /// The decision was not recorded, for a typed reason.
    DecisionRejected {
        /// The confirmation.
        confirmation_id: String,
        /// Why.
        reason: DecisionRejection,
    },
    /// A schedule operation's result.
    Schedule {
        /// The result, as the session protocol defines it.
        outcome: ScheduleResult,
    },
    /// A site operation's result.
    Sites {
        /// The result.
        outcome: SiteResult,
    },
    /// A skill invocation's result.
    Skill {
        /// The result, as the session protocol defines it.
        outcome: SkillResult,
    },
    /// The action was refused before anything ran.
    Refused {
        /// Why.
        reason: ActionRefusal,
    },
    /// An artifact revision's content (v2).
    ArtifactOpened {
        /// The artifact.
        artifact_id: String,
        /// The revision.
        revision: u32,
        /// What to show.
        body: ArtifactBody,
    },
    /// The artifact revision was shared (v2).
    ArtifactShared {
        /// The artifact.
        artifact_id: String,
        /// The revision shared.
        revision: u32,
    },
    /// The share-review gate refused it, and nothing left (v2).
    ShareRefused {
        /// The artifact.
        artifact_id: String,
        /// Why.
        reason: DeliveryRefusal,
    },
    /// The file was saved as an artifact revision (v2).
    ArtifactSaved {
        /// The artifact.
        artifact_id: String,
        /// Its revision.
        revision: u32,
    },
    /// The file was not saved (v2).
    FileRefused {
        /// Why.
        reason: FileRefusal,
    },
    /// The artifact action found nothing to act on (v2).
    ArtifactRefused {
        /// The artifact.
        artifact_id: String,
        /// Why.
        reason: ArtifactRefusal,
    },
    /// The control action was accepted; the handoff event follows (v2).
    ControlAccepted {
        /// The session.
        session_id: String,
        /// The handoff it starts.
        handoff: Handoff,
    },
    /// The control action was refused, and control did not move (v2).
    ControlRefused {
        /// The session.
        session_id: String,
        /// Why.
        reason: ControlRefusal,
    },
    /// The sign-in was kept, as this profile (WO-030).
    SignedInKept {
        /// The session.
        session_id: String,
        /// The profile it's kept as.
        profile: SignedInProfile,
    },
    /// A profile operation's result (WO-030).
    Profiles {
        /// The result.
        outcome: ProfileResult,
    },
    /// A sign-in operation's result (WO-030).
    SignIns {
        /// The result.
        outcome: SignInResult,
    },
    /// A goal operation's result.
    Goals {
        /// The result.
        outcome: GoalResult,
    },
    /// A memory operation's result.
    Memory {
        /// The result.
        outcome: KeptResult,
    },
    /// The decisions the owner made, the newest first.
    Decisions {
        /// They.
        decisions: Vec<DecisionRecord>,
    },
    /// The model usage the broker counted.
    Usage {
        /// The report.
        usage: UsageReport,
    },
    /// The model Hatchling uses, and the models its server has.
    Model {
        /// It.
        model: ModelState,
    },
    /// What's set up on this machine.
    Setup {
        /// It.
        setup: SetupState,
    },
    /// The posts on the owner's Feed page, the newest first.
    Feed {
        /// They.
        posts: Vec<FeedPost>,
    },
    /// An Ideas page operation's result.
    Suggestions {
        /// The result.
        outcome: SuggestionResult,
    },
    /// The code was taken: the broker types it into the page next.
    CodeTaken {
        /// The session.
        session_id: String,
    },
    /// The code request was declined.
    CodeDeclined {
        /// The session.
        session_id: String,
    },
    /// The code wasn't taken, and why.
    CodeRefused {
        /// The session.
        session_id: String,
        /// Why.
        reason: CodeRefusal,
    },
    /// The user's input was done; a frame of the page after it follows
    /// (v2).
    InputDone {
        /// The session.
        session_id: String,
    },
    /// The browser didn't do the user's input (v2).
    InputFailed {
        /// The session.
        session_id: String,
        /// Why.
        failure: crate::browser::BrowserFailure,
    },
    /// The browser session was stopped (v2).
    BrowserStopped {
        /// The session.
        session_id: String,
    },
}

/// Why a decision was not recorded.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DecisionRejection {
    /// No such confirmation.
    Unknown,
    /// The request changed since the user saw it.
    StaleRevision,
    /// The request lapsed.
    Expired,
    /// It was already decided. The earlier decision stands.
    AlreadyDecided,
    /// The request doesn't offer that decision: approving for the session
    /// what can only be approved once.
    NotOffered,
}

/// Why an action was refused before anything ran.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ActionRefusal {
    /// The connection has not completed the handshake.
    NotConnected,
    /// The action is malformed.
    BadRequest,
    /// This backend does not offer it.
    Unavailable,
    /// The caller's grants do not cover it: an API caller may
    /// do only what its grants allow, never more than the owner.
    NotGranted,
}

/// The sentence for a version mismatch: the newer side's template,
/// with both versions filled in. `None` when the versions match.
#[must_use]
pub fn incompatibility_message(frontend: u16, backend: u16) -> Option<String> {
    let template = match frontend.cmp(&backend) {
        Ordering::Equal => return None,
        Ordering::Greater => FRONTEND_NEWER,
        Ordering::Less => BACKEND_NEWER,
    };
    Some(
        template
            .replace(FRONTEND_SLOT, &frontend.to_string())
            .replace(BACKEND_SLOT, &backend.to_string()),
    )
}

/// The backend's answer to a [`UiRequest::Hello`] at `frontend`.
#[must_use]
pub fn negotiate(frontend: u16) -> UiResponse {
    negotiate_between(frontend, UI_SCHEMA_VERSION)
}

/// [`negotiate`] against a given backend version.
fn negotiate_between(frontend: u16, backend: u16) -> UiResponse {
    incompatibility_message(frontend, backend).map_or(
        UiResponse::Welcome {
            schema_version: backend,
            session_protocol: PROTOCOL_VERSION,
            grants: Vec::new(),
        },
        |message| UiResponse::Incompatible {
            frontend,
            backend,
            message,
        },
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::subagent::ChildFailure;

    /// Serializes to exactly `expected` and reads back unchanged.
    fn pinned<T>(value: &T, expected: &Value)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        assert_eq!(&serde_json::to_value(value).unwrap(), expected);
        let back: T = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(&back, value);
    }

    fn event(kind: SubagentEvent) -> ActivityEvent {
        ActivityEvent {
            event_id: String::from("run-1:3"),
            ts: String::from("2026-10-07T02:00:00Z"),
            run_id: String::from("run-1"),
            kind,
        }
    }

    fn finished(verdict: ChildVerdict) -> SubagentEvent {
        SubagentEvent::Finished {
            child_id: String::from("c1"),
            result_hash: String::from("ab"),
            steps_spent: 2,
            verdict,
        }
    }

    fn api_call(outcome: ApiOutcome) -> SubagentEvent {
        SubagentEvent::ApiCall {
            caller: String::from("uid 1000"),
            capabilities: vec![String::from("goals:read")],
            request: ApiRequest::ManageSchedule,
            outcome,
        }
    }

    fn harness_run(outcome: HarnessOutcome) -> SubagentEvent {
        SubagentEvent::HarnessRun {
            harness: String::from("claude-code"),
            binary_sha256: String::from("ab"),
            brief_sha256: String::from("cd"),
            outcome,
            files_changed: 2,
            network: None,
        }
    }

    #[test]
    fn a_harness_run_is_activity_with_its_pinned_binary_and_brief_hash() {
        pinned(
            &event(harness_run(HarnessOutcome::NeedsClarification)),
            &json!({
                "event_id": "run-1:3",
                "ts": "2026-10-07T02:00:00Z",
                "run_id": "run-1",
                "event": "harness_run",
                "harness": "claude-code",
                "binary_sha256": "ab",
                "brief_sha256": "cd",
                "outcome": "needs_clarification",
                "files_changed": 2,
            }),
        );
    }

    #[test]
    fn an_api_call_is_activity_with_its_kind_never_its_content() {
        pinned(
            &event(api_call(ApiOutcome::Served)),
            &json!({
                "event_id": "run-1:3",
                "ts": "2026-10-07T02:00:00Z",
                "run_id": "run-1",
                "event": "api_call",
                "caller": "uid 1000",
                "capabilities": ["goals:read"],
                "request": "manage_schedule",
                "outcome": "served",
            }),
        );
        pinned(
            &UiActionResult::Refused {
                reason: ActionRefusal::NotGranted,
            },
            &json!({"result": "refused", "reason": "not_granted"}),
        );
    }

    #[test]
    fn every_subagent_event_has_its_feed_state() {
        let spawned = SubagentEvent::Spawned {
            child_id: String::from("c1"),
            parent_run_id: String::from("run-1"),
            task_brief_hash: String::from("cd"),
            budget_steps: 3,
            capabilities: Vec::new(),
            depth: 1,
        };
        let progress = SubagentEvent::Progress {
            child_id: String::from("c1"),
            note: String::from("reading"),
            steps_spent: 1,
        };
        let killed = SubagentEvent::Killed {
            child_id: String::from("c1"),
            reason: String::from("enough"),
            partial_result_hash: None,
            steps_spent: 1,
        };
        let failed = SubagentEvent::Failed {
            child_id: String::from("c1"),
            failure: ChildFailure::Timeout,
            detail: String::new(),
            steps_spent: 3,
        };
        for (kind, state) in [
            (spawned, ActivityState::Pending),
            (progress, ActivityState::Pending),
            (finished(ChildVerdict::Ok), ActivityState::Success),
            (finished(ChildVerdict::Partial), ActivityState::Partial),
            (killed, ActivityState::Interrupted),
            (failed, ActivityState::Failed),
            (api_call(ApiOutcome::Served), ActivityState::Success),
            (api_call(ApiOutcome::Refused), ActivityState::Failed),
            (harness_run(HarnessOutcome::Done), ActivityState::Success),
            (
                harness_run(HarnessOutcome::NeedsClarification),
                ActivityState::Partial,
            ),
            (harness_run(HarnessOutcome::Failed), ActivityState::Failed),
            (harness_run(HarnessOutcome::TimedOut), ActivityState::Failed),
            (harness_run(HarnessOutcome::Refused), ActivityState::Failed),
            (
                SubagentEvent::RunSummary {
                    cell_id: String::from("run-1"),
                    chat: true,
                    tools: Vec::new(),
                    model_calls: 1,
                    model_calls_refused: 1,
                    peak_input_tokens: 10,
                    output_tokens: 2,
                    context_window: None,
                    model_on_cpu: None,
                    memory_low: false,
                },
                ActivityState::Success,
            ),
        ] {
            assert_eq!(activity_state(&kind), state, "{kind:?}");
        }
    }

    #[test]
    fn an_activity_event_carries_its_state_and_time_on_the_wire() {
        let ui = UiEvent::activity(7, event(finished(ChildVerdict::Ok)));
        assert_eq!(ui.seq, 7);
        assert_eq!(ui.ts, "2026-10-07T02:00:00Z");
        pinned(
            &UiResponse::Event(ui),
            &json!({
                "type": "event",
                "seq": 7,
                "ts": "2026-10-07T02:00:00Z",
                "body": {
                    "kind": "activity",
                    "state": "success",
                    "event": {
                        "event_id": "run-1:3",
                        "ts": "2026-10-07T02:00:00Z",
                        "run_id": "run-1",
                        "event": "subagent_finished",
                        "child_id": "c1",
                        "result_hash": "ab",
                        "steps_spent": 2,
                        "verdict": "ok",
                    },
                },
            }),
        );
        let killed = UiEvent::activity(
            8,
            event(SubagentEvent::Killed {
                child_id: String::from("c1"),
                reason: String::from("enough"),
                partial_result_hash: Some(String::from("ef")),
                steps_spent: 1,
            }),
        );
        let body = serde_json::to_value(&killed.body).unwrap();
        assert_eq!(body.get("state"), Some(&json!("interrupted")));
    }

    #[test]
    fn the_handshake_and_its_refusal_are_pinned() {
        pinned(
            &UiRequest::Hello {
                schema_version: 1,
                client: String::from("web"),
                token: None,
            },
            &json!({"type": "hello", "schema_version": 1, "client": "web", "token": null}),
        );
        pinned(
            &UiRequest::Hello {
                schema_version: 3,
                client: String::from("cli"),
                token: Some(ApiToken::new(String::from("t"))),
            },
            &json!({"type": "hello", "schema_version": 3, "client": "cli", "token": "t"}),
        );
        // A UI that predates the token still reads as tokenless.
        assert_eq!(
            serde_json::from_value::<UiRequest>(
                json!({"type": "hello", "schema_version": 3, "client": "web"})
            )
            .unwrap(),
            UiRequest::Hello {
                schema_version: 3,
                client: String::from("web"),
                token: None,
            }
        );
        pinned(
            &UiRequest::Subscribe {
                after_seq: Some(41),
            },
            &json!({"type": "subscribe", "after_seq": 41}),
        );
        pinned(
            &UiRequest::Subscribe { after_seq: None },
            &json!({"type": "subscribe", "after_seq": null}),
        );
        pinned(
            &negotiate(UI_SCHEMA_VERSION),
            &json!({"type": "welcome", "schema_version": UI_SCHEMA_VERSION, "session_protocol": PROTOCOL_VERSION}),
        );
        // A welcome names the connection's grants; one without them, from
        // an older broker, reads as holding none.
        pinned(
            &UiResponse::Welcome {
                schema_version: UI_SCHEMA_VERSION,
                session_protocol: PROTOCOL_VERSION,
                grants: vec![crate::ScheduleGrant::ChannelSend {
                    channel: String::from("local"),
                }],
            },
            &json!({
                "type": "welcome",
                "schema_version": UI_SCHEMA_VERSION,
                "session_protocol": PROTOCOL_VERSION,
                "grants": [{"grant": "channel-send", "channel": "local"}],
            }),
        );
        let message = incompatibility_message(UI_SCHEMA_VERSION + 1, UI_SCHEMA_VERSION).unwrap();
        pinned(
            &negotiate(UI_SCHEMA_VERSION + 1),
            &json!({
                "type": "incompatible",
                "frontend": UI_SCHEMA_VERSION + 1,
                "backend": UI_SCHEMA_VERSION,
                "message": message,
            }),
        );
    }

    #[test]
    fn a_mismatch_says_which_side_to_update_in_plain_words() {
        assert_eq!(
            incompatibility_message(3, 3),
            None,
            "matching versions are no mismatch"
        );
        assert_eq!(
            incompatibility_message(3, 2).as_deref(),
            Some(
                "frontend v3 needs broker \u{2265} v3; broker is v2 \u{2014} restart the broker to update"
            )
        );
        assert_eq!(
            incompatibility_message(2, 3).as_deref(),
            Some(
                "broker v3 needs frontend \u{2265} v3; frontend is v2 \u{2014} reload the frontend to update"
            )
        );
        assert_eq!(
            negotiate_between(4, 4),
            UiResponse::Welcome {
                schema_version: 4,
                session_protocol: PROTOCOL_VERSION,
                grants: Vec::new(),
            }
        );
        let older_backend = negotiate_between(5, 4);
        let expected = UiResponse::Incompatible {
            frontend: 5,
            backend: 4,
            message: incompatibility_message(5, 4).unwrap(),
        };
        assert_eq!(older_backend, expected);
        let newer_backend = negotiate_between(4, 5);
        let expected = UiResponse::Incompatible {
            frontend: 4,
            backend: 5,
            message: incompatibility_message(4, 5).unwrap(),
        };
        assert_eq!(newer_backend, expected);
    }

    #[test]
    fn chat_artifact_schedule_and_confirmation_events_are_pinned() {
        let at = |seq, body| UiEvent {
            seq,
            ts: String::from("t"),
            body,
        };
        pinned(
            &at(
                1,
                UiEventBody::ChatMessage(ChatMessage {
                    chat_id: String::from("main"),
                    message_id: String::from("m1"),
                    author: ChatAuthor::Agent,
                    run: None,
                    text: String::from("<b>hi</b>"),
                }),
            ),
            &json!({"seq": 1, "ts": "t", "body": {"kind": "chat_message", "chat_id": "main", "message_id": "m1", "author": "agent", "text": "<b>hi</b>"}}),
        );
        pinned(
            &at(
                2,
                UiEventBody::Artifact(ArtifactStatus::Failed {
                    build_id: String::from("b1"),
                    build_kind: String::from("page"),
                    failure: BuildFailure::BadOutput,
                }),
            ),
            &json!({"seq": 2, "ts": "t", "body": {"kind": "artifact", "state": "failed", "build_id": "b1", "build_kind": "page", "failure": "bad-output"}}),
        );
        pinned(
            &at(
                3,
                UiEventBody::Artifact(ArtifactStatus::Refused {
                    build_id: String::from("b1"),
                    refusal: DeliveryRefusal::HashMismatch,
                }),
            ),
            &json!({"seq": 3, "ts": "t", "body": {"kind": "artifact", "state": "refused", "build_id": "b1", "refusal": "hash-mismatch"}}),
        );
        pinned(
            &at(
                4,
                UiEventBody::Schedule(ScheduleNotice::Failed {
                    schedule_id: String::from("s1"),
                    detail: String::from("timeout"),
                }),
            ),
            &json!({"seq": 4, "ts": "t", "body": {"kind": "schedule", "outcome": "failed", "schedule_id": "s1", "detail": "timeout"}}),
        );
        pinned(
            &at(
                5,
                UiEventBody::Confirmation(ConfirmationRequest {
                    confirmation_id: String::from("k1"),
                    revision: 2,
                    chat_id: Some(String::from("main")),
                    summary: String::from("send the email"),
                    expires_at: None,
                    digest: String::from("ab12"),
                    fields: vec![
                        ConfirmationField {
                            label: String::from("To"),
                            value: String::from("pat@example.com"),
                            source: FieldSource::Broker,
                        },
                        ConfirmationField {
                            label: String::from("Text"),
                            value: String::from("Hello"),
                            source: FieldSource::Agent,
                        },
                    ],
                    session: false,
                    always: false,
                    approve: None,
                }),
            ),
            &json!({"seq": 5, "ts": "t", "body": {"kind": "confirmation", "confirmation_id": "k1", "revision": 2, "chat_id": "main", "summary": "send the email", "expires_at": null, "digest": "ab12", "fields": [{"label": "To", "value": "pat@example.com", "source": "broker"}, {"label": "Text", "value": "Hello", "source": "agent"}], "session": false, "always": false}}),
        );
        pinned(
            &at(
                6,
                UiEventBody::ConfirmationDecided {
                    confirmation_id: String::from("k1"),
                    revision: 2,
                    decision: Decision::Deny,
                },
            ),
            &json!({"seq": 6, "ts": "t", "body": {"kind": "confirmation_decided", "confirmation_id": "k1", "revision": 2, "decision": "deny"}}),
        );
    }

    #[test]
    fn actions_and_their_results_are_pinned() {
        pinned(
            &UiRequest::Action {
                request_id: String::from("r1"),
                action: UiAction::Decide {
                    confirmation_id: String::from("k1"),
                    revision: 2,
                    digest: String::from("ab12"),
                    decision: Decision::Approve,
                },
            },
            &json!({"type": "action", "request_id": "r1", "action": {"action": "decide", "confirmation_id": "k1", "revision": 2, "digest": "ab12", "decision": "approve"}}),
        );
        pinned(
            &UiAction::Decide {
                confirmation_id: String::from("k1"),
                revision: 2,
                digest: String::from("ab12"),
                decision: Decision::ApproveSession,
            },
            &json!({"action": "decide", "confirmation_id": "k1", "revision": 2, "digest": "ab12", "decision": "approve_session"}),
        );
        pinned(
            &UiAction::SendMessage {
                chat_id: String::from("main"),
                text: String::from("hello"),
            },
            &json!({"action": "send_message", "chat_id": "main", "text": "hello"}),
        );
        pinned(
            &UiAction::NewConversation {
                chat_id: String::from("local"),
            },
            &json!({"action": "new_conversation", "chat_id": "local"}),
        );
        pinned(
            &UiActionResult::ConversationStarted {
                chat_id: String::from("local"),
            },
            &json!({"result": "conversation_started", "chat_id": "local"}),
        );
        pinned(
            &UiAction::ManageSchedule {
                op: ScheduleOp::Cancel {
                    id: String::from("s1"),
                },
            },
            &json!({"action": "manage_schedule", "op": {"op": "cancel", "id": "s1"}}),
        );
        pinned(
            &UiResponse::ActionResult {
                request_id: String::from("r1"),
                result: UiActionResult::DecisionRejected {
                    confirmation_id: String::from("k1"),
                    reason: DecisionRejection::AlreadyDecided,
                },
            },
            &json!({"type": "action_result", "request_id": "r1", "result": {"result": "decision_rejected", "confirmation_id": "k1", "reason": "already_decided"}}),
        );
        pinned(
            &UiActionResult::Skill {
                outcome: SkillResult::UnknownSkill,
            },
            &json!({"result": "skill", "outcome": {"r": "unknown-skill"}}),
        );
        pinned(
            &UiActionResult::Refused {
                reason: ActionRefusal::NotConnected,
            },
            &json!({"result": "refused", "reason": "not_connected"}),
        );
    }

    fn s(text: &str) -> String {
        String::from(text)
    }

    fn png() -> InlineImage {
        InlineImage {
            media_type: ImageType::Png,
            base64: s("iVBORw0KGgo="),
        }
    }

    #[test]
    fn surface_events_are_pinned() {
        pinned(
            &UiEventBody::AgentStatus {
                state: AgentState::AwaitingApproval,
                detail: s("Post the summary?"),
            },
            &json!({"kind": "agent_status", "state": "awaiting_approval", "detail": "Post the summary?"}),
        );
        let revision = ArtifactRevision {
            revision: 2,
            build_id: s("b2"),
            created_at: s("2026-10-07T09:00:00Z"),
            bytes: 120,
            attribution: None,
        };
        pinned(
            &UiEventBody::ArtifactListed(UiArtifact {
                artifact_id: s("a1"),
                title: s("Notes"),
                format: FileFormat::Markdown,
                revisions: vec![revision],
                shared_revision: Some(1),
            }),
            &json!({"kind": "artifact_listed", "artifact_id": "a1", "title": "Notes", "format": "markdown", "shared_revision": 1,
                "revisions": [{"revision": 2, "build_id": "b2", "created_at": "2026-10-07T09:00:00Z", "bytes": 120, "attribution": null}]}),
        );
        pinned(
            &UiEventBody::Attachment {
                chat_id: s("main"),
                message_id: s("m1"),
                author: ChatAuthor::Agent,
                artifact_id: s("a1"),
                revision: 2,
                text: s("Here it is."),
            },
            &json!({"kind": "attachment", "chat_id": "main", "message_id": "m1", "author": "agent", "artifact_id": "a1", "revision": 2, "text": "Here it is."}),
        );
        pinned(
            &UiEventBody::BrowserSession(BrowserSession {
                session_id: s("w1"),
                chat_id: Some(s("main")),
                title: s("Find a tote"),
                site: s("shop.example"),
                state: BrowserState::Running,
                step: s("Searching"),
                last_confirmed: s("Home page"),
            }),
            &json!({"kind": "browser_session", "session_id": "w1", "chat_id": "main", "title": "Find a tote", "site": "shop.example", "state": "running", "step": "Searching", "last_confirmed": "Home page"}),
        );
        pinned(
            &UiEventBody::BrowserFrame(BrowserFrame {
                session_id: s("w1"),
                sequence: 7,
                captured_at: s("2026-10-07T09:00:01Z"),
                title: s("Results"),
                image: Some(png()),
            }),
            &json!({"kind": "browser_frame", "session_id": "w1", "sequence": 7, "captured_at": "2026-10-07T09:00:01Z", "title": "Results", "image": {"media_type": "png", "base64": "iVBORw0KGgo="}}),
        );
        pinned(
            &UiEventBody::BrowserAction(BrowserAction {
                session_id: s("w1"),
                by: Actor::User,
                narration: s("Opened the second result"),
            }),
            &json!({"kind": "browser_action", "session_id": "w1", "by": "user", "narration": "Opened the second result"}),
        );
        pinned(
            &UiEventBody::BrowserControl {
                session_id: s("w1"),
                handoff: Handoff::Resumed,
                frame_sequence: 9,
            },
            &json!({"kind": "browser_control", "session_id": "w1", "handoff": "resumed", "frame_sequence": 9}),
        );
        pinned(
            &UiEventBody::BrowserCode {
                session_id: s("w1"),
                request_id: s("code-1"),
                origin: s("https://a.example"),
                state: CodeState::Asked,
            },
            &json!({"kind": "browser_code", "session_id": "w1", "request_id": "code-1", "origin": "https://a.example", "state": "asked"}),
        );
    }

    #[test]
    fn surface_actions_are_pinned() {
        let actions = [
            (
                UiAction::OpenArtifact {
                    artifact_id: s("a1"),
                    revision: 2,
                },
                json!({"action": "open_artifact", "artifact_id": "a1", "revision": 2}),
            ),
            (
                UiAction::ShareArtifact {
                    artifact_id: s("a1"),
                    revision: 2,
                },
                json!({"action": "share_artifact", "artifact_id": "a1", "revision": 2}),
            ),
            (
                UiAction::AttachArtifact {
                    chat_id: s("main"),
                    artifact_id: s("a1"),
                    revision: 2,
                },
                json!({"action": "attach_artifact", "chat_id": "main", "artifact_id": "a1", "revision": 2}),
            ),
            (
                UiAction::SaveFile {
                    chat_id: None,
                    name: s("photo.png"),
                    format: FileFormat::Image,
                    content: FileContent::Image(png()),
                },
                json!({"action": "save_file", "chat_id": null, "name": "photo.png", "format": "image", "content": {"type": "image", "media_type": "png", "base64": "iVBORw0KGgo="}}),
            ),
            (
                UiAction::TakeControl {
                    session_id: s("w1"),
                },
                json!({"action": "take_control", "session_id": "w1"}),
            ),
            (
                UiAction::ReturnControl {
                    session_id: s("w1"),
                    frame_sequence: 8,
                },
                json!({"action": "return_control", "session_id": "w1", "frame_sequence": 8}),
            ),
            (
                UiAction::StopBrowser {
                    session_id: s("w1"),
                },
                json!({"action": "stop_browser", "session_id": "w1"}),
            ),
            (
                UiAction::KeepSignedIn {
                    session_id: s("w1"),
                },
                json!({"action": "keep_signed_in", "session_id": "w1"}),
            ),
            (
                UiAction::ManageProfiles {
                    op: ProfileOp::SignOut {
                        profile_id: s("profile-1"),
                    },
                },
                json!({"action": "manage_profiles", "op": {"op": "sign_out", "profile_id": "profile-1"}}),
            ),
            (
                UiAction::BrowserInput {
                    session_id: s("w1"),
                    input: crate::browser::OwnerInput::Click { x: 4, y: 5 },
                },
                json!({"action": "browser_input", "session_id": "w1", "input": {"input": "click", "x": 4, "y": 5}}),
            ),
        ];
        for (action, expected) in &actions {
            pinned(action, expected);
        }
    }

    #[test]
    fn surface_results_are_pinned() {
        let results = [
            (
                UiActionResult::ArtifactOpened {
                    artifact_id: s("a1"),
                    revision: 2,
                    body: ArtifactBody::Text {
                        text: s("<script>x</script>"),
                    },
                },
                json!({"result": "artifact_opened", "artifact_id": "a1", "revision": 2, "body": {"type": "text", "text": "<script>x</script>"}}),
            ),
            (
                UiActionResult::ArtifactShared {
                    artifact_id: s("a1"),
                    revision: 2,
                },
                json!({"result": "artifact_shared", "artifact_id": "a1", "revision": 2}),
            ),
            (
                UiActionResult::ShareRefused {
                    artifact_id: s("a1"),
                    reason: DeliveryRefusal::Blocked,
                },
                json!({"result": "share_refused", "artifact_id": "a1", "reason": "blocked"}),
            ),
            (
                UiActionResult::ArtifactSaved {
                    artifact_id: s("a2"),
                    revision: 1,
                },
                json!({"result": "artifact_saved", "artifact_id": "a2", "revision": 1}),
            ),
            (
                UiActionResult::FileRefused {
                    reason: FileRefusal::TooLarge { limit: 1024 },
                },
                json!({"result": "file_refused", "reason": {"refusal": "too_large", "limit": 1024}}),
            ),
            (
                UiActionResult::ArtifactRefused {
                    artifact_id: s("a9"),
                    reason: ArtifactRefusal::UnknownArtifact,
                },
                json!({"result": "artifact_refused", "artifact_id": "a9", "reason": "unknown_artifact"}),
            ),
            (
                UiActionResult::ControlAccepted {
                    session_id: s("w1"),
                    handoff: Handoff::Takeover,
                },
                json!({"result": "control_accepted", "session_id": "w1", "handoff": "takeover"}),
            ),
            (
                UiActionResult::ControlRefused {
                    session_id: s("w1"),
                    reason: ControlRefusal::StaleFrame,
                },
                json!({"result": "control_refused", "session_id": "w1", "reason": "stale_frame"}),
            ),
            (
                UiActionResult::BrowserStopped {
                    session_id: s("w1"),
                },
                json!({"result": "browser_stopped", "session_id": "w1"}),
            ),
            (
                UiActionResult::InputDone {
                    session_id: s("w1"),
                },
                json!({"result": "input_done", "session_id": "w1"}),
            ),
            (
                UiActionResult::InputFailed {
                    session_id: s("w1"),
                    failure: crate::browser::BrowserFailure::NotPossible,
                },
                json!({"result": "input_failed", "session_id": "w1", "failure": "not_possible"}),
            ),
        ];
        for (result, expected) in &results {
            pinned(result, expected);
        }
    }

    #[test]
    fn profile_results_are_pinned() {
        let results = [
            (
                UiActionResult::SignedInKept {
                    session_id: s("w1"),
                    profile: SignedInProfile {
                        profile_id: s("profile-1"),
                        sites: vec![s("a.example:443")],
                        signed_in: true,
                        created_at: s("2026-10-22T00:00:00Z"),
                        last_used: s("2026-10-22T00:00:00Z"),
                    },
                },
                json!({"result": "signed_in_kept", "session_id": "w1", "profile": {
                    "profile_id": "profile-1", "sites": ["a.example:443"], "signed_in": true,
                    "created_at": "2026-10-22T00:00:00Z", "last_used": "2026-10-22T00:00:00Z"
                }}),
            ),
            (
                UiActionResult::Profiles {
                    outcome: ProfileResult::Listed {
                        profiles: Vec::new(),
                    },
                },
                json!({"result": "profiles", "outcome": {"result": "listed", "profiles": []}}),
            ),
            (
                UiActionResult::Profiles {
                    outcome: ProfileResult::NotFound,
                },
                json!({"result": "profiles", "outcome": {"result": "not_found"}}),
            ),
        ];
        for (result, expected) in &results {
            pinned(result, expected);
        }
    }

    #[test]
    fn a_code_travels_as_a_string_and_never_shows() {
        let give = UiAction::GiveCode {
            session_id: s("w1"),
            request_id: s("code-1"),
            code: Concealed::new(s("481516")),
        };
        pinned(
            &give,
            &json!({"action": "give_code", "session_id": "w1", "request_id": "code-1", "code": "481516"}),
        );
        let shown = format!("{give:?}");
        assert!(!shown.contains("481516"), "{shown}");
        pinned(
            &UiAction::DeclineCode {
                session_id: s("w1"),
                request_id: s("code-1"),
            },
            &json!({"action": "decline_code", "session_id": "w1", "request_id": "code-1"}),
        );
        for (result, expected) in [
            (
                UiActionResult::CodeTaken {
                    session_id: s("w1"),
                },
                json!({"result": "code_taken", "session_id": "w1"}),
            ),
            (
                UiActionResult::CodeDeclined {
                    session_id: s("w1"),
                },
                json!({"result": "code_declined", "session_id": "w1"}),
            ),
            (
                UiActionResult::CodeRefused {
                    session_id: s("w1"),
                    reason: CodeRefusal::NoRequest,
                },
                json!({"result": "code_refused", "session_id": "w1", "reason": "no_request"}),
            ),
        ] {
            pinned(&result, &expected);
        }
    }

    #[test]
    fn a_sign_in_travels_as_strings_and_its_secrets_never_show() {
        let store = UiAction::ManageSignIns {
            op: SignInOp::Store {
                profile_id: s("profile-1"),
                origin: s("https://a.example"),
                label: s("Work"),
                username: Concealed::new(s("ada@a.example")),
                password: Concealed::new(s("hunter2-secret")),
            },
        };
        pinned(
            &store,
            &json!({"action": "manage_sign_ins", "op": {"op": "store", "profile_id": "profile-1",
                "origin": "https://a.example", "label": "Work", "username": "ada@a.example",
                "password": "hunter2-secret"}}),
        );
        let shown = format!("{store:?}");
        assert!(
            !shown.contains("hunter2") && !shown.contains("ada@"),
            "{shown}"
        );
        assert!(shown.contains("Concealed(..)"), "{shown}");
        assert_eq!(Concealed::new(s("x")).expose(), "x");
        assert_eq!(
            Concealed::from_bytes(b"pw \"q\"".to_vec()).map(|secret| secret.expose().to_owned()),
            Some(s("pw \"q\""))
        );
        assert!(
            Concealed::from_bytes(b"pw \xff".to_vec()).is_none(),
            "a secret that isn't text"
        );
        let credential = SiteSignIn {
            sign_in_id: s("cred-1"),
            profile_id: s("profile-1"),
            origin: s("https://a.example"),
            label: s("Work"),
            created_at: s("2026-10-22T00:00:00Z"),
        };
        let results = [
            (
                UiActionResult::SignIns {
                    outcome: SignInResult::Stored {
                        sign_in: credential,
                    },
                },
                json!({"result": "sign_ins", "outcome": {"result": "stored", "sign_in": {
                    "sign_in_id": "cred-1", "profile_id": "profile-1", "origin": "https://a.example",
                    "label": "Work", "created_at": "2026-10-22T00:00:00Z"
                }}}),
            ),
            (
                UiActionResult::SignIns {
                    outcome: SignInResult::Refused {
                        reason: SignInRefusal::NotTheProfiles,
                    },
                },
                json!({"result": "sign_ins", "outcome": {"result": "refused", "reason": "not_the_profiles"}}),
            ),
        ];
        for (result, expected) in &results {
            pinned(result, expected);
        }
        pinned(
            &UiAction::ManageSignIns {
                op: SignInOp::Delete {
                    sign_in_id: s("cred-1"),
                },
            },
            &json!({"action": "manage_sign_ins", "op": {"op": "delete", "sign_in_id": "cred-1"}}),
        );
    }

    #[test]
    fn suggestions_are_managed_by_id_and_read_as_the_agents_words() {
        pinned(
            &UiAction::ManageSuggestions {
                op: SuggestionOp::Start { id: s("idea-1") },
            },
            &json!({"action": "manage_suggestions", "op": {"op": "start", "id": "idea-1"}}),
        );
        pinned(
            &UiAction::ManageSuggestions {
                op: SuggestionOp::List,
            },
            &json!({"action": "manage_suggestions", "op": {"op": "list"}}),
        );
        pinned(
            &UiActionResult::Suggestions {
                outcome: SuggestionResult::Listed {
                    suggestions: vec![Suggestion {
                        suggestion_id: s("idea-1"),
                        title: s("A morning run plan"),
                        summary: s("A week of short runs."),
                        why: s("You set a goal to run 5k."),
                        area: crate::messages::SuggestionArea::Health,
                        suggested_at: s("2026-11-04T08:30:00Z"),
                        state: SuggestionState::Suggested,
                    }],
                },
            },
            &json!({"result": "suggestions", "outcome": {"result": "listed", "suggestions": [{
                "suggestion_id": "idea-1",
                "title": "A morning run plan",
                "summary": "A week of short runs.",
                "why": "You set a goal to run 5k.",
                "area": "health",
                "suggested_at": "2026-11-04T08:30:00Z",
                "state": "suggested"
            }]}}),
        );
        for (outcome, tag) in [
            (SuggestionResult::NotFound, "not_found"),
            (SuggestionResult::Off, "off"),
            (SuggestionResult::Failed, "failed"),
        ] {
            let wire = serde_json::to_value(UiActionResult::Suggestions { outcome }).unwrap();
            assert_eq!(wire.pointer("/outcome/result"), Some(&json!(tag)), "{wire}");
        }
        assert_eq!(IDEAS_PAGE, "ideas");
    }

    #[test]
    fn the_feed_is_read_as_kept_posts() {
        pinned(&UiAction::ReadFeed {}, &json!({"action": "read_feed"}));
        pinned(
            &UiActionResult::Feed {
                posts: vec![FeedPost {
                    post_id: s("post-1"),
                    written_at: s("2026-11-04T08:30:00Z"),
                    text: s("Your morning\nTwo goals moved."),
                }],
            },
            &json!({"result": "feed", "posts": [{
                "post_id": "post-1",
                "written_at": "2026-11-04T08:30:00Z",
                "text": "Your morning\nTwo goals moved."
            }]}),
        );
        assert_eq!(FEED_PAGE, "feed");
    }

    #[test]
    fn usage_is_read_as_counted_days_and_models() {
        pinned(&UiAction::ReadUsage {}, &json!({"action": "read_usage"}));
        let day = |date: &str, calls| UsageDay {
            date: s(date),
            calls,
            input_tokens: 1200,
            output_tokens: 300,
            unreported: 1,
        };
        pinned(
            &UiActionResult::Usage {
                usage: UsageReport {
                    counted: true,
                    days: vec![day("2026-11-03", 4), day("2026-11-02", 0)],
                    models: vec![UsageModel {
                        model: s("qwen3.5:9b"),
                        calls: 4,
                        input_tokens: 2400,
                        output_tokens: 600,
                    }],
                    limits: UsageLimits {
                        daily_tokens: Some(500_000),
                        monthly_tokens: None,
                    },
                    spent: UsageSpent {
                        today: 1500,
                        month: 9000,
                        month_of: s("2026-11"),
                    },
                },
            },
            &json!({"result": "usage", "usage": {
                "counted": true,
                "days": [
                    {"date": "2026-11-03", "calls": 4, "input_tokens": 1200, "output_tokens": 300, "unreported": 1},
                    {"date": "2026-11-02", "calls": 0, "input_tokens": 1200, "output_tokens": 300, "unreported": 1},
                ],
                "models": [{"model": "qwen3.5:9b", "calls": 4, "input_tokens": 2400, "output_tokens": 600}],
                "limits": {"daily_tokens": 500_000},
                "spent": {"today": 1500, "month": 9000, "month_of": "2026-11"},
            }}),
        );
        pinned(
            &UiAction::SetUsageLimits {
                limits: UsageLimits {
                    daily_tokens: None,
                    monthly_tokens: Some(20_000_000),
                },
            },
            &json!({"action": "set_usage_limits", "limits": {"monthly_tokens": 20_000_000}}),
        );
        pinned(
            &UiAction::SetUsageLimits {
                limits: UsageLimits::default(),
            },
            &json!({"action": "set_usage_limits", "limits": {}}),
        );
        let unknown = serde_json::from_value::<UiAction>(
            json!({"action": "set_usage_limits", "limits": {"hourly_tokens": 5}}),
        );
        assert!(unknown.is_err(), "{unknown:?}");
    }

    #[test]
    fn the_model_is_read_and_chosen() {
        pinned(&UiAction::ReadModel {}, &json!({"action": "read_model"}));
        pinned(&UiAction::ReadSetup {}, &json!({"action": "read_setup"}));
        pinned(
            &UiAction::SetModel {
                name: Some(s("gemma4:12b")),
            },
            &json!({"action": "set_model", "name": "gemma4:12b"}),
        );
        pinned(
            &UiAction::SetModel { name: None },
            &json!({"action": "set_model"}),
        );
        pinned(
            &UiActionResult::Model {
                model: ModelState {
                    endpoint: Some(s("http://localhost:11434/v1")),
                    configured: Some(s("qwen3.5:9b")),
                    chosen: Some(s("gemma4:12b")),
                    available: Some(vec![s("gemma4:12b"), s("qwen3.5:9b")]),
                    window: Some(32768),
                    size: Some(6_600_000_000),
                    on_cpu: Some(0),
                    other: None,
                    switched: false,
                },
            },
            &json!({"result": "model", "model": {
                "endpoint": "http://localhost:11434/v1",
                "configured": "qwen3.5:9b",
                "chosen": "gemma4:12b",
                "available": ["gemma4:12b", "qwen3.5:9b"],
                "window": 32768,
                "size": 6_600_000_000_u64,
                "on_cpu": 0,
            }}),
        );
        pinned(
            &UiAction::SwitchModelServer { other: true },
            &json!({"action": "switch_model_server", "other": true}),
        );
        pinned(
            &UiActionResult::Model {
                model: ModelState {
                    endpoint: Some(s("https://openrouter.ai/api/v1")),
                    configured: Some(s("nvidia/nemotron-3-ultra-550b-a55b:free")),
                    other: Some(ModelServer {
                        endpoint: Some(s("http://localhost:11434/v1")),
                        name: Some(s("qwen3.5:9b")),
                    }),
                    switched: true,
                    ..ModelState::default()
                },
            },
            &json!({"result": "model", "model": {
                "endpoint": "https://openrouter.ai/api/v1",
                "configured": "nvidia/nemotron-3-ultra-550b-a55b:free",
                "other": {"endpoint": "http://localhost:11434/v1", "name": "qwen3.5:9b"},
                "switched": true,
            }}),
        );
        pinned(
            &UiActionResult::Model {
                model: ModelState::default(),
            },
            &json!({"result": "model", "model": {}}),
        );
    }

    #[test]
    fn goals_travel_as_their_ops_and_results() {
        pinned(
            &UiAction::ManageGoals { op: GoalOp::List },
            &json!({"action": "manage_goals", "op": {"op": "list"}}),
        );
        pinned(
            &UiAction::ManageGoals {
                op: GoalOp::Create {
                    title: s("Run a 10k"),
                    detail: s("Under an hour"),
                },
            },
            &json!({"action": "manage_goals", "op": {"op": "create", "title": "Run a 10k", "detail": "Under an hour"}}),
        );
        pinned(
            &UiAction::ManageGoals {
                op: GoalOp::Update {
                    id: s("g1"),
                    title: None,
                    detail: Some(s("Under 55 minutes")),
                },
            },
            &json!({"action": "manage_goals", "op": {"op": "update", "id": "g1", "detail": "Under 55 minutes"}}),
        );
        pinned(
            &UiAction::ManageGoals {
                op: GoalOp::Close {
                    id: s("g1"),
                    achieved: true,
                },
            },
            &json!({"action": "manage_goals", "op": {"op": "close", "id": "g1", "achieved": true}}),
        );
        let goal = GoalView {
            id: s("g1"),
            title: s("Run a 10k"),
            detail: String::new(),
            state: GoalState::Achieved,
            created_at: 10,
            updated_at: 20,
        };
        pinned(
            &UiActionResult::Goals {
                outcome: GoalResult::Listed {
                    goals: vec![goal.clone()],
                },
            },
            &json!({"result": "goals", "outcome": {"result": "listed", "goals": [
                {"id": "g1", "title": "Run a 10k", "detail": "", "state": "achieved", "created_at": 10, "updated_at": 20}
            ]}}),
        );
        for (outcome, tag) in [
            (GoalResult::Saved { goal }, "saved"),
            (GoalResult::NotFound, "not_found"),
            (GoalResult::AlreadyClosed, "already_closed"),
            (GoalResult::Rejected, "rejected"),
            (GoalResult::LimitReached, "limit_reached"),
            (GoalResult::Off, "off"),
            (GoalResult::Failed, "failed"),
        ] {
            let wire = serde_json::to_value(UiActionResult::Goals { outcome }).unwrap();
            assert_eq!(wire.pointer("/outcome/result"), Some(&json!(tag)), "{wire}");
        }
    }

    #[test]
    fn what_is_kept_travels_as_its_ops_and_results() {
        pinned(
            &UiAction::ManageMemory { op: KeptOp::List },
            &json!({"action": "manage_memory", "op": {"op": "list"}}),
        );
        pinned(
            &UiAction::ManageMemory {
                op: KeptOp::Forget { id: s("kept-1") },
            },
            &json!({"action": "manage_memory", "op": {"op": "forget", "id": "kept-1"}}),
        );
        pinned(
            &UiActionResult::Memory {
                outcome: KeptResult::Listed {
                    kept: vec![Kept {
                        id: s("kept-1"),
                        content: s("Their cat is Biscuit."),
                        kept_at: 10,
                        approved: true,
                    }],
                },
            },
            &json!({"result": "memory", "outcome": {"result": "listed", "kept": [
                {"id": "kept-1", "content": "Their cat is Biscuit.", "kept_at": 10, "approved": true}
            ]}}),
        );
        for (outcome, tag) in [
            (KeptResult::Forgotten, "forgotten"),
            (KeptResult::NotFound, "not_found"),
            (KeptResult::Off, "off"),
            (KeptResult::Failed, "failed"),
        ] {
            let wire = serde_json::to_value(UiActionResult::Memory { outcome }).unwrap();
            assert_eq!(wire.pointer("/outcome/result"), Some(&json!(tag)), "{wire}");
        }
    }

    #[test]
    fn decisions_are_listed_as_kept() {
        pinned(
            &UiAction::ListDecisions {},
            &json!({"action": "list_decisions"}),
        );
        pinned(
            &UiActionResult::Decisions {
                decisions: vec![DecisionRecord {
                    confirmation_id: s("req-1"),
                    summary: s("Remember this about you?"),
                    fields: vec![ConfirmationField {
                        label: s("What it would remember"),
                        value: s("Their cat is Biscuit."),
                        source: FieldSource::Agent,
                    }],
                    decision: Decision::Approve,
                    decided_at: s("2026-11-03T12:00:00Z"),
                }],
            },
            &json!({"result": "decisions", "decisions": [{
                "confirmation_id": "req-1",
                "summary": "Remember this about you?",
                "fields": [{"label": "What it would remember", "value": "Their cat is Biscuit.", "source": "agent"}],
                "decision": "approve",
                "decided_at": "2026-11-03T12:00:00Z",
            }]}),
        );
    }
}
