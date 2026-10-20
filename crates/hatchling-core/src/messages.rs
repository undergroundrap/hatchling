//! Handshake message types: the vocabulary of the `HELLO` / `WELCOME` /
//! `REJECT` exchange.
//!
//! These are the only messages either end may send before the handshake
//! completes. The tagged JSON encoding is part of the wire contract.
//!
//! After the handshake, the session speaks [`SessionRequest`] (agent to
//! broker) and [`SessionResponse`] (broker to agent): the unified
//! post-handshake vocabulary covering vault, egress, channel, memory, and
//! skill ops. Every op names one thing and performs one action; there is
//! no bulk operation in the vocabulary by design.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{CapabilityName, Error};

/// A message the agent sends.
///
/// Public because the handshake protocol is a cross-crate contract: the
/// agent, the broker, and test fixtures all speak it.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "t")]
pub enum AgentMessage {
    /// Opens the handshake: protocol version plus the cell's identity.
    #[serde(rename = "hello")]
    Hello {
        /// [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION).
        v: u16,
        /// The cell the agent runs in. Informational for the broker's
        /// audit trail; identity comes from the kernel, never this field.
        cell_id: String,
    },
}

/// A message the broker may send in reply.
///
/// Public for the same reason as [`AgentMessage`].
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "t")]
pub enum BrokerMessage {
    /// Handshake accepted.
    #[serde(rename = "welcome")]
    Welcome {
        /// [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION).
        v: u16,
    },
    /// Handshake refused. The agent must not retry or downgrade.
    #[serde(rename = "reject")]
    Reject {
        /// [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION).
        v: u16,
        /// Why the broker refused.
        reason: String,
    },
}

/// A vault operation: the agent's post-handshake request vocabulary.
///
/// Each variant names exactly one secret and performs exactly one
/// action. There is deliberately no list, bulk-read, or dump variant —
/// the protocol cannot express enumeration, so a compromised agent
/// cannot bulk-read the store. See the `hatchling-dev` no-bulk-read
/// ratchet, which fails the gate if such a variant is ever added.
///
/// Public because the vault protocol is a cross-crate contract: the
/// agent, the broker, and test fixtures all speak it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum VaultOp {
    /// Stores `secret_b64` under `name`, replacing any existing value.
    /// The secret is base64: arbitrary bytes inside framed JSON.
    #[serde(rename = "store")]
    Store {
        /// Validated [`SecretName`](crate::SecretName), as a string.
        name: String,
        /// Base64-encoded secret bytes.
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        secret_b64: crate::SecretB64,
    },
    /// Fetches the secret stored under `name`.
    #[serde(rename = "fetch")]
    Fetch {
        /// Validated [`SecretName`](crate::SecretName), as a string.
        name: String,
    },
    /// Removes the secret stored under `name`. Idempotent: removing a
    /// missing secret reports [`Removed`](VaultResult::Removed).
    #[serde(rename = "remove")]
    Remove {
        /// Validated [`SecretName`](crate::SecretName), as a string.
        name: String,
    },
}

impl VaultOp {
    /// The action this operation performs: the unit of grant matching.
    #[must_use]
    pub const fn action(&self) -> VaultAction {
        match self {
            Self::Store { .. } => VaultAction::Store,
            Self::Fetch { .. } => VaultAction::Fetch,
            Self::Remove { .. } => VaultAction::Remove,
        }
    }

    /// The secret name this operation targets.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Store { name, .. } | Self::Fetch { name } | Self::Remove { name } => name,
        }
    }
}

/// The broker's reply to one [`VaultOp`].
///
/// Replies are per-operation and carry no more than the op asked for: a
/// fetch returns one secret, never the store. Failures that would leak
/// internals (filesystem errors, oversize values) collapse to the opaque
/// [`VaultError`](VaultResult::VaultError): the agent learns that the op
/// failed, not why.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "r")]
pub enum VaultResult {
    /// The secret was stored.
    #[serde(rename = "stored")]
    Stored,
    /// The requested secret, base64-encoded. Exactly one secret.
    #[serde(rename = "fetched")]
    Fetched {
        /// Base64-encoded secret bytes.
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        secret_b64: crate::SecretB64,
    },
    /// The secret was removed (or was already absent).
    #[serde(rename = "removed")]
    Removed,
    /// No secret is stored under the name.
    #[serde(rename = "not-found")]
    NotFound,
    /// The broker holds no grant for this (action, name) pair. Checked
    /// before the vault is touched: an ungranted op never reaches the
    /// filesystem.
    #[serde(rename = "denied")]
    Denied,
    /// The vault backend failed, or the op was semantically rejected
    /// (e.g. an oversize value). Deliberately opaque.
    #[serde(rename = "vault-error")]
    VaultError,
}

/// A memory operation: the agent's post-handshake memory vocabulary.
///
/// Each variant names exactly one item and performs exactly one
/// action. There is deliberately no list, bulk-read, or dump variant —
/// the protocol cannot express enumeration, so a compromised agent
/// cannot bulk-read the store. The no-list rule applies by
/// analogy: scoped reads, no dumps.
///
/// Public because the memory protocol is a cross-crate contract: the
/// agent, the broker, and test fixtures all speak it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum MemoryOp {
    /// Stores `content` under (`scope`, `id`), replacing any existing
    /// value. `retention` is `keep` or `ttl-secs:<n>`: there is no
    /// default, the caller names it explicitly.
    #[serde(rename = "write")]
    Write {
        /// `user`, `project`, or `session`. The broker re-validates:
        /// the agent's bytes are never trusted.
        scope: String,
        /// The item id within the scope.
        id: String,
        /// The item content.
        content: String,
        /// Tags for scoped recall.
        tags: Vec<String>,
        /// `keep` or `ttl-secs:<n>`.
        retention: String,
    },
    /// Recalls the item stored under (`scope`, `id`).
    #[serde(rename = "recall")]
    Recall {
        /// `user`, `project`, or `session`.
        scope: String,
        /// The item id within the scope.
        id: String,
    },
    /// Recalls items under `scope` carrying `tag`, ordered by id, one
    /// page per op. Scoped, not a dump: the caller must already know the
    /// tag.
    #[serde(rename = "recall-by-tag")]
    RecallByTag {
        /// `user`, `project`, or `session`.
        scope: String,
        /// The tag to match.
        tag: String,
        /// Resume after this cursor (a memory id): the previous page's
        /// `next`. Absent for the first page.
        after: Option<String>,
    },
    /// Removes the item stored under (`scope`, `id`). Idempotent:
    /// removing a missing item reports
    /// [`Removed`](MemoryResult::Removed) with `removed: false`.
    #[serde(rename = "remove")]
    Remove {
        /// `user`, `project`, or `session`.
        scope: String,
        /// The item id within the scope.
        id: String,
    },
    /// Appends `text` to the session's append-only context log.
    #[serde(rename = "context-append")]
    ContextAppend {
        /// The note to append.
        text: String,
    },
    /// Reads the session's context entries in sequence order, one page
    /// per op.
    #[serde(rename = "context-read")]
    ContextRead {
        /// Resume after this cursor (a sequence number): the previous
        /// page's `next`. Absent for the first page.
        after: Option<String>,
    },
}

/// The broker's reply to one [`MemoryOp`].
///
/// Mirrors the request: write ops get [`Stored`](MemoryResult::Stored),
/// recall ops get [`Item`](MemoryResult::Item) or
/// [`NotFound`](MemoryResult::NotFound).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result")]
pub enum MemoryResult {
    /// The item was stored.
    #[serde(rename = "stored")]
    Stored,
    /// The requested item, as JSON. Exactly one item.
    #[serde(rename = "item")]
    Item {
        /// JSON-encoded memory item.
        item_json: String,
    },
    /// One page of items matching the tag, each as JSON. Scoped
    /// recall, not a dump.
    #[serde(rename = "items")]
    Items {
        /// JSON-encoded memory items.
        items_json: Vec<String>,
        /// The cursor to pass as `after` for the next page, or `None`
        /// when this page ends the result.
        next: Option<String>,
    },
    /// No item is stored under the id.
    #[serde(rename = "not-found")]
    NotFound,
    /// The item was removed; `removed` reports whether one existed.
    #[serde(rename = "removed")]
    Removed {
        /// True if an item was actually removed.
        removed: bool,
    },
    /// The note was appended to the context log.
    #[serde(rename = "context-appended")]
    ContextAppended,
    /// One page of the session's context entries, each as JSON, in
    /// sequence order.
    #[serde(rename = "context-entries")]
    ContextEntries {
        /// JSON-encoded context entries.
        entries_json: Vec<String>,
        /// The cursor to pass as `after` for the next page, or `None`
        /// when this page ends the log.
        next: Option<String>,
    },
    /// The broker holds no grant for this op. Checked before the store
    /// is touched: an ungranted op never reaches the filesystem.
    #[serde(rename = "denied")]
    Denied,
    /// The memory backend failed, or the op was semantically rejected
    /// (e.g. an oversize value). Deliberately opaque.
    #[serde(rename = "memory-error")]
    MemoryError,
}

/// A goals operation: the agent's post-handshake write vocabulary for
/// the goals and tracking store.
///
/// Every variant writes, and nothing else does: reads are a separate
/// session kind ([`QueryOp`]). A read-only holder — the feed — speaks a
/// vocabulary with no write in it, and no goals op can be spelled as a
/// query. Every op is bound to the broker-asserted peer uid; none names
/// a user.
///
/// Public because the goals protocol is a cross-crate contract: the
/// agent, the broker, and test fixtures all speak it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum GoalsOp {
    /// Records a new open goal the user explicitly asked for. The broker
    /// mints the id: a create never overwrites a goal.
    #[serde(rename = "goal-create")]
    GoalCreate {
        /// One-line title.
        title: String,
        /// Free-form detail; may be empty.
        detail: String,
    },
    /// Changes the title and/or detail of an open goal. At least one
    /// must be set.
    #[serde(rename = "goal-update")]
    GoalUpdate {
        /// The goal id, as minted at create.
        id: String,
        /// A new title, or `None` to keep it.
        title: Option<String>,
        /// A new detail, or `None` to keep it.
        detail: Option<String>,
    },
    /// Closes an open goal with the outcome the user named. Final: a
    /// closed goal is kept but never changes again.
    #[serde(rename = "goal-close")]
    GoalClose {
        /// The goal id.
        id: String,
        /// `achieved` or `abandoned`.
        outcome: String,
    },
    /// Appends one entry to the user's append-only tracking log.
    #[serde(rename = "tracking-append")]
    TrackingAppend {
        /// Where the report came from (`user`, `whatsapp`, …), carried
        /// verbatim as the entry's original source.
        source: String,
        /// What happened.
        text: String,
    },
}

/// The broker's reply to one [`GoalsOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result")]
pub enum GoalsResult {
    /// The goal was recorded under the broker-minted `id`.
    #[serde(rename = "goal-created")]
    GoalCreated {
        /// The new goal's id.
        id: String,
    },
    /// The update or close was applied.
    #[serde(rename = "goal-changed")]
    GoalChanged,
    /// The user has no goal with this id.
    #[serde(rename = "goal-not-found")]
    GoalNotFound,
    /// The goal is closed and can no longer change.
    #[serde(rename = "goal-already-closed")]
    GoalAlreadyClosed,
    /// The entry was appended at sequence number `seq`.
    #[serde(rename = "tracking-appended")]
    TrackingAppended {
        /// The entry's position in the user's log.
        seq: u64,
    },
    /// The op's content was refused: an empty or oversize field, a
    /// control character, or an update that changes nothing.
    #[serde(rename = "rejected")]
    Rejected,
    /// The user is at a store bound (goals held, or tracking entries).
    #[serde(rename = "limit-reached")]
    LimitReached,
    /// The broker holds no grant for this op. Checked before the store
    /// is touched.
    #[serde(rename = "denied")]
    Denied,
    /// The store backend failed. Deliberately opaque.
    #[serde(rename = "goals-error")]
    GoalsError,
}

/// A read-scoped query: the vocabulary the feed reads through.
///
/// Every variant reads, and nothing else is expressible here: the
/// writes live in [`GoalsOp`], a different session kind. Each query
/// selects one store; every reply item is a feed item carrying its
/// provenance (which store, and its original source) from the store
/// layer.
///
/// `after` resumes a paged query past the `next` cursor of the previous
/// reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum QueryOp {
    /// The user's goals, ordered by id.
    #[serde(rename = "goals")]
    Goals {
        /// `open`, `closed`, or `any`.
        status: String,
        /// Only goals changed at or after this unix time.
        updated_since_secs: Option<u64>,
        /// Resume after this cursor (a goal id).
        after: Option<String>,
    },
    /// The user's tracking entries, in log order.
    #[serde(rename = "tracking")]
    Tracking {
        /// Only entries recorded at or after this unix time.
        since_secs: Option<u64>,
        /// Only entries recorded before this unix time.
        until_secs: Option<u64>,
        /// Only entries reported from this source.
        source: Option<String>,
        /// Resume after this cursor (a sequence number).
        after: Option<String>,
    },
    /// The user's memory items carrying `tag`, ordered by id. Tag
    /// scoped like every memory read, and gated by the memory grants.
    #[serde(rename = "memory")]
    Memory {
        /// The tag to recall.
        tag: String,
        /// Resume after this cursor (a memory id).
        after: Option<String>,
    },
}

/// The broker's reply to one [`QueryOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result")]
pub enum QueryResult {
    /// One page of feed items, each as JSON with its provenance.
    #[serde(rename = "items")]
    Items {
        /// JSON-encoded feed items.
        items_json: Vec<String>,
        /// The cursor to pass as `after` for the next page, or `None`
        /// when this page ends the result.
        next: Option<String>,
    },
    /// The broker holds no grant for this query.
    #[serde(rename = "denied")]
    Denied,
    /// The store backend failed. Deliberately opaque.
    #[serde(rename = "query-error")]
    QueryError,
}

/// A schedule operation: the agent's vocabulary for the
/// broker-owned schedule store.
///
/// There is no list, read, or describe op: the store is broker-private,
/// and the cell never sees it, not even read-only. It creates a schedule
/// (and learns only its id) and cancels one by that id. Every op is
/// bound to the broker-asserted peer; a schedule's grants must be a
/// subset of the creator's, checked when it is created and again when
/// it fires.
///
/// Public because the schedule protocol is a cross-crate contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum ScheduleOp {
    /// Create a schedule.
    #[serde(rename = "create")]
    Create {
        /// When it fires.
        when: ScheduleSpec,
        /// The task the scheduled run is given. Untrusted text: it
        /// enters the run below the system prompt, as a user task.
        task: String,
        /// The capabilities the run may use: a subset of the creator's.
        grants: Vec<ScheduleGrant>,
        /// The channel the run's output is delivered to. Must be one the
        /// grants may send on.
        channel: String,
    },
    /// Create a feed: a schedule whose run is a digest of the
    /// user's own records, generated by the broker, read-only.
    ///
    /// There is no task and no grants field: the broker derives the read
    /// grants from `feed` (one per section), so a feed can never hold a
    /// write. They must be a subset of the creator's, as for `Create`.
    #[serde(rename = "create-feed")]
    CreateFeed {
        /// When it fires.
        when: ScheduleSpec,
        /// The channel each digest is delivered to.
        channel: String,
        /// What each digest gathers.
        feed: FeedSpec,
    },
    /// Cancel a schedule by the id `Create` returned.
    #[serde(rename = "cancel")]
    Cancel {
        /// The schedule id.
        id: String,
    },
    /// List the schedules the caller may see, a page at a time:
    /// those whose every grant the caller holds, in id order. Read-only;
    /// nothing says how many others exist.
    #[serde(rename = "list")]
    List {
        /// Resume after this schedule id: a previous page's `next`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
    /// Ask for one follow-up of this run: a one-shot run of
    /// `task`, `delay_secs` from now, delivered to this run's channel.
    ///
    /// Only a scheduled or hook run's own session may ask, and only once
    /// per run. The broker knows the run's grants and its depth in the
    /// follow-up chain; the request carries neither. `grants` must be a
    /// subset of the *run's* grants, and a run at the chain's depth bound
    /// is refused.
    #[serde(rename = "followup")]
    Followup {
        /// The follow-up run's task. Untrusted text, as for `Create`.
        task: String,
        /// How long from now it fires, in seconds.
        delay_secs: u64,
        /// The capabilities it may use: a subset of this run's.
        grants: Vec<ScheduleGrant>,
    },
    /// Mark a schedule's fire of unknown outcome resolved: the owner's
    /// word, after looking at what it did (WO-025). The schedule resumes
    /// its cadence; a one-shot is done. The owner's alone.
    #[serde(rename = "resolve")]
    Resolve {
        /// The schedule id.
        id: String,
    },
    /// Run a schedule's fire of unknown outcome again, as a new attempt:
    /// it is due now, and runs as any fire does, under the grants the
    /// creator holds then (WO-025). The owner's alone.
    #[serde(rename = "retry")]
    Retry {
        /// The schedule id.
        id: String,
    },
    /// List the results that couldn't be delivered, kept for the owner, a
    /// page at a time, oldest first. The owner's alone.
    #[serde(rename = "list-parked")]
    ListParked {
        /// Resume after this parked result: a previous page's `next`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
    /// Dismiss a parked result, once the owner has read it. The owner's
    /// alone.
    #[serde(rename = "dismiss")]
    Dismiss {
        /// The parked result's id, as `ListParked` gave it.
        id: String,
    },
}

/// What a feed digest gathers: the user's own tuning, as rules.
///
/// Each section reads one store through the read-only query path, and
/// needs that query's grant. At least one section must be on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FeedSpec {
    /// Goals changed in the window: `open` or `any`; absent for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goals: Option<String>,
    /// Tracking entries recorded in the window.
    #[serde(default)]
    pub tracking: bool,
    /// Only tracking entries reported from this source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_source: Option<String>,
    /// Memory items carrying these tags, in the user scope.
    #[serde(default)]
    pub memory_tags: Vec<String>,
    /// How far back each digest looks, in hours.
    pub lookback_hours: u32,
    /// Most items in one digest.
    pub max_items: u32,
}

/// What a hook fires on: a closed set of event sources, each
/// named by the broker when it observes the event.
///
/// There is no free-form source. A string-typed source could be claimed
/// by anything that can write text; these can only be asserted by the
/// broker code that observes them. A frame naming any other source does
/// not decode, and ends the session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "source")]
pub enum HookSource {
    /// A message arrived on a local channel through the broker's
    /// authenticated ingress.
    #[serde(rename = "channel-message")]
    ChannelMessage {
        /// The channel id.
        channel: String,
    },
    /// A run of one schedule finished.
    #[serde(rename = "schedule-run-finished")]
    ScheduleRunFinished {
        /// The schedule's id.
        schedule_id: String,
    },
}

impl fmt::Display for HookSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelMessage { channel } => write!(f, "channel-message:{channel}"),
            Self::ScheduleRunFinished { schedule_id } => {
                write!(f, "schedule-run-finished:{schedule_id}")
            }
        }
    }
}

/// A hook operation: the agent's vocabulary for the
/// broker-owned hook registry.
///
/// As for schedules, there is no list or read op: the registry is
/// broker-private. Hooks are created and cancelled through the
/// peer-authorized session only, never from a scheduled run's session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum HookOp {
    /// Register a hook.
    #[serde(rename = "create")]
    Create {
        /// The event it fires on.
        source: HookSource,
        /// The task a hook run is given. Untrusted text.
        task: String,
        /// The capabilities a hook run may use: a subset of the
        /// creator's, checked when it is created and again when it fires.
        grants: Vec<ScheduleGrant>,
        /// The channel a hook run's output is delivered to. Must be one
        /// the grants may send on.
        channel: String,
    },
    /// Cancel a hook by the id `Create` returned.
    #[serde(rename = "cancel")]
    Cancel {
        /// The hook id.
        id: String,
    },
}

/// The broker's reply to one [`HookOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result")]
pub enum HookResult {
    /// The hook was registered; this is its id.
    #[serde(rename = "created")]
    Created {
        /// The hook id, for `Cancel`.
        id: String,
    },
    /// The hook was cancelled.
    #[serde(rename = "cancelled")]
    Cancelled,
    /// No hook has that id.
    #[serde(rename = "not-found")]
    NotFound,
    /// The create was refused, for a reason in the request itself.
    #[serde(rename = "refused")]
    Refused {
        /// Why.
        reason: ScheduleRefusal,
    },
    /// Hooks are not available to this session.
    #[serde(rename = "denied")]
    Denied,
    /// The store backend failed. Deliberately opaque.
    #[serde(rename = "hook-error")]
    HookError,
}

/// When a schedule fires. Every form names an IANA timezone explicitly:
/// there is no server-local default that shifts when the host moves.
///
/// `tz` is required, in the type and in the published schema. A
/// request that leaves it out or sends `null` still decodes, with an empty
/// name, so the broker refuses it by name (`missing-timezone`) instead of
/// failing the whole frame; nothing is ever defaulted.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "every")]
pub enum ScheduleSpec {
    /// A five-field cron expression (`minute hour day-of-month month
    /// day-of-week`), evaluated in `tz`.
    #[serde(rename = "cron")]
    Cron {
        /// The cron expression.
        expr: String,
        /// The IANA timezone (`Europe/Berlin`).
        tz: String,
    },
    /// Once, at a local date and time (`2026-10-01T09:00`) in `tz`.
    #[serde(rename = "once")]
    Once {
        /// The local date and time.
        at: String,
        /// The IANA timezone, as for cron.
        tz: String,
    },
}

impl<'de> Deserialize<'de> for ScheduleSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        WireSpec::deserialize(deserializer).map(Self::from)
    }
}

/// [`ScheduleSpec`] as it decodes: the same forms, with `tz` possibly
/// absent or `null`, which becomes the empty name the broker refuses.
#[derive(Deserialize)]
#[serde(tag = "every")]
enum WireSpec {
    #[serde(rename = "cron")]
    Cron {
        expr: String,
        #[serde(default)]
        tz: Option<String>,
    },
    #[serde(rename = "once")]
    Once {
        at: String,
        #[serde(default)]
        tz: Option<String>,
    },
}

impl From<WireSpec> for ScheduleSpec {
    fn from(wire: WireSpec) -> Self {
        match wire {
            WireSpec::Cron { expr, tz } => Self::Cron {
                expr,
                tz: tz.unwrap_or_default(),
            },
            WireSpec::Once { at, tz } => Self::Once {
                at,
                tz: tz.unwrap_or_default(),
            },
        }
    }
}

/// One capability a scheduled run may hold. The vocabulary mirrors the
/// broker's own grants, so "subset of the creator's" is an exact set
/// comparison.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "grant")]
pub enum ScheduleGrant {
    /// A vault grant: `action` on the secret `name`.
    #[serde(rename = "vault")]
    Vault {
        /// The vault action (`fetch`, `store`, `remove`).
        action: String,
        /// The secret name.
        name: String,
    },
    /// An egress grant for one `host:port` destination.
    #[serde(rename = "egress")]
    Egress {
        /// The destination.
        destination: String,
    },
    /// A memory grant: `action` in `scope`.
    #[serde(rename = "memory")]
    Memory {
        /// The memory scope (`user`, `project`, `session`).
        scope: String,
        /// The memory action.
        action: String,
    },
    /// A goals or query grant, by action name.
    #[serde(rename = "goals")]
    Goals {
        /// The action.
        action: String,
    },
    /// Sending on a channel.
    #[serde(rename = "channel-send")]
    ChannelSend {
        /// The channel id.
        channel: String,
    },
}

/// The broker's reply to one [`ScheduleOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result")]
pub enum ScheduleResult {
    /// The schedule was stored; this is its id.
    #[serde(rename = "created")]
    Created {
        /// The schedule id, for `Cancel`.
        id: String,
    },
    /// The schedule was cancelled.
    #[serde(rename = "cancelled")]
    Cancelled,
    /// No schedule has that id.
    #[serde(rename = "not-found")]
    NotFound,
    /// One page of the schedules the caller may see.
    #[serde(rename = "listed")]
    Listed {
        /// The schedules, in id order.
        schedules: Vec<ScheduleSummary>,
        /// The `after` that resumes the list; absent on its last page.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<String>,
    },
    /// The create was refused, for a reason in the cell's own request.
    #[serde(rename = "refused")]
    Refused {
        /// Why.
        reason: ScheduleRefusal,
    },
    /// Scheduling is not enabled on this broker.
    #[serde(rename = "denied")]
    Denied,
    /// The store backend failed. Deliberately opaque.
    #[serde(rename = "schedule-error")]
    ScheduleError,
    /// The schedule's fire of unknown outcome is marked resolved.
    #[serde(rename = "resolved")]
    Resolved,
    /// The schedule's fire of unknown outcome is due again, as a new
    /// attempt.
    #[serde(rename = "retried")]
    Retried,
    /// One page of the parked results.
    #[serde(rename = "parked")]
    Parked {
        /// The parked results, oldest first.
        parked: Vec<ParkedSummary>,
        /// The `after` that resumes the list; absent on its last page.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<String>,
    },
    /// The parked result is dismissed.
    #[serde(rename = "dismissed")]
    Dismissed,
}

/// A result that couldn't be delivered, kept for the owner (WO-025).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ParkedSummary {
    /// Its id, for `Dismiss`.
    pub id: String,
    /// The schedule (or hook) it came from.
    pub schedule_id: String,
    /// The channel it was for.
    pub channel: String,
    /// When its fire was due, in Unix seconds.
    pub due: i64,
    /// Why it was parked.
    pub reason: String,
    /// The run's output, or what went wrong, bounded.
    pub text: String,
}

/// A schedule's fire of unknown outcome: claimed or running when a broker
/// stopped (WO-025). It holds the schedule until the owner resolves or
/// retries it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HeldFire {
    /// When it was due, in Unix seconds.
    pub due: i64,
    /// Whether its run had been started.
    pub started: bool,
}

/// What a schedule list shows of one schedule: enough to inspect
/// and cancel it. The task text, untrusted run input, is not shown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScheduleSummary {
    /// The id, for `Cancel`.
    pub id: String,
    /// What its runs are.
    pub kind: ScheduleKind,
    /// When it fires.
    pub when: ScheduleSpec,
    /// Where a run's output goes.
    pub channel: String,
    /// The grants a run holds.
    pub grants: Vec<ScheduleGrant>,
    /// The next fire, in Unix seconds, broker clock.
    pub next_due: i64,
    /// Its fire of unknown outcome, when it has one: the schedule is held
    /// until the owner resolves or retries it (WO-025).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<HeldFire>,
}

/// What a schedule's runs are.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ScheduleKind {
    /// An agent run of the schedule's task.
    #[serde(rename = "task")]
    Task,
    /// A digest the broker builds.
    #[serde(rename = "feed")]
    Feed,
}

/// Why a create was refused. Each names a fault in the request itself,
/// so saying which leaks nothing the cell did not send.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ScheduleRefusal {
    /// A requested grant is not one the creator holds.
    #[serde(rename = "exceeds-grants")]
    ExceedsGrants,
    /// No timezone was given.
    #[serde(rename = "missing-timezone")]
    MissingTimezone,
    /// The timezone is not a known IANA name.
    #[serde(rename = "unknown-timezone")]
    UnknownTimezone,
    /// The cron expression or date/time does not parse, or never fires.
    #[serde(rename = "invalid-spec")]
    InvalidSpec,
    /// A one-shot time is not in the future, or does not exist locally
    /// (inside a daylight-saving gap).
    #[serde(rename = "not-in-future")]
    NotInFuture,
    /// The grants do not include sending on the delivery channel.
    #[serde(rename = "channel-not-granted")]
    ChannelNotGranted,
    /// The task text is empty or over the bound; for a feed, its
    /// spec is unusable: no section on, an unknown goal status, a bad
    /// source label or tag, or a bound out of range.
    #[serde(rename = "bad-task")]
    BadTask,
    /// Too many grants on one schedule.
    #[serde(rename = "too-many-grants")]
    TooManyGrants,
    /// The store already holds the most schedules it keeps.
    #[serde(rename = "full")]
    Full,
    /// A hook names an event source the broker cannot authenticate: a
    /// channel without an authenticated ingress the creator reads, or
    /// a schedule that does not exist.
    #[serde(rename = "unknown-source")]
    UnknownSource,
    /// This run is already at the follow-up chain's depth bound.
    #[serde(rename = "max-followup-depth")]
    MaxFollowupDepth,
    /// This run already asked for its one follow-up.
    #[serde(rename = "followup-taken")]
    FollowupTaken,
    /// A follow-up's delay is zero or over the bound.
    #[serde(rename = "bad-delay")]
    BadDelay,
}

/// The action a [`VaultOp`] performs: the unit of grant matching.
///
/// Grants are (action, name) pairs, deny-by-default: the broker answers
/// [`Denied`](VaultResult::Denied) for any op it holds no grant for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VaultAction {
    /// Store (create or replace) a secret.
    Store,
    /// Fetch one named secret.
    Fetch,
    /// Remove one named secret.
    Remove,
}

impl FromStr for VaultAction {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "store" => Ok(Self::Store),
            "fetch" => Ok(Self::Fetch),
            "remove" => Ok(Self::Remove),
            _ => Err(Error::InvalidVaultAction(format!(
                "expected one of 'store', 'fetch', 'remove', got {raw:?}"
            ))),
        }
    }
}

impl fmt::Display for VaultAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Store => "store",
            Self::Fetch => "fetch",
            Self::Remove => "remove",
        };
        f.write_str(text)
    }
}

/// The size of one part of an egress body sent in parts: 32 KiB, which
/// base64 makes about 43 KiB, well within a frame with the message
/// around it.
pub const EGRESS_PART_BYTES: usize = 32_768;

/// An egress operation: the agent's post-handshake outbound-network
/// request vocabulary.
///
/// The agent never touches the network directly. It presents a
/// broker-minted surrogate (see `hatchling-egress`); the broker
/// validates the surrogate, checks the destination against the
/// allowlist, swaps in the real credential, and forwards the request.
///
/// There are two ops: `MintSurrogate` (ask the broker for a surrogate
/// for a destination the peer is granted for) and `Egress` (perform a
/// request with a previously minted surrogate). Two more move a body
/// too large for one frame (v13): `Stage` sends a request body's parts
/// before its `Egress` op, and `Read` fetches an answer's body after
/// its `Ok`, a part at a time.
///
/// Public because the egress protocol is a cross-crate contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum EgressOp {
    /// Mints a surrogate for `destination`. The broker checks the
    /// peer's grant for the destination, then the egress allowlist.
    /// The surrogate is bound to (`peer_uid`, `destination`, `expiry`).
    #[serde(rename = "mint-surrogate")]
    MintSurrogate {
        /// Destination `host:port` to mint the surrogate for.
        destination: String,
        /// Time-to-live in seconds.
        ttl_secs: u64,
    },
    /// Performs an outbound request with a previously minted surrogate.
    #[serde(rename = "egress")]
    Egress {
        /// HTTP method (`GET`, `POST`, etc.).
        method: String,
        /// Destination `host:port`. Must match the surrogate's binding
        /// and the allowlist.
        destination: String,
        /// Request path, starting with `/`.
        path: String,
        /// Request headers (name, value). `Authorization` and `Host`
        /// are forbidden — they come from the swap.
        headers: Vec<(String, String)>,
        /// Request body bytes, base64-encoded: all of it, or what
        /// follows the `staged` bytes. Bounded.
        body_b64: String,
        /// The broker-minted surrogate authorizing this request.
        surrogate_b64: String,
        /// How many bytes the body begins with, sent before this op in
        /// `Stage` parts (v13); 0 when `body_b64` is all of it. The
        /// broker refuses the request if its stage holds a different
        /// count, and empties the stage either way.
        #[serde(default)]
        staged: u64,
    },
    /// Adds a part of a request body to the session's stage (v13), for
    /// an `Egress` op to send. The stage holds at most the egress
    /// proxy's request bound.
    #[serde(rename = "stage")]
    Stage {
        /// The part, base64-encoded: at most [`EGRESS_PART_BYTES`] bytes.
        part_b64: String,
    },
    /// Reads the next part of the body the broker holds from the last
    /// `Ok` answer (v13): what its `more` said was left.
    #[serde(rename = "read")]
    Read,
}

/// The broker's reply to one [`EgressOp`].
///
/// Replies are per-operation. Failures that would leak internals
/// collapse to opaque variants: the agent learns that the op failed,
/// not why.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "r")]
pub enum EgressResult {
    /// A surrogate was minted.
    #[serde(rename = "surrogate")]
    Surrogate {
        /// The broker-minted surrogate, base64-encoded.
        surrogate_b64: String,
    },
    /// The egress request was forwarded; this is the bounded upstream
    /// response.
    #[serde(rename = "ok")]
    Ok {
        /// HTTP status code.
        status: u16,
        /// Response headers.
        headers: Vec<(String, String)>,
        /// Response body, base64-encoded, capped: all of it, or nothing
        /// when `more` says the broker holds it.
        body_b64: String,
        /// Bytes of the body the broker holds, because the whole answer
        /// wouldn't fit one frame (v13): read them with `Read` ops. 0
        /// when `body_b64` is all of it.
        #[serde(default)]
        more: u64,
    },
    /// A part was added to the stage (v13): how many bytes it holds now.
    #[serde(rename = "staged")]
    Staged {
        /// Bytes on the stage, this part included.
        held: u64,
    },
    /// The next part of a held answer's body (v13).
    #[serde(rename = "part")]
    Part {
        /// The part, base64-encoded: at most [`EGRESS_PART_BYTES`] bytes.
        body_b64: String,
        /// Bytes still held after this part; 0 when it ends the body.
        more: u64,
    },
    /// The op was denied: no grant, not on the allowlist, invalid
    /// surrogate, or no credential configured. The agent learns nothing
    /// beyond "no".
    #[serde(rename = "denied")]
    Denied,
    /// The upstream request failed, or the response was too large; or,
    /// for a body in parts, the stage would pass its bound, an `Egress`
    /// op's `staged` doesn't match it, or there is no part to read.
    /// Deliberately opaque.
    #[serde(rename = "egress-error")]
    EgressError,
}

/// A post-handshake session request: a vault op, an egress op, or a
/// channel op.
///
/// v4 unifies the session vocabulary. The `kind` tag discriminates:
/// `vault` frames carry a [`VaultOp`], `egress` frames carry an
/// [`EgressOp`], `channel` frames carry a [`ChannelOp`]. Unknown kinds
/// are protocol violations — the session ends, unknown traffic is never
/// acted on.
///
/// Public because the session protocol is a cross-crate contract.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind")]
pub enum SessionRequest {
    /// A vault operation.
    #[serde(rename = "vault")]
    Vault(VaultOp),
    /// An egress operation.
    #[serde(rename = "egress")]
    Egress(EgressOp),
    /// A channel operation.
    #[serde(rename = "channel")]
    Channel(ChannelOp),
    /// A memory operation.
    #[serde(rename = "memory")]
    Memory(MemoryOp),
    /// A skill operation.
    #[serde(rename = "skill")]
    Skill(SkillOp),
    /// A goals or tracking write.
    #[serde(rename = "goals")]
    Goals(GoalsOp),
    /// A read-scoped query over goals, tracking, and memory.
    #[serde(rename = "query")]
    Query(QueryOp),
    /// A schedule operation.
    #[serde(rename = "schedule")]
    Schedule(ScheduleOp),
    /// A hook operation.
    #[serde(rename = "hook")]
    Hook(HookOp),
    /// An artifact operation.
    #[serde(rename = "artifact")]
    Artifact(ArtifactOp),
    /// A space operation.
    #[serde(rename = "space")]
    Space(SpaceOp),
    /// A subagent operation.
    #[serde(rename = "subagent")]
    Subagent(crate::subagent::SubagentOp),
    /// A web operation (WO-028).
    #[serde(rename = "web")]
    Web(WebOp),
}

/// The broker's reply to one [`SessionRequest`].
///
/// Mirrors the request kind: vault ops get [`VaultResult`], egress ops
/// get [`EgressResult`], channel ops get [`ChannelResult`].
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind")]
pub enum SessionResponse {
    /// Reply to a vault op.
    #[serde(rename = "vault")]
    Vault(VaultResult),
    /// Reply to an egress op.
    #[serde(rename = "egress")]
    Egress(EgressResult),
    /// Reply to a channel op.
    #[serde(rename = "channel")]
    Channel(ChannelResult),
    /// Reply to a memory op.
    #[serde(rename = "memory")]
    Memory(MemoryResult),
    /// Reply to a skill op.
    #[serde(rename = "skill")]
    Skill(SkillResult),
    /// Reply to a goals op.
    #[serde(rename = "goals")]
    Goals(GoalsResult),
    /// Reply to a query.
    #[serde(rename = "query")]
    Query(QueryResult),
    /// Reply to a schedule op.
    #[serde(rename = "schedule")]
    Schedule(ScheduleResult),
    /// Reply to a hook op.
    #[serde(rename = "hook")]
    Hook(HookResult),
    /// Reply to an artifact op.
    #[serde(rename = "artifact")]
    Artifact(ArtifactResult),
    /// Reply to a space op.
    #[serde(rename = "space")]
    Space(SpaceResult),
    /// Reply to a subagent op.
    #[serde(rename = "subagent")]
    Subagent(crate::subagent::SubagentResult),
    /// Reply to a web op.
    #[serde(rename = "web")]
    Web(WebResult),
}

/// A space operation.
///
/// A space is a named, persistent scope with members. The broker mints
/// its id; the caller only gives it a display name. Every op that touches
/// a space names it **and** presents the admission ticket `Join` returned,
/// and the broker re-checks on every call that the ticket was minted for
/// exactly that space, in this live session, for this peer, and that the
/// session's broker-asserted principal is still a member. Naming another
/// caller's space gets nothing.
///
/// There is deliberately no op that lists, dumps, or exports spaces or a
/// space's items: the protocol cannot express enumeration.
///
/// Public because the space protocol is a cross-crate contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum SpaceOp {
    /// Create a space owned by the caller, holding `grants` for its tasks.
    #[serde(rename = "create")]
    Create {
        /// The display name. Untrusted text; never the space's identity.
        name: String,
        /// The grants the space's tasks may draw on: a subset of the
        /// caller's own.
        grants: Vec<ScheduleGrant>,
    },
    /// Join a space: admission, checked factor by factor.
    #[serde(rename = "join")]
    Join {
        /// The space id `Create` returned.
        space: String,
    },
    /// Store an item in a space.
    #[serde(rename = "put")]
    Put {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The item's key, a lowercase token.
        key: String,
        /// The item's value. Untrusted text.
        value: String,
    },
    /// Read an item from a space.
    #[serde(rename = "get")]
    Get {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The item's key.
        key: String,
    },
    /// Remove an item from a space.
    #[serde(rename = "remove")]
    Remove {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The item's key.
        key: String,
    },
    /// Add a task to a space: a work item a run can do with a subset of
    /// the space's grants.
    #[serde(rename = "add-task")]
    AddTask {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The task text the run is given.
        task: String,
        /// The grants the run may use: a subset of the space's.
        grants: Vec<ScheduleGrant>,
        /// Where the run's output goes; the grants must include sending
        /// there.
        channel: String,
    },
    /// Run a space's task: the broker queues a run bound to the space.
    #[serde(rename = "run-task")]
    RunTask {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The task id `AddTask` returned.
        task: String,
    },
    /// Issue a pairing code for a device to join this space:
    /// single-use, short-lived, and redeemed only on the broker's device
    /// endpoint.
    #[serde(rename = "pair")]
    Pair {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
    },
    /// Pull this space's changes since a version. Only the named
    /// space's items move, and only to a member.
    #[serde(rename = "sync-pull")]
    SyncPull {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The version the puller already has: changes after it come back.
        since: u64,
    },
    /// Push changes to this space: last writer wins, and every
    /// write it overwrote unseen goes to the conflict log.
    #[serde(rename = "sync-push")]
    SyncPush {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The changes, applied in order.
        changes: Vec<SyncChange>,
    },
    /// Read this space's conflict log after a sequence number.
    #[serde(rename = "conflicts")]
    Conflicts {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The last conflict already seen: entries after it come back.
        after: u64,
    },
    /// Upload media into this space, bounded and typed by the
    /// broker before it is admitted.
    #[serde(rename = "put-media")]
    PutMedia {
        /// What the uploader says it is: `image` or `audio`. The broker
        /// checks the bytes agree; it never takes a content type from the
        /// uploader.
        media_kind: String,
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The bytes, base64.
        data: String,
    },
    /// Fetch media from this space, with headers the broker
    /// sets.
    #[serde(rename = "get-media")]
    GetMedia {
        /// The space id.
        space: String,
        /// The admission ticket for that space.
        ticket: String,
        /// The media id `PutMedia` returned.
        media: String,
    },
}

/// One change a sync push carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SyncChange {
    /// The item's key.
    pub key: String,
    /// Its new value; `None` removes it.
    pub value: Option<String>,
    /// The item's version the writer last saw: 0 for an item it never
    /// saw. A different current version means someone else wrote first.
    pub base: u64,
}

/// One changed item a sync pull returns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SyncItem {
    /// The item's key.
    pub key: String,
    /// Its value; `None` if it was removed.
    pub value: Option<String>,
    /// Its version: the space's change counter when it was written.
    pub version: u64,
    /// Who wrote it, as the broker named them.
    pub writer: String,
}

/// One write that lost to a later one: what was overwritten,
/// kept so the loss is visible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConflictEntry {
    /// The conflict's sequence number in this space.
    pub seq: u64,
    /// The item's key.
    pub key: String,
    /// The overwritten value; `None` if it was a removal.
    pub lost: Option<String>,
    /// The overwritten version.
    pub lost_version: u64,
    /// Who wrote the overwritten value.
    pub lost_writer: String,
    /// Who overwrote it.
    pub winner: String,
    /// The version the winning write became.
    pub won_version: u64,
    /// When, in Unix seconds.
    pub at: i64,
}

/// One response header the broker sets on served media.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MediaHeader {
    /// The header name.
    pub name: String,
    /// Its value.
    pub value: String,
}

/// The broker's reply to one [`SpaceOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result")]
pub enum SpaceResult {
    /// The space was created; the caller is its owner and only member.
    #[serde(rename = "created")]
    Created {
        /// The broker-minted space id.
        space: String,
    },
    /// The caller was admitted to the space for this session.
    #[serde(rename = "admitted")]
    Admitted {
        /// The space id.
        space: String,
        /// The admission ticket: good for this space, in this session,
        /// for this peer, until it expires.
        ticket: String,
        /// When it expires, in Unix seconds.
        expires_at: i64,
    },
    /// The item was stored.
    #[serde(rename = "stored")]
    Stored,
    /// The item's value.
    #[serde(rename = "value")]
    Value {
        /// The value.
        value: String,
    },
    /// The item was removed.
    #[serde(rename = "removed")]
    Removed,
    /// The task was added.
    #[serde(rename = "task-added")]
    TaskAdded {
        /// The broker-minted task id.
        task: String,
    },
    /// The task's run was queued.
    #[serde(rename = "task-queued")]
    TaskQueued {
        /// The id of the one-shot schedule that runs it.
        schedule: String,
    },
    /// A pairing code for a device to redeem on the device endpoint.
    #[serde(rename = "pairing-code")]
    PairingCode {
        /// The code: single-use.
        code: String,
        /// When it expires, in Unix seconds.
        expires_at: i64,
    },
    /// This space's changes since the version asked for.
    #[serde(rename = "changes")]
    Changes {
        /// The changed items, oldest first.
        changes: Vec<SyncItem>,
        /// The newest version this reply covers: the next pull's `since`.
        head: u64,
        /// Whether there are more changes after `head`.
        more: bool,
    },
    /// The push was applied.
    #[serde(rename = "pushed")]
    Pushed {
        /// The space's version after it.
        head: u64,
        /// How many of its writes overwrote one the pusher had not seen;
        /// each is in the conflict log.
        conflicts: u32,
    },
    /// This space's conflict log after the sequence number asked for.
    #[serde(rename = "conflict-log")]
    ConflictLog {
        /// The entries, oldest first.
        entries: Vec<ConflictEntry>,
        /// How many older entries the bounded log has dropped.
        dropped: u64,
        /// Whether there are more entries after these.
        more: bool,
    },
    /// The media was admitted.
    #[serde(rename = "media-stored")]
    MediaStored {
        /// The broker-minted media id.
        media: String,
        /// The content type the broker assigned from the bytes.
        content_type: String,
        /// Its size in bytes.
        bytes: u32,
    },
    /// Media, as the broker serves it.
    #[serde(rename = "media")]
    Media {
        /// The media id.
        media: String,
        /// The content type the broker assigned when it was admitted.
        content_type: String,
        /// The bytes, base64.
        data: String,
        /// The headers to serve it with, all set by the broker.
        headers: Vec<MediaHeader>,
    },
    /// The op was refused, and nothing changed.
    #[serde(rename = "refused")]
    Refused {
        /// Why.
        reason: SpaceRefusal,
    },
    /// Spaces are not available to this session.
    #[serde(rename = "denied")]
    Denied,
    /// The store failed. Deliberately opaque.
    #[serde(rename = "space-error")]
    SpaceError,
}

/// Why a space op was refused.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SpaceRefusal {
    /// No space the caller is a member of has that id. The same reply
    /// for a space that does not exist and one that is someone else's,
    /// so naming cannot probe for spaces.
    #[serde(rename = "no-such-space")]
    NoSuchSpace,
    /// The ticket is not a live admission to this space.
    #[serde(rename = "not-admitted")]
    NotAdmitted,
    /// The ticket was minted in another session: a replayed or stale
    /// credential. Join again.
    #[serde(rename = "stale-admission")]
    StaleAdmission,
    /// The ticket was minted for another peer. Join again.
    #[serde(rename = "peer-changed")]
    PeerChanged,
    /// The admission has expired. Join again.
    #[serde(rename = "expired")]
    Expired,
    /// The display name is blank, too long, or has control characters.
    #[serde(rename = "bad-name")]
    BadName,
    /// The key is not a lowercase token.
    #[serde(rename = "bad-key")]
    BadKey,
    /// The value is too long.
    #[serde(rename = "bad-value")]
    BadValue,
    /// The space has no item with that key.
    #[serde(rename = "no-such-item")]
    NoSuchItem,
    /// A grant is not one the caller (for a space) or the space (for a
    /// task) holds, or the grants do not include sending on the channel.
    #[serde(rename = "exceeds-grants")]
    ExceedsGrants,
    /// The task text is blank or too long, or there are too many grants.
    #[serde(rename = "bad-task")]
    BadTask,
    /// The space has no task with that id.
    #[serde(rename = "no-such-task")]
    NoSuchTask,
    /// A bound was reached: spaces, items, tasks, or admissions.
    #[serde(rename = "full")]
    Full,
    /// This session may not do that: a task run may use only its own
    /// space, and may not create spaces or tasks.
    #[serde(rename = "not-allowed")]
    NotAllowed,
    /// Tasks cannot run: the broker has no scheduler.
    #[serde(rename = "scheduler-off")]
    SchedulerOff,
    /// A push carries too many changes.
    #[serde(rename = "too-many-changes")]
    TooManyChanges,
    /// The media is not base64, or empty.
    #[serde(rename = "bad-media")]
    BadMedia,
    /// The media is over the size bound.
    #[serde(rename = "media-too-large")]
    MediaTooLarge,
    /// The media's bytes are not a type the broker admits, or not the
    /// kind the uploader said.
    #[serde(rename = "wrong-media-type")]
    WrongMediaType,
    /// The space has no media with that id.
    #[serde(rename = "no-such-media")]
    NoSuchMedia,
}

/// What the broker's device endpoint sends first: a fresh nonce
/// to sign, and the broker's own device key, which a device pins at
/// pairing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DeviceChallenge {
    /// The protocol version.
    pub v: u16,
    /// A fresh nonce, hex: the device signs over it.
    pub nonce: String,
    /// The broker's device public key, base64.
    pub broker_key: String,
}

/// A device's answer to the challenge: pair with a code, or
/// authenticate as a paired device. Either way the device proves it
/// holds its key, and sends a nonce of its own for the broker to sign.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "hello")]
pub enum DeviceHello {
    /// Pair with a code the agent was given.
    #[serde(rename = "pair")]
    Pair {
        /// The pairing code.
        code: String,
        /// The device's public key, base64.
        device_key: String,
        /// The device's nonce, hex, for the broker to sign.
        nonce: String,
        /// The device's signature over the pairing transcript, base64.
        signature: String,
    },
    /// Authenticate as a paired device.
    #[serde(rename = "auth")]
    Auth {
        /// The device id the broker gave it at pairing.
        device: String,
        /// The device's nonce, hex, for the broker to sign.
        nonce: String,
        /// The device's signature over the auth transcript, base64.
        signature: String,
    },
}

/// The broker's answer to a device's hello.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "welcome")]
pub enum DeviceWelcome {
    /// Paired, and authenticated for this connection.
    #[serde(rename = "paired")]
    Paired {
        /// The device's id.
        device: String,
        /// The space it was paired into.
        space: String,
        /// The broker's signature over the device's nonce, base64.
        signature: String,
    },
    /// Authenticated for this connection.
    #[serde(rename = "authenticated")]
    Authenticated {
        /// The device's id.
        device: String,
        /// The broker's signature over the device's nonce, base64.
        signature: String,
    },
    /// Refused: the connection ends, and no state moved.
    #[serde(rename = "refused")]
    Refused {
        /// Why.
        reason: DeviceRefusal,
    },
}

/// Why the device endpoint refused a device.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DeviceRefusal {
    /// The pairing code is unknown, used, or expired. One reply for all
    /// three; the broker logs which.
    #[serde(rename = "bad-code")]
    BadCode,
    /// No paired device has that id.
    #[serde(rename = "unpaired")]
    Unpaired,
    /// The key or the signature does not verify.
    #[serde(rename = "bad-signature")]
    BadSignature,
    /// The broker already holds the most devices it keeps.
    #[serde(rename = "full")]
    Full,
    /// The broker's device store failed. Deliberately opaque.
    #[serde(rename = "device-error")]
    DeviceError,
}

/// An artifact operation: build an artifact from pinned
/// inputs, or deliver a completed one.
///
/// A build runs a builder, which is a registered skill declaring the
/// `artifact.build` capability, inside a skill cell. The broker pins the
/// inputs when it receives them, records provenance it observed itself,
/// and keeps the result. A delivery names a build by its id; the broker
/// re-checks that the build completed, re-hashes its bytes, and passes
/// them through the share-review gate before anything leaves.
///
/// Public because the artifact protocol is a cross-crate contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum ArtifactOp {
    /// Build an artifact.
    #[serde(rename = "build")]
    Build {
        /// The build kind, a lowercase token (`document`, `preview`, …).
        /// Not `kind`: that is the session request's own tag.
        build_kind: String,
        /// The builder: an exact registered skill id, `name@version`.
        builder: String,
        /// The inputs. The broker pins each by its content hash; the
        /// builder receives exactly these bytes.
        inputs: Vec<ArtifactInput>,
    },
    /// Deliver a completed build.
    #[serde(rename = "deliver")]
    Deliver {
        /// The build id `Build` returned.
        build: String,
        /// Where it goes.
        to: DeliveryTarget,
    },
    /// Describe a build as a UI shows it: who built it, as the
    /// broker can vouch for now, and what the inspector found.
    #[serde(rename = "describe")]
    Describe {
        /// The build id.
        build: String,
    },
    /// Capture an idea: its first revision.
    #[serde(rename = "capture-idea")]
    CaptureIdea {
        /// The idea's text. Untrusted.
        text: String,
        /// Where it was captured from. A feed item is re-read by
        /// the broker, which records its provenance; absent, the idea says
        /// its source was not stated.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<IdeaSource>,
    },
    /// List the ideas, a page at a time, oldest first.
    #[serde(rename = "list-ideas")]
    ListIdeas {
        /// Resume after this idea id: the previous page's `next`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
    /// Add a development note to an idea. A note withdraws any
    /// activation awaiting confirmation: what was proposed has changed.
    #[serde(rename = "note-idea")]
    NoteIdea {
        /// The idea id.
        idea: String,
        /// The note. Untrusted.
        note: String,
    },
    /// Propose activating an idea: the broker pins the exact
    /// build request (kind, builder, description, and the hash of every
    /// input) and answers with it, for the user to confirm.
    #[serde(rename = "propose-activation")]
    ProposeActivation {
        /// The idea id.
        idea: String,
        /// The build kind.
        build_kind: String,
        /// The builder: an exact registered skill id, `name@version`.
        builder: String,
        /// What the build is for, as the user will confirm it.
        description: String,
    },
    /// Activate a proposed activation, with the confirmation the
    /// broker minted when the user confirmed it. Builds exactly the
    /// pinned request.
    #[serde(rename = "activate")]
    Activate {
        /// The activation id `ProposeActivation` returned.
        activation: String,
        /// The broker-minted confirmation.
        confirmation: String,
    },
    /// Refine an idea: its text is replaced, and its revision moves on.
    #[serde(rename = "refine-idea")]
    RefineIdea {
        /// The idea id `CaptureIdea` returned.
        idea: String,
        /// The refined text. Untrusted.
        text: String,
    },
    /// Build from an idea's current revision, through the same pipeline
    /// as `Build`: the revision is pinned as the one input, `idea`.
    #[serde(rename = "build-idea")]
    BuildIdea {
        /// The idea id.
        idea: String,
        /// The build kind.
        build_kind: String,
        /// The builder: an exact registered skill id, `name@version`.
        builder: String,
    },
}

/// Where an idea was captured from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "source")]
pub enum IdeaSource {
    /// A chat with the user.
    #[serde(rename = "chat")]
    Chat,
    /// An explicit command from the user.
    #[serde(rename = "command")]
    Command,
    /// A feed item, which the broker re-reads to record its provenance.
    #[serde(rename = "feed")]
    Feed {
        /// The item's store: `goal`, `tracking`, or `memory`.
        store: String,
        /// The item's id: a goal id, a tracking sequence number, or a
        /// memory id.
        id: String,
        /// For a memory item, the tag it is read by.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
}

/// One build input: a name and its content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArtifactInput {
    /// The input's name, a lowercase token, unique within the build.
    pub name: String,
    /// The content. Untrusted text.
    pub content: String,
}

/// Where a delivery goes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "target")]
pub enum DeliveryTarget {
    /// A channel the session may send on.
    #[serde(rename = "channel")]
    Channel {
        /// The channel id.
        channel: String,
    },
    /// A file in the operator's artifact workspace.
    #[serde(rename = "workspace")]
    Workspace {
        /// The file name: one plain path component.
        name: String,
    },
}

/// The broker's reply to one [`ArtifactOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result")]
pub enum ArtifactResult {
    /// The build completed. Everything here is the broker's own record,
    /// not the builder's claim.
    #[serde(rename = "built")]
    Built {
        /// The build id, for `Deliver`.
        build: String,
        /// SHA-256 of the artifact's bytes, lowercase hex.
        output_hash: String,
        /// The builder that ran: its skill id, `name@version`.
        builder: String,
        /// The fingerprint of the key that signed the builder:
        /// the identity to show, not the builder's name.
        signer: String,
        /// How a UI names the builder, from the signing
        /// identity the broker verified.
        attribution: String,
        /// What the inspector found in the output. Advisory: it never
        /// decides a delivery.
        inspection: ArtifactInspection,
    },
    /// The build did not complete.
    #[serde(rename = "build-failed")]
    BuildFailed {
        /// Why.
        reason: BuildFailure,
    },
    /// The artifact was delivered.
    #[serde(rename = "delivered")]
    Delivered {
        /// The inspector's findings on what was delivered: a failed
        /// inspection does not stop a delivery the gate passed, so it is
        /// surfaced here instead.
        inspection: ArtifactInspection,
    },
    /// The delivery was refused, and nothing left the broker.
    #[serde(rename = "refused")]
    Refused {
        /// Why.
        reason: DeliveryRefusal,
    },
    /// A build, as a UI shows it.
    #[serde(rename = "described")]
    Described {
        /// The view.
        view: ArtifactView,
    },
    /// No build has that id.
    #[serde(rename = "unknown-build")]
    UnknownBuild,
    /// The idea was saved at this revision.
    #[serde(rename = "idea-saved")]
    IdeaSaved {
        /// The idea id.
        idea: String,
        /// Its revision, from 1.
        revision: u32,
    },
    /// A page of ideas, oldest first.
    #[serde(rename = "ideas")]
    Ideas {
        /// The ideas.
        ideas: Vec<IdeaSummary>,
        /// The cursor to pass as `after` for the next page, or `None`.
        next: Option<String>,
    },
    /// An activation was proposed: the pinned build request,
    /// for the user to confirm.
    #[serde(rename = "activation-proposed")]
    ActivationProposed {
        /// What was pinned.
        proposal: ActivationProposal,
    },
    /// An activation was pinned, but the owner wasn't asked to approve
    /// it (WO-027): why. Nothing will build it.
    #[serde(rename = "approval-refused")]
    ApprovalRefused {
        /// What was pinned.
        proposal: ActivationProposal,
        /// Why the owner wasn't asked.
        reason: ApprovalRefusal,
    },
    /// The idea op was refused.
    #[serde(rename = "idea-refused")]
    IdeaRefused {
        /// Why.
        reason: IdeaRefusal,
    },
    /// Artifacts are not available to this session.
    #[serde(rename = "denied")]
    Denied,
    /// The store failed. Deliberately opaque.
    #[serde(rename = "artifact-error")]
    ArtifactError,
}

/// Why a build did not complete.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum BuildFailure {
    /// The request is malformed: a bad kind or input name, a duplicate
    /// name, or too many or too large inputs.
    #[serde(rename = "bad-request")]
    BadRequest,
    /// No registered skill has that id.
    #[serde(rename = "unknown-builder")]
    UnknownBuilder,
    /// The skill does not declare the build capability: the skill gate
    /// refused it, and it never ran.
    #[serde(rename = "capability-denied")]
    CapabilityDenied,
    /// The builder ran and failed: it exited non-zero, timed out, or
    /// its output was too large.
    #[serde(rename = "builder-failed")]
    BuilderFailed,
    /// The builder's output is not a builder result: malformed, or
    /// carrying fields only the broker writes (such as provenance).
    #[serde(rename = "bad-output")]
    BadOutput,
    /// The store already holds the most builds it keeps.
    #[serde(rename = "full")]
    Full,
    /// No kind profile is registered for the build kind: the
    /// broker has nothing to inspect its output against.
    #[serde(rename = "unknown-kind")]
    UnknownKind,
}

/// Why an idea op was refused.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum IdeaRefusal {
    /// No idea has that id.
    #[serde(rename = "unknown-idea")]
    UnknownIdea,
    /// The text is blank or too long.
    #[serde(rename = "bad-text")]
    BadText,
    /// The store already holds the most ideas it keeps.
    #[serde(rename = "full")]
    Full,
    /// A note is blank, too long, or holds a control character
    /// .
    #[serde(rename = "bad-note")]
    BadNote,
    /// The idea already has the most notes it keeps.
    #[serde(rename = "too-many-notes")]
    TooManyNotes,
    /// An activation's description is blank, too long, or holds a
    /// control character.
    #[serde(rename = "bad-description")]
    BadDescription,
    /// An idea becomes a build only through a confirmed activation.
    #[serde(rename = "confirmation-required")]
    ConfirmationRequired,
    /// The confirmation is not one the broker minted for this
    /// activation: missing, forged, spent, or expired.
    #[serde(rename = "not-confirmed")]
    NotConfirmed,
    /// No activation awaits confirmation under that id: never proposed,
    /// already activated, or withdrawn because the idea changed.
    #[serde(rename = "no-proposal")]
    NoProposal,
    /// The feed item the idea was captured from cannot be read: no such
    /// item, or no grant to read it.
    #[serde(rename = "unreadable-source")]
    UnreadableSource,
}

/// Where an idea stands.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum IdeaState {
    /// Captured, not yet developed.
    #[serde(rename = "captured")]
    Captured,
    /// Refined or annotated since capture.
    #[serde(rename = "developing")]
    Developing,
    /// An activation awaits the user's confirmation.
    #[serde(rename = "awaiting-confirmation")]
    AwaitingConfirmation,
    /// Built through a confirmed activation, with nothing awaiting.
    #[serde(rename = "activated")]
    Activated,
}

/// An idea as a list shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct IdeaSummary {
    /// The idea id.
    pub idea: String,
    /// Its first line, cut short.
    pub title: String,
    /// Its current revision.
    pub revision: u32,
    /// How many development notes it has.
    pub notes: u32,
    /// Where it stands.
    pub state: IdeaState,
    /// Where it was captured from, as a label (`captured from the feed:
    /// from tracking (originally from whatsapp)`).
    pub origin: String,
}

/// One pinned input of an activation: its name and content hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActivationInput {
    /// The input's name.
    pub name: String,
    /// SHA-256 of its content, lowercase hex.
    pub sha256: String,
}

/// An activation's pinned build request: the typed handoff to
/// the artifact pipeline, exactly as the user confirms it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActivationProposal {
    /// The activation id.
    pub activation: String,
    /// The idea revision it builds.
    pub idea: IdeaRef,
    /// The build kind.
    pub build_kind: String,
    /// The builder.
    pub builder: String,
    /// What the build is for.
    pub description: String,
    /// Every input, by name and hash.
    pub inputs: Vec<ActivationInput>,
    /// The pinned request's hash: its kind and every input.
    pub request_hash: String,
}

/// Why a proposal wasn't put to the owner for approval (WO-027).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRefusal {
    /// The owner denied this action. Asking again won't change that.
    Denied,
    /// Approval requests are paused for a while: after a denial, or
    /// past the hourly limit.
    Paused,
    /// Too many requests are waiting for the owner, or one of this
    /// session's is.
    QueueFull,
    /// The broker couldn't make the request.
    Unavailable,
}

/// A web operation (WO-028): a page read through the broker, from a site
/// the owner allows.
///
/// A site is an origin: `https`, a host and a port. A page from a site the
/// session hasn't been allowed is put to the owner first, in the app; the
/// agent is told it's waiting, and nothing is fetched until the owner
/// says yes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum WebOp {
    /// Read one page.
    #[serde(rename = "fetch")]
    Fetch {
        /// The page's absolute `https` URL.
        url: String,
        /// Why the agent wants it. Shown to the owner when the site needs
        /// their approval, labeled as the agent's words.
        reason: String,
    },
}

/// The broker's reply to one [`WebOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result")]
pub enum WebResult {
    /// The page, as text. The site's words: data, never instructions.
    #[serde(rename = "page")]
    Page {
        /// The URL it was read from.
        url: String,
        /// The HTTP status.
        status: u16,
        /// Its text: HTML reduced to what it shows, other text as it is.
        text: String,
        /// Whether the text was cut short.
        truncated: bool,
    },
    /// The page redirects to another site, and the broker didn't follow:
    /// fetch `to` to ask for that site.
    #[serde(rename = "redirect")]
    Redirect {
        /// Where it leads.
        to: String,
    },
    /// The site isn't allowed yet. The owner has been asked in the app and
    /// hasn't decided: fetch again later.
    #[serde(rename = "waiting")]
    Waiting {
        /// The site, as the owner sees it.
        site: String,
        /// The approval request.
        request: String,
    },
    /// The owner said no to this site.
    #[serde(rename = "denied")]
    Denied {
        /// The site.
        site: String,
    },
    /// The owner didn't decide in time: fetch again to ask again.
    #[serde(rename = "expired")]
    Expired {
        /// The site.
        site: String,
    },
    /// The site couldn't be put to the owner.
    #[serde(rename = "unasked")]
    Unasked {
        /// Why.
        reason: ApprovalRefusal,
    },
    /// Refused or failed before a page: the URL, the operator's refusal of
    /// the site, or the fetch itself.
    #[serde(rename = "refused")]
    Refused {
        /// Why, in the broker's words.
        reason: String,
    },
    /// The operator hasn't turned the web on.
    #[serde(rename = "off")]
    Off,
}

/// The inspector's verdict on a build's output.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum InspectionVerdict {
    /// Every check passed.
    #[serde(rename = "pass")]
    Pass,
    /// At least one check found a problem.
    #[serde(rename = "fail")]
    Fail,
}

/// One thing the inspector found. It names the problem; it never quotes
/// the content, so surfacing it cannot carry the artifact's bytes
/// around the share-review gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InspectionFinding {
    /// The check that found it, a token (`size`, `title`, `script`, …).
    pub check: String,
    /// What it found, in the inspector's own words.
    pub detail: String,
}

/// What the inspector found in a build's output, as the broker
/// records it in provenance and surfaces it. Advisory only: delivery is
/// decided by the share-review gate alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArtifactInspection {
    /// Pass exactly when there are no findings.
    pub verdict: InspectionVerdict,
    /// The content type the build kind declares (`text/markdown`, …).
    pub content_type: String,
    /// What it found, in check order.
    pub findings: Vec<InspectionFinding>,
}

/// Which idea revision a build was made from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct IdeaRef {
    /// The idea id.
    pub idea: String,
    /// The revision built.
    pub revision: u32,
    /// SHA-256 of that revision's text, lowercase hex.
    pub sha256: String,
}

/// Where a build stands, as a UI shows it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum BuildState {
    /// Requested and not finished.
    #[serde(rename = "pending")]
    Pending,
    /// Finished and accepted.
    #[serde(rename = "completed")]
    Completed,
    /// Failed.
    #[serde(rename = "failed")]
    Failed,
}

/// A build as a UI shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArtifactView {
    /// The build id.
    pub build: String,
    /// The build kind.
    pub kind: String,
    /// Where it stands.
    pub state: BuildState,
    /// Who built it, as the broker can vouch for now; `None` until it
    /// completes.
    pub attribution: Option<String>,
    /// What the inspector found; `None` until it completes.
    pub inspection: Option<ArtifactInspection>,
    /// SHA-256 of its bytes; `None` until it completes.
    pub output_hash: Option<String>,
    /// The idea revision it was built from, if any.
    pub idea: Option<IdeaRef>,
    /// How many times it was delivered.
    pub delivered: u32,
}

/// Why a delivery was refused.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DeliveryRefusal {
    /// No build has that id.
    #[serde(rename = "unknown-build")]
    UnknownBuild,
    /// The build has not completed: it is pending or failed.
    #[serde(rename = "incomplete")]
    Incomplete,
    /// The stored bytes no longer match the output hash recorded at
    /// completion.
    #[serde(rename = "hash-mismatch")]
    HashMismatch,
    /// The share-review gate blocked the bytes.
    #[serde(rename = "blocked")]
    Blocked,
    /// The share-review gate could not judge (an error, or no review
    /// policy): fail-closed.
    #[serde(rename = "gate-error")]
    GateError,
    /// The target is not one this session can deliver to.
    #[serde(rename = "bad-target")]
    BadTarget,
    /// The gate passed, but writing to the target failed.
    #[serde(rename = "delivery-failed")]
    DeliveryFailed,
}

/// A skill operation: the agent's post-handshake skill vocabulary.
///
/// One op, one action: invoke one registered skill, exercising one
/// capability it declared, on one input. The broker owns the registry;
/// the agent names a skill and the broker decides. There is no list or
/// describe op: the agent learns which skills exist from its
/// configuration, not by asking the broker to enumerate them.
///
/// Public because the skill protocol is a cross-crate contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum SkillOp {
    /// Runs `skill` on `input`, exercising `capability`.
    #[serde(rename = "invoke")]
    Invoke {
        /// The exact registered skill id, `name@version`. Looked up
        /// verbatim: a bare name or another version matches nothing.
        skill: String,
        /// The declared capability this invocation exercises. Typed on
        /// the wire: a malformed name fails to decode, which makes the
        /// frame a protocol violation and ends the session.
        capability: CapabilityName,
        /// The skill's input. Opaque to the broker.
        input: String,
    },
}

/// The broker's reply to one [`SkillOp`].
///
/// Denials are typed and distinct, never silent: an unknown skill and an
/// undeclared capability are different answers. Everything else that can
/// go wrong collapses to the opaque [`SkillError`](SkillResult::SkillError).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "r")]
pub enum SkillResult {
    /// The skill ran; this is its output. Untrusted: the agent hands it
    /// to the model only as a tool result, inside the untrusted-content
    /// envelope, never as instructions.
    #[serde(rename = "output")]
    Output {
        /// The skill's output text.
        output: String,
    },
    /// No skill is registered under the exact id.
    #[serde(rename = "unknown-skill")]
    UnknownSkill,
    /// The skill is registered but did not declare the capability.
    /// Checked on every call, not only at admission.
    #[serde(rename = "capability-denied")]
    CapabilityDenied,
    /// The skill could not run, or failed. Deliberately opaque.
    #[serde(rename = "skill-error")]
    SkillError,
}

/// Largest skill input the protocol carries, in bytes.
///
/// Sized so that the worst-case encoded request (every byte a control
/// character, which JSON escapes as six) still fits one frame.
pub const MAX_SKILL_INPUT_BYTES: usize = 8 * 1024;

/// Largest skill output the protocol carries, in bytes. Sized like
/// [`MAX_SKILL_INPUT_BYTES`], for the reply.
pub const MAX_SKILL_OUTPUT_BYTES: usize = 8 * 1024;

/// A channel operation: the agent's post-handshake messaging vocabulary.
///
/// The agent never touches a channel socket directly. It asks the broker
/// to receive or send; the broker owns the endpoints, stamps identities,
/// and enforces the confirmation-rail boundary on state-changing sends.
///
/// Accounts: a channel that authenticates to its backend has
/// one or more accounts, whose secrets only the broker holds. The cell
/// names an account by the opaque handle [`Accounts`](ChannelOp::Accounts)
/// returned, never by anything secret; a credential-less channel (the
/// local one) takes no account.
///
/// Public because the channel protocol is a cross-crate contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op")]
pub enum ChannelOp {
    /// Receive the next inbound message on `channel`, or
    /// [`NoMessage`](ChannelResult::NoMessage) if none is waiting.
    #[serde(rename = "receive")]
    Receive {
        /// The broker-assigned channel id.
        channel: String,
        /// The account handle to receive as, for a credentialed
        /// channel; absent for a credential-less one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// List the accounts of `channel`, as broker-minted handles.
    #[serde(rename = "accounts")]
    Accounts {
        /// The broker-assigned channel id.
        channel: String,
    },
    /// Send `text` on `channel`. The broker stamps the sender from the
    /// authenticated peer before delivery — the agent cannot spoof it.
    ///
    /// The broker decides whether the send needs the confirmation rail
    ///: only a conversational reply — answering an inbound
    /// message the broker delivered on this channel, once — goes
    /// without an approval. Anything else is refused with
    /// [`Denied`](ChannelResult::Denied) unless `approval` carries a
    /// broker-minted token for exactly this channel and text.
    #[serde(rename = "send")]
    Send {
        /// The broker-assigned channel id.
        channel: String,
        /// The message text. Untrusted on receipt; the loop places it
        /// below the system prompt.
        text: String,
        /// The cell's own view that the message triggers external
        /// actions. It can put a send on the rail, never take one off:
        /// the broker classifies regardless.
        state_changing: bool,
        /// A broker-minted rail approval (`rail-v1-…`) for exactly this
        /// channel and text. Presenting one spends it; a string the
        /// broker did not mint, or an expired or mismatched token, is
        /// refused.
        approval: Option<String>,
        /// The account handle to send as, for a credentialed channel;
        /// absent for a credential-less one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
}

/// One account of a credentialed channel, as the cell sees it: a
/// broker-minted handle and the operator's label for it. Neither is
/// secret, and neither derives from the secret.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChannelAccount {
    /// Opaque, unguessable, valid on this session only.
    pub handle: String,
    /// The operator's name for the account (`work`, `personal`).
    pub label: String,
}

/// The broker's reply to one [`ChannelOp`].
///
/// Replies are per-operation and carry no more than the op asked for.
/// Failures that would leak internals collapse to opaque variants.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "r")]
pub enum ChannelResult {
    /// An inbound message. Sender and channel are broker-asserted.
    #[serde(rename = "message")]
    Message {
        /// Broker-stamped sender identity.
        sender: String,
        /// The channel this arrived on.
        channel: String,
        /// The message text.
        text: String,
    },
    /// No inbound message is waiting.
    #[serde(rename = "no-message")]
    NoMessage,
    /// The outbound message was delivered.
    #[serde(rename = "sent")]
    Sent,
    /// The op was denied: unknown channel, state-changing send without
    /// confirmation-rail approval, or no grant. The agent learns nothing
    /// beyond "no".
    #[serde(rename = "denied")]
    Denied,
    /// The channel backend failed. Deliberately opaque.
    #[serde(rename = "channel-error")]
    ChannelError,
    /// The channel's accounts, in label order. Empty for a
    /// credential-less channel.
    #[serde(rename = "accounts")]
    Accounts {
        /// One entry per configured account.
        accounts: Vec<ChannelAccount>,
    },
    /// The op named no account this session's broker minted for this
    /// channel: a forged, foreign, or missing handle on a credentialed
    /// channel, or any handle on a credential-less one. It tells the
    /// cell nothing it did not already hold.
    #[serde(rename = "unknown-account")]
    UnknownAccount,
    /// The broker's output gate redacted the message: a notice
    /// naming the category and the event was delivered in its place.
    /// The flagged text is not echoed back.
    #[serde(rename = "redacted")]
    Redacted {
        /// The category it was redacted under.
        category: String,
        /// The redaction's event id, which the operator reviews it by.
        event_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_op_action_and_name_cover_every_variant() {
        // Exhaustive: adding a VaultOp variant without updating
        // `action`/`name` fails to compile, and the assertions pin the
        // mapping each variant must keep.
        let store = VaultOp::Store {
            name: String::from("a"),
            secret_b64: String::from("Yg==").into(),
        };
        assert_eq!(store.action(), VaultAction::Store);
        assert_eq!(store.name(), "a");

        let fetch = VaultOp::Fetch {
            name: String::from("b"),
        };
        assert_eq!(fetch.action(), VaultAction::Fetch);
        assert_eq!(fetch.name(), "b");

        let remove = VaultOp::Remove {
            name: String::from("c"),
        };
        assert_eq!(remove.action(), VaultAction::Remove);
        assert_eq!(remove.name(), "c");
    }

    #[test]
    fn vault_op_serde_round_trip() {
        // Pins the wire tags: the JSON shape is the cross-crate contract.
        let op = VaultOp::Fetch {
            name: String::from("app-token"),
        };
        let json = serde_json::to_vec(&op).unwrap();
        assert_eq!(json, br#"{"op":"fetch","name":"app-token"}"#.as_slice());
        assert_eq!(serde_json::from_slice::<VaultOp>(&json).unwrap(), op);

        let result = VaultResult::Fetched {
            secret_b64: String::from("aGVsbG8=").into(),
        };
        let json = serde_json::to_vec(&result).unwrap();
        assert_eq!(
            json,
            br#"{"r":"fetched","secret_b64":"aGVsbG8="}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<VaultResult>(&json).unwrap(),
            result
        );
    }

    #[test]
    fn vault_op_rejects_unknown_tag() {
        // A frame claiming an op the protocol has no variant for must
        // not decode: unknown traffic is never acted on.
        let json = br#"{"op":"list","name":"x"}"#;
        assert!(serde_json::from_slice::<VaultOp>(json).is_err());
    }

    #[test]
    fn vault_action_parses_and_displays() {
        for (text, action) in [
            ("store", VaultAction::Store),
            ("fetch", VaultAction::Fetch),
            ("remove", VaultAction::Remove),
        ] {
            assert_eq!(text.parse::<VaultAction>().unwrap(), action);
            assert_eq!(action.to_string(), text);
        }
        assert!("list".parse::<VaultAction>().is_err());
        assert!("".parse::<VaultAction>().is_err());
    }

    #[test]
    fn egress_op_serde_round_trip() {
        // Pins the wire tags: the JSON shape is the cross-crate contract.
        let op = EgressOp::MintSurrogate {
            destination: String::from("api.github.com:443"),
            ttl_secs: 3600,
        };
        let json = serde_json::to_vec(&op).unwrap();
        assert_eq!(
            json,
            br#"{"op":"mint-surrogate","destination":"api.github.com:443","ttl_secs":3600}"#
                .as_slice()
        );
        assert_eq!(serde_json::from_slice::<EgressOp>(&json).unwrap(), op);

        let op = EgressOp::Egress {
            method: String::from("GET"),
            destination: String::from("api.github.com:443"),
            path: String::from("/user"),
            headers: vec![(String::from("Accept"), String::from("application/json"))],
            body_b64: String::new(),
            surrogate_b64: String::from("c3Vycm9nYXRl"),
            staged: 7,
        };
        let json = serde_json::to_vec(&op).unwrap();
        assert_eq!(serde_json::from_slice::<EgressOp>(&json).unwrap(), op);

        let result = EgressResult::Ok {
            status: 200,
            headers: vec![],
            body_b64: String::from("aGVsbG8="),
            more: 0,
        };
        let json = serde_json::to_vec(&result).unwrap();
        assert_eq!(
            json,
            br#"{"r":"ok","status":200,"headers":[],"body_b64":"aGVsbG8=","more":0}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<EgressResult>(&json).unwrap(),
            result
        );
    }

    #[test]
    fn egress_bodies_in_parts_have_their_own_tags() {
        // v13: a body too large for one frame moves in parts.
        let cases: [(EgressOp, &[u8]); 2] = [
            (
                EgressOp::Stage {
                    part_b64: String::from("cGFydA=="),
                },
                br#"{"op":"stage","part_b64":"cGFydA=="}"#,
            ),
            (EgressOp::Read, br#"{"op":"read"}"#),
        ];
        for (op, json) in cases {
            assert_eq!(serde_json::to_vec(&op).unwrap(), json);
            assert_eq!(serde_json::from_slice::<EgressOp>(json).unwrap(), op);
        }
        let cases: [(EgressResult, &[u8]); 2] = [
            (
                EgressResult::Staged { held: 32_768 },
                br#"{"r":"staged","held":32768}"#,
            ),
            (
                EgressResult::Part {
                    body_b64: String::from("cGFydA=="),
                    more: 4,
                },
                br#"{"r":"part","body_b64":"cGFydA==","more":4}"#,
            ),
        ];
        for (result, json) in cases {
            assert_eq!(serde_json::to_vec(&result).unwrap(), json);
            assert_eq!(
                serde_json::from_slice::<EgressResult>(json).unwrap(),
                result
            );
        }
        // A body sent whole needs neither field.
        let op: EgressOp = serde_json::from_slice(
            br#"{"op":"egress","method":"GET","destination":"d:443","path":"/","headers":[],"body_b64":"","surrogate_b64":"s"}"#,
        )
        .unwrap();
        assert!(matches!(op, EgressOp::Egress { staged: 0, .. }));
        let result: EgressResult =
            serde_json::from_slice(br#"{"r":"ok","status":200,"headers":[],"body_b64":""}"#)
                .unwrap();
        assert!(matches!(result, EgressResult::Ok { more: 0, .. }));
        assert_eq!(EGRESS_PART_BYTES, 32 << 10);
    }

    #[test]
    fn egress_op_rejects_unknown_tag() {
        // A frame claiming an op the protocol has no variant for must
        // not decode: unknown traffic is never acted on.
        let json = br#"{"op":"egress-admin","destination":"x"}"#;
        assert!(serde_json::from_slice::<EgressOp>(json).is_err());
    }

    #[test]
    fn session_request_serde_round_trip() {
        // The v3 session envelope: kind discriminates vault vs egress.
        let req = SessionRequest::Vault(VaultOp::Fetch {
            name: String::from("app-token"),
        });
        let json = serde_json::to_vec(&req).unwrap();
        assert_eq!(
            json,
            br#"{"kind":"vault","op":"fetch","name":"app-token"}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<SessionRequest>(&json).unwrap(),
            req
        );

        let req = SessionRequest::Egress(EgressOp::MintSurrogate {
            destination: String::from("api.github.com:443"),
            ttl_secs: 3600,
        });
        let json = serde_json::to_vec(&req).unwrap();
        assert_eq!(
            serde_json::from_slice::<SessionRequest>(&json).unwrap(),
            req
        );

        let resp = SessionResponse::Egress(EgressResult::Denied);
        let json = serde_json::to_vec(&resp).unwrap();
        assert_eq!(json, br#"{"kind":"egress","r":"denied"}"#.as_slice());
        assert_eq!(
            serde_json::from_slice::<SessionResponse>(&json).unwrap(),
            resp
        );
    }

    #[test]
    fn session_request_rejects_unknown_kind() {
        let json = br#"{"kind":"admin","op":"fetch","name":"x"}"#;
        assert!(serde_json::from_slice::<SessionRequest>(json).is_err());
    }

    #[test]
    fn channel_op_serde_round_trip() {
        // Pins the wire tags: the JSON shape is the cross-crate contract.
        let op = ChannelOp::Receive {
            channel: String::from("local"),
            account: None,
        };
        let json = serde_json::to_vec(&op).unwrap();
        assert_eq!(json, br#"{"op":"receive","channel":"local"}"#.as_slice());
        assert_eq!(serde_json::from_slice::<ChannelOp>(&json).unwrap(), op);

        let op = ChannelOp::Send {
            channel: String::from("local"),
            text: String::from("hello"),
            state_changing: false,
            approval: None,
            account: None,
        };
        let json = serde_json::to_vec(&op).unwrap();
        assert_eq!(serde_json::from_slice::<ChannelOp>(&json).unwrap(), op);

        let op = ChannelOp::Send {
            channel: String::from("local"),
            text: String::from("rm -rf /"),
            state_changing: true,
            approval: Some(String::from("rail-token-123")),
            account: None,
        };
        let json = serde_json::to_vec(&op).unwrap();
        assert_eq!(serde_json::from_slice::<ChannelOp>(&json).unwrap(), op);

        let result = ChannelResult::Message {
            sender: String::from("uid:1000"),
            channel: String::from("local"),
            text: String::from("hi"),
        };
        let json = serde_json::to_vec(&result).unwrap();
        assert_eq!(
            json,
            br#"{"r":"message","sender":"uid:1000","channel":"local","text":"hi"}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<ChannelResult>(&json).unwrap(),
            result
        );
    }

    #[test]
    fn channel_account_wire_forms_are_pinned() {
        // an account handle rides on receive and send, and is
        // omitted entirely for a credential-less channel — the forms
        // above are byte-identical to before accounts existed.
        let op = ChannelOp::Receive {
            channel: String::from("mail"),
            account: Some(String::from("acct-v1-ab")),
        };
        let json = serde_json::to_vec(&op).unwrap();
        assert_eq!(
            json,
            br#"{"op":"receive","channel":"mail","account":"acct-v1-ab"}"#.as_slice()
        );
        assert_eq!(serde_json::from_slice::<ChannelOp>(&json).unwrap(), op);

        let op = ChannelOp::Send {
            channel: String::from("mail"),
            text: String::from("hi"),
            state_changing: false,
            approval: None,
            account: Some(String::from("acct-v1-ab")),
        };
        let json = serde_json::to_vec(&op).unwrap();
        assert_eq!(
            json,
            br#"{"op":"send","channel":"mail","text":"hi","state_changing":false,"approval":null,"account":"acct-v1-ab"}"#
                .as_slice()
        );
        assert_eq!(serde_json::from_slice::<ChannelOp>(&json).unwrap(), op);

        let op = ChannelOp::Accounts {
            channel: String::from("mail"),
        };
        let json = serde_json::to_vec(&op).unwrap();
        assert_eq!(json, br#"{"op":"accounts","channel":"mail"}"#.as_slice());
        assert_eq!(serde_json::from_slice::<ChannelOp>(&json).unwrap(), op);

        let result = ChannelResult::Accounts {
            accounts: vec![ChannelAccount {
                handle: String::from("acct-v1-ab"),
                label: String::from("work"),
            }],
        };
        let json = serde_json::to_vec(&result).unwrap();
        assert_eq!(
            json,
            br#"{"r":"accounts","accounts":[{"handle":"acct-v1-ab","label":"work"}]}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<ChannelResult>(&json).unwrap(),
            result
        );

        let json = serde_json::to_vec(&ChannelResult::UnknownAccount).unwrap();
        assert_eq!(json, br#"{"r":"unknown-account"}"#.as_slice());
        assert_eq!(
            serde_json::from_slice::<ChannelResult>(&json).unwrap(),
            ChannelResult::UnknownAccount
        );

        let result = ChannelResult::Redacted {
            category: String::from("secret"),
            event_id: String::from("red-1-2-3"),
        };
        let json = serde_json::to_vec(&result).unwrap();
        assert_eq!(
            json,
            br#"{"r":"redacted","category":"secret","event_id":"red-1-2-3"}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<ChannelResult>(&json).unwrap(),
            result
        );
    }

    #[test]
    fn a_missing_or_null_timezone_decodes_as_the_empty_name() {
        // A missing or null timezone decodes, as the empty name, so the
        // broker can refuse it by name.
        for bare in [
            r#"{"every":"once","at":"2026-10-01T09:00"}"#,
            r#"{"every":"once","at":"2026-10-01T09:00","tz":null}"#,
        ] {
            assert_eq!(
                serde_json::from_str::<ScheduleSpec>(bare).unwrap(),
                ScheduleSpec::Once {
                    at: String::from("2026-10-01T09:00"),
                    tz: String::new(),
                }
            );
        }
        for bare in [
            r#"{"every":"cron","expr":"0 9 * * *"}"#,
            r#"{"every":"cron","expr":"0 9 * * *","tz":null}"#,
        ] {
            let mut decoder = serde_json::Deserializer::from_str(bare);
            assert_eq!(
                ScheduleSpec::deserialize(&mut decoder).unwrap(),
                ScheduleSpec::Cron {
                    expr: String::from("0 9 * * *"),
                    tz: String::new(),
                }
            );
        }
        // Anything else malformed still fails to decode.
        for bad in [
            r#"{"every":"cron","tz":"UTC"}"#,
            r#"{"every":"once","at":"x","tz":3}"#,
            r#"{"every":"weekly","tz":"UTC"}"#,
        ] {
            assert!(serde_json::from_str::<ScheduleSpec>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_schedule_list_wire_forms_are_pinned() {
        let list = SessionRequest::Schedule(ScheduleOp::List { after: None });
        assert_eq!(
            serde_json::to_string(&list).unwrap(),
            r#"{"kind":"schedule","op":"list"}"#
        );
        let resumed = SessionRequest::Schedule(ScheduleOp::List {
            after: Some(String::from("sch-1")),
        });
        let json = serde_json::to_string(&resumed).unwrap();
        assert_eq!(json, r#"{"kind":"schedule","op":"list","after":"sch-1"}"#);
        assert_eq!(
            serde_json::from_str::<SessionRequest>(&json).unwrap(),
            resumed
        );
        let listed = ScheduleResult::Listed {
            schedules: vec![ScheduleSummary {
                id: String::from("sch-1"),
                kind: ScheduleKind::Feed,
                when: ScheduleSpec::Cron {
                    expr: String::from("0 8 * * *"),
                    tz: String::from("UTC"),
                },
                channel: String::from("local"),
                grants: vec![ScheduleGrant::ChannelSend {
                    channel: String::from("local"),
                }],
                next_due: 1_790_000_000,
                held: None,
            }],
            next: Some(String::from("sch-1")),
        };
        let json = serde_json::to_string(&listed).unwrap();
        assert_eq!(
            json,
            r#"{"result":"listed","schedules":[{"id":"sch-1","kind":"feed","when":{"every":"cron","expr":"0 8 * * *","tz":"UTC"},"channel":"local","grants":[{"grant":"channel-send","channel":"local"}],"next_due":1790000000}],"next":"sch-1"}"#
        );
        assert_eq!(
            serde_json::from_str::<ScheduleResult>(&json).unwrap(),
            listed
        );
        assert_eq!(
            serde_json::to_string(&ScheduleKind::Task).unwrap(),
            r#""task""#
        );
    }

    #[test]
    fn schedule_wire_forms_are_pinned() {
        let create = SessionRequest::Schedule(ScheduleOp::Create {
            when: ScheduleSpec::Cron {
                expr: String::from("0 9 * * 1-5"),
                tz: String::from("Europe/Berlin"),
            },
            task: String::from("summarize"),
            grants: vec![
                ScheduleGrant::ChannelSend {
                    channel: String::from("local"),
                },
                ScheduleGrant::Vault {
                    action: String::from("fetch"),
                    name: String::from("app-token"),
                },
            ],
            channel: String::from("local"),
        });
        let json = serde_json::to_string(&create).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"schedule","op":"create","when":{"every":"cron","expr":"0 9 * * 1-5","tz":"Europe/Berlin"},"task":"summarize","grants":[{"grant":"channel-send","channel":"local"},{"grant":"vault","action":"fetch","name":"app-token"}],"channel":"local"}"#
        );
        assert_eq!(
            serde_json::from_str::<SessionRequest>(&json).unwrap(),
            create
        );

        for (grant, json) in [
            (
                ScheduleGrant::Egress {
                    destination: String::from("a:1"),
                },
                r#"{"grant":"egress","destination":"a:1"}"#,
            ),
            (
                ScheduleGrant::Memory {
                    scope: String::from("user"),
                    action: String::from("recall"),
                },
                r#"{"grant":"memory","scope":"user","action":"recall"}"#,
            ),
            (
                ScheduleGrant::Goals {
                    action: String::from("goal-create"),
                },
                r#"{"grant":"goals","action":"goal-create"}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&grant).unwrap(), json);
        }
    }

    #[test]
    fn schedule_replies_are_pinned() {
        let cancel = SessionRequest::Schedule(ScheduleOp::Cancel {
            id: String::from("sch-1"),
        });
        assert_eq!(
            serde_json::to_string(&cancel).unwrap(),
            r#"{"kind":"schedule","op":"cancel","id":"sch-1"}"#
        );
        for (result, json) in [
            (
                ScheduleResult::Created {
                    id: String::from("sch-1"),
                },
                r#"{"kind":"schedule","result":"created","id":"sch-1"}"#,
            ),
            (
                ScheduleResult::Cancelled,
                r#"{"kind":"schedule","result":"cancelled"}"#,
            ),
            (
                ScheduleResult::NotFound,
                r#"{"kind":"schedule","result":"not-found"}"#,
            ),
            (
                ScheduleResult::Refused {
                    reason: ScheduleRefusal::MissingTimezone,
                },
                r#"{"kind":"schedule","result":"refused","reason":"missing-timezone"}"#,
            ),
            (
                ScheduleResult::Denied,
                r#"{"kind":"schedule","result":"denied"}"#,
            ),
            (
                ScheduleResult::ScheduleError,
                r#"{"kind":"schedule","result":"schedule-error"}"#,
            ),
        ] {
            let response = SessionResponse::Schedule(result);
            assert_eq!(serde_json::to_string(&response).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SessionResponse>(json).unwrap(),
                response
            );
        }
    }

    #[test]
    fn followup_and_hook_wire_forms_are_pinned() {
        let followup = SessionRequest::Schedule(ScheduleOp::Followup {
            task: String::from("check again"),
            delay_secs: 600,
            grants: vec![ScheduleGrant::ChannelSend {
                channel: String::from("local"),
            }],
        });
        let json = serde_json::to_string(&followup).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"schedule","op":"followup","task":"check again","delay_secs":600,"grants":[{"grant":"channel-send","channel":"local"}]}"#
        );
        assert_eq!(
            serde_json::from_str::<SessionRequest>(&json).unwrap(),
            followup
        );
        let create = SessionRequest::Hook(HookOp::Create {
            source: HookSource::ChannelMessage {
                channel: String::from("local"),
            },
            task: String::from("triage"),
            grants: Vec::new(),
            channel: String::from("local"),
        });
        let json = serde_json::to_string(&create).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"hook","op":"create","source":{"source":"channel-message","channel":"local"},"task":"triage","grants":[],"channel":"local"}"#
        );
        assert_eq!(
            serde_json::from_str::<SessionRequest>(&json).unwrap(),
            create
        );
        let finished = HookSource::ScheduleRunFinished {
            schedule_id: String::from("sch-1"),
        };
        assert_eq!(
            serde_json::to_string(&finished).unwrap(),
            r#"{"source":"schedule-run-finished","schedule_id":"sch-1"}"#
        );
        assert_eq!(finished.to_string(), "schedule-run-finished:sch-1");
        assert_eq!(
            HookSource::ChannelMessage {
                channel: String::from("local")
            }
            .to_string(),
            "channel-message:local"
        );
        // The source set is closed: anything else does not decode.
        for bad in [
            r#"{"source":"webhook","url":"x"}"#,
            r#"{"source":"channel-message"}"#,
            r#""channel-message:local""#,
        ] {
            assert!(serde_json::from_str::<HookSource>(bad).is_err(), "{bad}");
        }
        let cancel = SessionRequest::Hook(HookOp::Cancel {
            id: String::from("hk-1"),
        });
        assert_eq!(
            serde_json::to_string(&cancel).unwrap(),
            r#"{"kind":"hook","op":"cancel","id":"hk-1"}"#
        );
    }

    #[test]
    fn hook_replies_and_new_refusals_are_pinned() {
        for (result, json) in [
            (
                HookResult::Created {
                    id: String::from("hk-1"),
                },
                r#"{"kind":"hook","result":"created","id":"hk-1"}"#,
            ),
            (
                HookResult::Cancelled,
                r#"{"kind":"hook","result":"cancelled"}"#,
            ),
            (
                HookResult::NotFound,
                r#"{"kind":"hook","result":"not-found"}"#,
            ),
            (
                HookResult::Refused {
                    reason: ScheduleRefusal::UnknownSource,
                },
                r#"{"kind":"hook","result":"refused","reason":"unknown-source"}"#,
            ),
            (HookResult::Denied, r#"{"kind":"hook","result":"denied"}"#),
            (
                HookResult::HookError,
                r#"{"kind":"hook","result":"hook-error"}"#,
            ),
        ] {
            let response = SessionResponse::Hook(result);
            assert_eq!(serde_json::to_string(&response).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SessionResponse>(json).unwrap(),
                response
            );
        }
        for (reason, json) in [
            (ScheduleRefusal::MaxFollowupDepth, r#""max-followup-depth""#),
            (ScheduleRefusal::FollowupTaken, r#""followup-taken""#),
            (ScheduleRefusal::BadDelay, r#""bad-delay""#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), json);
        }
    }

    #[test]
    fn artifact_wire_forms_are_pinned() {
        let build = SessionRequest::Artifact(ArtifactOp::Build {
            build_kind: String::from("document"),
            builder: String::from("doc@1.0.0"),
            inputs: vec![ArtifactInput {
                name: String::from("body"),
                content: String::from("hello"),
            }],
        });
        let json = serde_json::to_string(&build).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"artifact","op":"build","build_kind":"document","builder":"doc@1.0.0","inputs":[{"name":"body","content":"hello"}]}"#
        );
        assert_eq!(
            serde_json::from_str::<SessionRequest>(&json).unwrap(),
            build
        );
        let deliver = SessionRequest::Artifact(ArtifactOp::Deliver {
            build: String::from("art-1"),
            to: DeliveryTarget::Workspace {
                name: String::from("out.md"),
            },
        });
        let json = serde_json::to_string(&deliver).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"artifact","op":"deliver","build":"art-1","to":{"target":"workspace","name":"out.md"}}"#
        );
        assert_eq!(
            serde_json::from_str::<SessionRequest>(&json).unwrap(),
            deliver
        );
        assert_eq!(
            serde_json::to_string(&DeliveryTarget::Channel {
                channel: String::from("local")
            })
            .unwrap(),
            r#"{"target":"channel","channel":"local"}"#
        );
        for (op, json) in [
            (
                ArtifactOp::Describe {
                    build: String::from("art-1"),
                },
                r#"{"kind":"artifact","op":"describe","build":"art-1"}"#,
            ),
            (
                ArtifactOp::CaptureIdea {
                    text: String::from("a zine"),
                    from: None,
                },
                r#"{"kind":"artifact","op":"capture-idea","text":"a zine"}"#,
            ),
            (
                ArtifactOp::RefineIdea {
                    idea: String::from("idea-1"),
                    text: String::from("a zine, 8 pages"),
                },
                r#"{"kind":"artifact","op":"refine-idea","idea":"idea-1","text":"a zine, 8 pages"}"#,
            ),
            (
                ArtifactOp::BuildIdea {
                    idea: String::from("idea-1"),
                    build_kind: String::from("page"),
                    builder: String::from("page@1.0.0"),
                },
                r#"{"kind":"artifact","op":"build-idea","idea":"idea-1","build_kind":"page","builder":"page@1.0.0"}"#,
            ),
        ] {
            let request = SessionRequest::Artifact(op);
            assert_eq!(serde_json::to_string(&request).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SessionRequest>(json).unwrap(),
                request
            );
        }
    }

    #[test]
    fn artifact_result_wire_forms_are_pinned() {
        let inspection = ArtifactInspection {
            verdict: InspectionVerdict::Fail,
            content_type: String::from("text/html"),
            findings: vec![InspectionFinding {
                check: String::from("script"),
                detail: String::from("the page runs a script"),
            }],
        };
        let inspected = r#"{"verdict":"fail","content_type":"text/html","findings":[{"check":"script","detail":"the page runs a script"}]}"#;
        for (result, json) in [
            (
                ArtifactResult::Built {
                    build: String::from("art-1"),
                    output_hash: String::from("ab"),
                    builder: String::from("doc@1.0.0"),
                    signer: String::from("cd"),
                    attribution: String::from("built by verified builder doc@1.0.0"),
                    inspection: inspection.clone(),
                },
                format!(
                    r#"{{"kind":"artifact","result":"built","build":"art-1","output_hash":"ab","builder":"doc@1.0.0","signer":"cd","attribution":"built by verified builder doc@1.0.0","inspection":{inspected}}}"#
                ),
            ),
            (
                ArtifactResult::BuildFailed {
                    reason: BuildFailure::CapabilityDenied,
                },
                String::from(
                    r#"{"kind":"artifact","result":"build-failed","reason":"capability-denied"}"#,
                ),
            ),
            (
                ArtifactResult::Delivered { inspection },
                format!(r#"{{"kind":"artifact","result":"delivered","inspection":{inspected}}}"#),
            ),
            (
                ArtifactResult::Described {
                    view: ArtifactView {
                        build: String::from("art-1"),
                        kind: String::from("page"),
                        state: BuildState::Completed,
                        attribution: Some(String::from("a")),
                        inspection: None,
                        output_hash: Some(String::from("ab")),
                        idea: Some(IdeaRef {
                            idea: String::from("idea-1"),
                            revision: 2,
                            sha256: String::from("ef"),
                        }),
                        delivered: 1,
                    },
                },
                String::from(
                    r#"{"kind":"artifact","result":"described","view":{"build":"art-1","kind":"page","state":"completed","attribution":"a","inspection":null,"output_hash":"ab","idea":{"idea":"idea-1","revision":2,"sha256":"ef"},"delivered":1}}"#,
                ),
            ),
            (
                ArtifactResult::UnknownBuild,
                String::from(r#"{"kind":"artifact","result":"unknown-build"}"#),
            ),
            (
                ArtifactResult::IdeaSaved {
                    idea: String::from("idea-1"),
                    revision: 1,
                },
                String::from(
                    r#"{"kind":"artifact","result":"idea-saved","idea":"idea-1","revision":1}"#,
                ),
            ),
            (
                ArtifactResult::IdeaRefused {
                    reason: IdeaRefusal::BadText,
                },
                String::from(r#"{"kind":"artifact","result":"idea-refused","reason":"bad-text"}"#),
            ),
            (
                ArtifactResult::Refused {
                    reason: DeliveryRefusal::GateError,
                },
                String::from(r#"{"kind":"artifact","result":"refused","reason":"gate-error"}"#),
            ),
            (
                ArtifactResult::Denied,
                String::from(r#"{"kind":"artifact","result":"denied"}"#),
            ),
            (
                ArtifactResult::ArtifactError,
                String::from(r#"{"kind":"artifact","result":"artifact-error"}"#),
            ),
        ] {
            let response = SessionResponse::Artifact(result);
            assert_eq!(serde_json::to_string(&response).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SessionResponse>(&json).unwrap(),
                response
            );
        }
    }

    #[test]
    fn space_op_wire_forms_are_pinned_and_no_op_enumerates() {
        let send = ScheduleGrant::ChannelSend {
            channel: String::from("local"),
        };
        for (op, json) in [
            (
                SpaceOp::Create {
                    name: String::from("Garden"),
                    grants: vec![send.clone()],
                },
                r#"{"kind":"space","op":"create","name":"Garden","grants":[{"grant":"channel-send","channel":"local"}]}"#,
            ),
            (
                SpaceOp::Join {
                    space: String::from("spc-1"),
                },
                r#"{"kind":"space","op":"join","space":"spc-1"}"#,
            ),
            (
                SpaceOp::Put {
                    space: String::from("spc-1"),
                    ticket: String::from("adm-1"),
                    key: String::from("plan"),
                    value: String::from("v"),
                },
                r#"{"kind":"space","op":"put","space":"spc-1","ticket":"adm-1","key":"plan","value":"v"}"#,
            ),
            (
                SpaceOp::Get {
                    space: String::from("spc-1"),
                    ticket: String::from("adm-1"),
                    key: String::from("plan"),
                },
                r#"{"kind":"space","op":"get","space":"spc-1","ticket":"adm-1","key":"plan"}"#,
            ),
            (
                SpaceOp::Remove {
                    space: String::from("spc-1"),
                    ticket: String::from("adm-1"),
                    key: String::from("plan"),
                },
                r#"{"kind":"space","op":"remove","space":"spc-1","ticket":"adm-1","key":"plan"}"#,
            ),
            (
                SpaceOp::AddTask {
                    space: String::from("spc-1"),
                    ticket: String::from("adm-1"),
                    task: String::from("water"),
                    grants: vec![send],
                    channel: String::from("local"),
                },
                r#"{"kind":"space","op":"add-task","space":"spc-1","ticket":"adm-1","task":"water","grants":[{"grant":"channel-send","channel":"local"}],"channel":"local"}"#,
            ),
            (
                SpaceOp::RunTask {
                    space: String::from("spc-1"),
                    ticket: String::from("adm-1"),
                    task: String::from("tsk-1"),
                },
                r#"{"kind":"space","op":"run-task","space":"spc-1","ticket":"adm-1","task":"tsk-1"}"#,
            ),
        ] {
            let request = SessionRequest::Space(op);
            assert_eq!(serde_json::to_string(&request).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SessionRequest>(json).unwrap(),
                request
            );
        }
        // No op enumerates: these are not space ops at all.
        for op in ["list", "dump", "export", "sync", "list-items", "members"] {
            let json = format!(r#"{{"kind":"space","op":"{op}","space":"spc-1"}}"#);
            assert!(
                serde_json::from_str::<SessionRequest>(&json).is_err(),
                "{op}"
            );
        }
    }

    #[test]
    fn space_result_wire_forms_are_pinned() {
        for (result, json) in [
            (
                SpaceResult::Created {
                    space: String::from("spc-1"),
                },
                r#"{"kind":"space","result":"created","space":"spc-1"}"#,
            ),
            (
                SpaceResult::Admitted {
                    space: String::from("spc-1"),
                    ticket: String::from("adm-1"),
                    expires_at: 1_790_000_900,
                },
                r#"{"kind":"space","result":"admitted","space":"spc-1","ticket":"adm-1","expires_at":1790000900}"#,
            ),
            (SpaceResult::Stored, r#"{"kind":"space","result":"stored"}"#),
            (
                SpaceResult::Value {
                    value: String::from("v"),
                },
                r#"{"kind":"space","result":"value","value":"v"}"#,
            ),
            (
                SpaceResult::Removed,
                r#"{"kind":"space","result":"removed"}"#,
            ),
            (
                SpaceResult::TaskAdded {
                    task: String::from("tsk-1"),
                },
                r#"{"kind":"space","result":"task-added","task":"tsk-1"}"#,
            ),
            (
                SpaceResult::TaskQueued {
                    schedule: String::from("sch-1"),
                },
                r#"{"kind":"space","result":"task-queued","schedule":"sch-1"}"#,
            ),
            (
                SpaceResult::Refused {
                    reason: SpaceRefusal::StaleAdmission,
                },
                r#"{"kind":"space","result":"refused","reason":"stale-admission"}"#,
            ),
            (SpaceResult::Denied, r#"{"kind":"space","result":"denied"}"#),
            (
                SpaceResult::SpaceError,
                r#"{"kind":"space","result":"space-error"}"#,
            ),
        ] {
            let response = SessionResponse::Space(result);
            assert_eq!(serde_json::to_string(&response).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SessionResponse>(json).unwrap(),
                response
            );
        }
        for (reason, json) in [
            (SpaceRefusal::NoSuchSpace, r#""no-such-space""#),
            (SpaceRefusal::NotAdmitted, r#""not-admitted""#),
            (SpaceRefusal::StaleAdmission, r#""stale-admission""#),
            (SpaceRefusal::PeerChanged, r#""peer-changed""#),
            (SpaceRefusal::Expired, r#""expired""#),
            (SpaceRefusal::BadName, r#""bad-name""#),
            (SpaceRefusal::BadKey, r#""bad-key""#),
            (SpaceRefusal::BadValue, r#""bad-value""#),
            (SpaceRefusal::NoSuchItem, r#""no-such-item""#),
            (SpaceRefusal::ExceedsGrants, r#""exceeds-grants""#),
            (SpaceRefusal::BadTask, r#""bad-task""#),
            (SpaceRefusal::NoSuchTask, r#""no-such-task""#),
            (SpaceRefusal::Full, r#""full""#),
            (SpaceRefusal::NotAllowed, r#""not-allowed""#),
            (SpaceRefusal::SchedulerOff, r#""scheduler-off""#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), json);
        }
    }

    #[test]
    fn sync_and_media_op_wire_forms_are_pinned() {
        let base = || (String::from("spc-1"), String::from("adm-1"));
        let (space, ticket) = base();
        for (op, json) in [
            (
                SpaceOp::Pair {
                    space: space.clone(),
                    ticket: ticket.clone(),
                },
                r#"{"kind":"space","op":"pair","space":"spc-1","ticket":"adm-1"}"#,
            ),
            (
                SpaceOp::SyncPull {
                    space: space.clone(),
                    ticket: ticket.clone(),
                    since: 7,
                },
                r#"{"kind":"space","op":"sync-pull","space":"spc-1","ticket":"adm-1","since":7}"#,
            ),
            (
                SpaceOp::SyncPush {
                    space: space.clone(),
                    ticket: ticket.clone(),
                    changes: vec![
                        SyncChange {
                            key: String::from("a"),
                            value: Some(String::from("v")),
                            base: 3,
                        },
                        SyncChange {
                            key: String::from("b"),
                            value: None,
                            base: 0,
                        },
                    ],
                },
                r#"{"kind":"space","op":"sync-push","space":"spc-1","ticket":"adm-1","changes":[{"key":"a","value":"v","base":3},{"key":"b","value":null,"base":0}]}"#,
            ),
            (
                SpaceOp::Conflicts {
                    space: space.clone(),
                    ticket: ticket.clone(),
                    after: 2,
                },
                r#"{"kind":"space","op":"conflicts","space":"spc-1","ticket":"adm-1","after":2}"#,
            ),
            (
                SpaceOp::PutMedia {
                    media_kind: String::from("image"),
                    space: space.clone(),
                    ticket: ticket.clone(),
                    data: String::from("iVBO"),
                },
                r#"{"kind":"space","op":"put-media","media_kind":"image","space":"spc-1","ticket":"adm-1","data":"iVBO"}"#,
            ),
            (
                SpaceOp::GetMedia {
                    space,
                    ticket,
                    media: String::from("med-1"),
                },
                r#"{"kind":"space","op":"get-media","space":"spc-1","ticket":"adm-1","media":"med-1"}"#,
            ),
        ] {
            let request = SessionRequest::Space(op);
            assert_eq!(serde_json::to_string(&request).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SessionRequest>(json).unwrap(),
                request
            );
        }
    }

    #[test]
    fn sync_and_media_result_wire_forms_are_pinned() {
        for (result, json) in [
            (
                SpaceResult::PairingCode {
                    code: String::from("pair-1"),
                    expires_at: 9,
                },
                r#"{"result":"pairing-code","code":"pair-1","expires_at":9}"#,
            ),
            (
                SpaceResult::Changes {
                    changes: vec![SyncItem {
                        key: String::from("a"),
                        value: None,
                        version: 4,
                        writer: String::from("device:dev-1"),
                    }],
                    head: 4,
                    more: false,
                },
                r#"{"result":"changes","changes":[{"key":"a","value":null,"version":4,"writer":"device:dev-1"}],"head":4,"more":false}"#,
            ),
            (
                SpaceResult::Pushed {
                    head: 5,
                    conflicts: 1,
                },
                r#"{"result":"pushed","head":5,"conflicts":1}"#,
            ),
            (
                SpaceResult::ConflictLog {
                    entries: vec![ConflictEntry {
                        seq: 1,
                        key: String::from("a"),
                        lost: Some(String::from("old")),
                        lost_version: 2,
                        lost_writer: String::from("uid:1000"),
                        winner: String::from("device:dev-1"),
                        won_version: 3,
                        at: 8,
                    }],
                    dropped: 0,
                    more: false,
                },
                r#"{"result":"conflict-log","entries":[{"seq":1,"key":"a","lost":"old","lost_version":2,"lost_writer":"uid:1000","winner":"device:dev-1","won_version":3,"at":8}],"dropped":0,"more":false}"#,
            ),
            (
                SpaceResult::MediaStored {
                    media: String::from("med-1"),
                    content_type: String::from("image/png"),
                    bytes: 12,
                },
                r#"{"result":"media-stored","media":"med-1","content_type":"image/png","bytes":12}"#,
            ),
            (
                SpaceResult::Media {
                    media: String::from("med-1"),
                    content_type: String::from("image/png"),
                    data: String::from("iVBO"),
                    headers: vec![MediaHeader {
                        name: String::from("X-Content-Type-Options"),
                        value: String::from("nosniff"),
                    }],
                },
                r#"{"result":"media","media":"med-1","content_type":"image/png","data":"iVBO","headers":[{"name":"X-Content-Type-Options","value":"nosniff"}]}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&result).unwrap(), json);
            assert_eq!(serde_json::from_str::<SpaceResult>(json).unwrap(), result);
        }
        for (reason, json) in [
            (SpaceRefusal::TooManyChanges, r#""too-many-changes""#),
            (SpaceRefusal::BadMedia, r#""bad-media""#),
            (SpaceRefusal::MediaTooLarge, r#""media-too-large""#),
            (SpaceRefusal::WrongMediaType, r#""wrong-media-type""#),
            (SpaceRefusal::NoSuchMedia, r#""no-such-media""#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), json);
        }
    }

    #[test]
    fn device_handshake_wire_forms_are_pinned() {
        let challenge = DeviceChallenge {
            v: 10,
            nonce: String::from("ab"),
            broker_key: String::from("Kk"),
        };
        assert_eq!(
            serde_json::to_string(&challenge).unwrap(),
            r#"{"v":10,"nonce":"ab","broker_key":"Kk"}"#
        );
        for (hello, json) in [
            (
                DeviceHello::Pair {
                    code: String::from("pair-1"),
                    device_key: String::from("Kd"),
                    nonce: String::from("cd"),
                    signature: String::from("Sg"),
                },
                r#"{"hello":"pair","code":"pair-1","device_key":"Kd","nonce":"cd","signature":"Sg"}"#,
            ),
            (
                DeviceHello::Auth {
                    device: String::from("dev-1"),
                    nonce: String::from("cd"),
                    signature: String::from("Sg"),
                },
                r#"{"hello":"auth","device":"dev-1","nonce":"cd","signature":"Sg"}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&hello).unwrap(), json);
            assert_eq!(serde_json::from_str::<DeviceHello>(json).unwrap(), hello);
        }
        for (welcome, json) in [
            (
                DeviceWelcome::Paired {
                    device: String::from("dev-1"),
                    space: String::from("spc-1"),
                    signature: String::from("Sb"),
                },
                r#"{"welcome":"paired","device":"dev-1","space":"spc-1","signature":"Sb"}"#,
            ),
            (
                DeviceWelcome::Authenticated {
                    device: String::from("dev-1"),
                    signature: String::from("Sb"),
                },
                r#"{"welcome":"authenticated","device":"dev-1","signature":"Sb"}"#,
            ),
            (
                DeviceWelcome::Refused {
                    reason: DeviceRefusal::BadCode,
                },
                r#"{"welcome":"refused","reason":"bad-code"}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&welcome).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<DeviceWelcome>(json).unwrap(),
                welcome
            );
        }
        for (reason, json) in [
            (DeviceRefusal::BadCode, r#""bad-code""#),
            (DeviceRefusal::Unpaired, r#""unpaired""#),
            (DeviceRefusal::BadSignature, r#""bad-signature""#),
            (DeviceRefusal::Full, r#""full""#),
            (DeviceRefusal::DeviceError, r#""device-error""#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), json);
        }
    }

    #[test]
    fn idea_lifecycle_op_wire_forms_are_pinned() {
        for (op, json) in [
            (
                ArtifactOp::CaptureIdea {
                    text: String::from("a zine"),
                    from: Some(IdeaSource::Feed {
                        store: String::from("memory"),
                        id: String::from("m-1"),
                        tag: Some(String::from("travel")),
                    }),
                },
                r#"{"kind":"artifact","op":"capture-idea","text":"a zine","from":{"source":"feed","store":"memory","id":"m-1","tag":"travel"}}"#,
            ),
            (
                ArtifactOp::CaptureIdea {
                    text: String::from("t"),
                    from: Some(IdeaSource::Chat),
                },
                r#"{"kind":"artifact","op":"capture-idea","text":"t","from":{"source":"chat"}}"#,
            ),
            (
                ArtifactOp::CaptureIdea {
                    text: String::from("t"),
                    from: Some(IdeaSource::Command),
                },
                r#"{"kind":"artifact","op":"capture-idea","text":"t","from":{"source":"command"}}"#,
            ),
            (
                ArtifactOp::ListIdeas { after: None },
                r#"{"kind":"artifact","op":"list-ideas"}"#,
            ),
            (
                ArtifactOp::ListIdeas {
                    after: Some(String::from("idea-1")),
                },
                r#"{"kind":"artifact","op":"list-ideas","after":"idea-1"}"#,
            ),
            (
                ArtifactOp::NoteIdea {
                    idea: String::from("idea-1"),
                    note: String::from("A5"),
                },
                r#"{"kind":"artifact","op":"note-idea","idea":"idea-1","note":"A5"}"#,
            ),
            (
                ArtifactOp::ProposeActivation {
                    idea: String::from("idea-1"),
                    build_kind: String::from("document"),
                    builder: String::from("doc@1.0.0"),
                    description: String::from("a printable zine"),
                },
                r#"{"kind":"artifact","op":"propose-activation","idea":"idea-1","build_kind":"document","builder":"doc@1.0.0","description":"a printable zine"}"#,
            ),
            (
                ArtifactOp::Activate {
                    activation: String::from("act-1"),
                    confirmation: String::from("confirm-1"),
                },
                r#"{"kind":"artifact","op":"activate","activation":"act-1","confirmation":"confirm-1"}"#,
            ),
        ] {
            let request = SessionRequest::Artifact(op);
            assert_eq!(serde_json::to_string(&request).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SessionRequest>(json).unwrap(),
                request
            );
        }
    }

    #[test]
    fn idea_lifecycle_result_wire_forms_are_pinned() {
        let ideas = SessionResponse::Artifact(ArtifactResult::Ideas {
            ideas: vec![IdeaSummary {
                idea: String::from("idea-1"),
                title: String::from("a zine"),
                revision: 2,
                notes: 1,
                state: IdeaState::Developing,
                origin: String::from("captured from chat"),
            }],
            next: None,
        });
        let json = serde_json::to_string(&ideas).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"artifact","result":"ideas","ideas":[{"idea":"idea-1","title":"a zine","revision":2,"notes":1,"state":"developing","origin":"captured from chat"}],"next":null}"#
        );
        assert_eq!(
            serde_json::from_str::<SessionResponse>(&json).unwrap(),
            ideas
        );
        let proposed = SessionResponse::Artifact(ArtifactResult::ActivationProposed {
            proposal: ActivationProposal {
                activation: String::from("act-1"),
                idea: IdeaRef {
                    idea: String::from("idea-1"),
                    revision: 2,
                    sha256: String::from("ab"),
                },
                build_kind: String::from("document"),
                builder: String::from("doc@1.0.0"),
                description: String::from("a zine"),
                inputs: vec![ActivationInput {
                    name: String::from("idea"),
                    sha256: String::from("ab"),
                }],
                request_hash: String::from("cd"),
            },
        });
        let json = serde_json::to_string(&proposed).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"artifact","result":"activation-proposed","proposal":{"activation":"act-1","idea":{"idea":"idea-1","revision":2,"sha256":"ab"},"build_kind":"document","builder":"doc@1.0.0","description":"a zine","inputs":[{"name":"idea","sha256":"ab"}],"request_hash":"cd"}}"#
        );
        assert_eq!(
            serde_json::from_str::<SessionResponse>(&json).unwrap(),
            proposed
        );
    }

    #[test]
    fn a_feed_schedule_wire_form_is_pinned() {
        let create = SessionRequest::Schedule(ScheduleOp::CreateFeed {
            when: ScheduleSpec::Cron {
                expr: String::from("0 8 * * *"),
                tz: String::from("Europe/Berlin"),
            },
            channel: String::from("local"),
            feed: FeedSpec {
                goals: Some(String::from("open")),
                tracking: true,
                tracking_source: Some(String::from("whatsapp")),
                memory_tags: vec![String::from("travel")],
                lookback_hours: 24,
                max_items: 20,
            },
        });
        let json = serde_json::to_string(&create).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"schedule","op":"create-feed","when":{"every":"cron","expr":"0 8 * * *","tz":"Europe/Berlin"},"channel":"local","feed":{"goals":"open","tracking":true,"tracking_source":"whatsapp","memory_tags":["travel"],"lookback_hours":24,"max_items":20}}"#
        );
        assert_eq!(
            serde_json::from_str::<SessionRequest>(&json).unwrap(),
            create
        );
        // The smallest spec, and one with a field the spec does not have.
        let minimal: FeedSpec =
            serde_json::from_str(r#"{"tracking":true,"lookback_hours":1,"max_items":1}"#).unwrap();
        assert_eq!(
            (
                minimal.goals,
                minimal.tracking_source,
                minimal.memory_tags.len()
            ),
            (None, None, 0)
        );
        assert!(
            serde_json::from_str::<FeedSpec>(
                r#"{"tracking":true,"lookback_hours":1,"max_items":1,"act":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn artifact_reason_wire_forms_are_pinned() {
        for (reason, json) in [
            (BuildFailure::BadRequest, r#""bad-request""#),
            (BuildFailure::UnknownBuilder, r#""unknown-builder""#),
            (BuildFailure::BuilderFailed, r#""builder-failed""#),
            (BuildFailure::BadOutput, r#""bad-output""#),
            (BuildFailure::Full, r#""full""#),
            (BuildFailure::UnknownKind, r#""unknown-kind""#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), json);
        }
        for (reason, json) in [
            (IdeaRefusal::UnknownIdea, r#""unknown-idea""#),
            (IdeaRefusal::BadText, r#""bad-text""#),
            (IdeaRefusal::Full, r#""full""#),
            (IdeaRefusal::BadNote, r#""bad-note""#),
            (IdeaRefusal::TooManyNotes, r#""too-many-notes""#),
            (IdeaRefusal::BadDescription, r#""bad-description""#),
            (
                IdeaRefusal::ConfirmationRequired,
                r#""confirmation-required""#,
            ),
            (IdeaRefusal::NotConfirmed, r#""not-confirmed""#),
            (IdeaRefusal::NoProposal, r#""no-proposal""#),
            (IdeaRefusal::UnreadableSource, r#""unreadable-source""#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), json);
        }
        for (state, json) in [
            (IdeaState::Captured, r#""captured""#),
            (IdeaState::Developing, r#""developing""#),
            (
                IdeaState::AwaitingConfirmation,
                r#""awaiting-confirmation""#,
            ),
            (IdeaState::Activated, r#""activated""#),
        ] {
            assert_eq!(serde_json::to_string(&state).unwrap(), json);
        }
        for (state, json) in [
            (BuildState::Pending, r#""pending""#),
            (BuildState::Failed, r#""failed""#),
        ] {
            assert_eq!(serde_json::to_string(&state).unwrap(), json);
        }
        assert_eq!(
            serde_json::to_string(&InspectionVerdict::Pass).unwrap(),
            r#""pass""#
        );
        for (reason, json) in [
            (DeliveryRefusal::UnknownBuild, r#""unknown-build""#),
            (DeliveryRefusal::Incomplete, r#""incomplete""#),
            (DeliveryRefusal::HashMismatch, r#""hash-mismatch""#),
            (DeliveryRefusal::Blocked, r#""blocked""#),
            (DeliveryRefusal::BadTarget, r#""bad-target""#),
            (DeliveryRefusal::DeliveryFailed, r#""delivery-failed""#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), json);
        }
    }

    #[test]
    fn skill_op_serde_round_trip() {
        // Pins the wire tags: the JSON shape is the cross-crate contract.
        let req = SessionRequest::Skill(SkillOp::Invoke {
            skill: String::from("web@1.0.0"),
            capability: "net.egress".parse().unwrap(),
            input: String::from("q"),
        });
        let json = serde_json::to_vec(&req).unwrap();
        assert_eq!(
            json,
            br#"{"kind":"skill","op":"invoke","skill":"web@1.0.0","capability":"net.egress","input":"q"}"#
                .as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<SessionRequest>(&json).unwrap(),
            req
        );

        for (result, wire) in [
            (
                SkillResult::Output {
                    output: String::from("hi"),
                },
                br#"{"kind":"skill","r":"output","output":"hi"}"#.as_slice(),
            ),
            (
                SkillResult::UnknownSkill,
                br#"{"kind":"skill","r":"unknown-skill"}"#.as_slice(),
            ),
            (
                SkillResult::CapabilityDenied,
                br#"{"kind":"skill","r":"capability-denied"}"#.as_slice(),
            ),
            (
                SkillResult::SkillError,
                br#"{"kind":"skill","r":"skill-error"}"#.as_slice(),
            ),
        ] {
            let resp = SessionResponse::Skill(result);
            let json = serde_json::to_vec(&resp).unwrap();
            assert_eq!(json, wire);
            assert_eq!(
                serde_json::from_slice::<SessionResponse>(&json).unwrap(),
                resp
            );
        }
    }

    #[test]
    fn the_skill_caps_are_pinned() {
        // Part of the wire contract: both ends size buffers and refuse
        // by these. A change is a deliberate protocol change.
        assert_eq!(MAX_SKILL_INPUT_BYTES, 8192);
        assert_eq!(MAX_SKILL_OUTPUT_BYTES, 8192);
    }

    #[test]
    fn worst_case_skill_frames_fit_one_frame() {
        // Every byte a control character: the most expensive escaping
        // JSON does. With a maximal capability and a long id, the capped
        // request and reply still fit a frame.
        let request = SessionRequest::Skill(SkillOp::Invoke {
            skill: format!("{}@{}", "n".repeat(256), "1".repeat(64)),
            capability: "c".repeat(128).parse().unwrap(),
            input: "\u{1}".repeat(MAX_SKILL_INPUT_BYTES),
        });
        let len = serde_json::to_vec(&request).unwrap().len();
        assert!(len <= crate::MAX_FRAME_BYTES, "request frame {len}");
        let reply = SessionResponse::Skill(SkillResult::Output {
            output: "\u{1}".repeat(MAX_SKILL_OUTPUT_BYTES),
        });
        let len = serde_json::to_vec(&reply).unwrap().len();
        assert!(len <= crate::MAX_FRAME_BYTES, "reply frame {len}");
    }

    #[test]
    fn skill_op_refuses_a_malformed_capability_at_decode() {
        // The capability is typed on the wire: a name the parser refuses
        // never becomes a request, so it cannot reach the broker's
        // enforcement path as a string.
        let long = "a".repeat(129);
        for capability in ["", "has space", "semi;colon", long.as_str()] {
            let json = format!(
                r#"{{"kind":"skill","op":"invoke","skill":"a@1","capability":"{capability}","input":""}}"#
            );
            assert!(
                serde_json::from_str::<SessionRequest>(&json).is_err(),
                "capability {capability:?} must not decode"
            );
        }
    }

    #[test]
    fn skill_op_rejects_unknown_tag() {
        let json = br#"{"op":"list"}"#;
        assert!(serde_json::from_slice::<SkillOp>(json).is_err());
        let json = br#"{"r":"granted"}"#;
        assert!(serde_json::from_slice::<SkillResult>(json).is_err());
    }

    #[test]
    fn channel_op_rejects_unknown_tag() {
        let json = br#"{"op":"broadcast","channel":"x"}"#;
        assert!(serde_json::from_slice::<ChannelOp>(json).is_err());
    }

    #[test]
    fn session_request_channel_round_trip() {
        // The v4 session envelope: kind=channel carries a ChannelOp.
        let req = SessionRequest::Channel(ChannelOp::Receive {
            channel: String::from("local"),
            account: None,
        });
        let json = serde_json::to_vec(&req).unwrap();
        assert_eq!(
            json,
            br#"{"kind":"channel","op":"receive","channel":"local"}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<SessionRequest>(&json).unwrap(),
            req
        );

        let resp = SessionResponse::Channel(ChannelResult::Denied);
        let json = serde_json::to_vec(&resp).unwrap();
        assert_eq!(json, br#"{"kind":"channel","r":"denied"}"#.as_slice());
        assert_eq!(
            serde_json::from_slice::<SessionResponse>(&json).unwrap(),
            resp
        );
    }

    #[test]
    fn goals_op_wire_form_is_pinned() {
        let create = SessionRequest::Goals(GoalsOp::GoalCreate {
            title: String::from("run a 5k"),
            detail: String::new(),
        });
        let json = serde_json::to_vec(&create).unwrap();
        assert_eq!(
            json,
            br#"{"kind":"goals","op":"goal-create","title":"run a 5k","detail":""}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<SessionRequest>(&json).unwrap(),
            create
        );
        for op in [
            GoalsOp::GoalUpdate {
                id: String::from("g1"),
                title: None,
                detail: Some(String::from("by spring")),
            },
            GoalsOp::GoalClose {
                id: String::from("g1"),
                outcome: String::from("achieved"),
            },
            GoalsOp::TrackingAppend {
                source: String::from("whatsapp"),
                text: String::from("see you at the race"),
            },
        ] {
            let json = serde_json::to_vec(&SessionRequest::Goals(op.clone())).unwrap();
            assert_eq!(
                serde_json::from_slice::<SessionRequest>(&json).unwrap(),
                SessionRequest::Goals(op)
            );
        }
        let reply = SessionResponse::Goals(GoalsResult::TrackingAppended { seq: 3 });
        let json = serde_json::to_vec(&reply).unwrap();
        assert_eq!(
            json,
            br#"{"kind":"goals","result":"tracking-appended","seq":3}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<SessionResponse>(&json).unwrap(),
            reply
        );
    }

    #[test]
    fn query_op_wire_form_is_pinned_and_optional_fields_default() {
        let query = SessionRequest::Query(QueryOp::Tracking {
            since_secs: Some(10),
            until_secs: None,
            source: Some(String::from("user")),
            after: None,
        });
        let json = serde_json::to_vec(&query).unwrap();
        assert_eq!(
            json,
            br#"{"kind":"query","op":"tracking","since_secs":10,"until_secs":null,"source":"user","after":null}"#
                .as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<SessionRequest>(&json).unwrap(),
            query
        );
        // Absent optional fields decode as `None`.
        let bare = br#"{"kind":"query","op":"goals","status":"open"}"#;
        assert_eq!(
            serde_json::from_slice::<SessionRequest>(bare).unwrap(),
            SessionRequest::Query(QueryOp::Goals {
                status: String::from("open"),
                updated_since_secs: None,
                after: None,
            })
        );
        let memory = br#"{"kind":"query","op":"memory","tag":"feed"}"#;
        assert!(serde_json::from_slice::<SessionRequest>(memory).is_ok());
        let reply = SessionResponse::Query(QueryResult::Items {
            items_json: vec![String::from("{}")],
            next: Some(String::from("4")),
        });
        let json = serde_json::to_vec(&reply).unwrap();
        assert_eq!(
            json,
            br#"{"kind":"query","result":"items","items_json":["{}"],"next":"4"}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<SessionResponse>(&json).unwrap(),
            reply
        );
    }

    #[test]
    fn no_goals_op_can_be_spelled_as_a_query() {
        // The feed's vocabulary has no write in it: every goals op,
        // re-sent under the query kind, fails to decode — the broker
        // ends the session instead of acting on it.
        for op in [
            GoalsOp::GoalCreate {
                title: String::from("t"),
                detail: String::new(),
            },
            GoalsOp::GoalUpdate {
                id: String::from("g1"),
                title: Some(String::from("t")),
                detail: None,
            },
            GoalsOp::GoalClose {
                id: String::from("g1"),
                outcome: String::from("achieved"),
            },
            GoalsOp::TrackingAppend {
                source: String::from("user"),
                text: String::from("t"),
            },
        ] {
            let json = String::from_utf8(serde_json::to_vec(&SessionRequest::Goals(op)).unwrap())
                .unwrap()
                .replace(r#""kind":"goals""#, r#""kind":"query""#);
            assert!(json.contains(r#""kind":"query""#));
            assert!(
                serde_json::from_str::<SessionRequest>(&json).is_err(),
                "a write decoded as a query: {json}"
            );
        }
    }

    #[test]
    fn goals_and_query_reject_unknown_tags() {
        assert!(serde_json::from_slice::<GoalsOp>(br#"{"op":"goal-delete","id":"g1"}"#).is_err());
        assert!(serde_json::from_slice::<QueryOp>(br#"{"op":"dump"}"#).is_err());
        assert!(serde_json::from_slice::<GoalsResult>(br#"{"result":"deleted"}"#).is_err());
    }
}

#[cfg(test)]
mod memory_paging_wire_tests {
    use super::*;

    #[test]
    fn paged_memory_ops_decode_without_a_cursor() {
        // A first page names no cursor: the absent field is `None`, so a
        // request that predates paging still decodes.
        let bare = br#"{"kind":"memory","op":"context-read"}"#;
        assert_eq!(
            serde_json::from_slice::<SessionRequest>(bare).unwrap(),
            SessionRequest::Memory(MemoryOp::ContextRead { after: None })
        );
        let tag = br#"{"kind":"memory","op":"recall-by-tag","scope":"user","tag":"t"}"#;
        assert_eq!(
            serde_json::from_slice::<SessionRequest>(tag).unwrap(),
            SessionRequest::Memory(MemoryOp::RecallByTag {
                scope: String::from("user"),
                tag: String::from("t"),
                after: None,
            })
        );
    }

    #[test]
    fn paged_memory_replies_carry_their_cursor() {
        let reply = SessionResponse::Memory(MemoryResult::Items {
            items_json: vec![String::from("{}")],
            next: Some(String::from("note-2")),
        });
        let json = serde_json::to_vec(&reply).unwrap();
        assert_eq!(
            json,
            br#"{"kind":"memory","result":"items","items_json":["{}"],"next":"note-2"}"#.as_slice()
        );
        assert_eq!(
            serde_json::from_slice::<SessionResponse>(&json).unwrap(),
            reply
        );
        let entries = SessionResponse::Memory(MemoryResult::ContextEntries {
            entries_json: vec![],
            next: None,
        });
        let json = serde_json::to_vec(&entries).unwrap();
        assert_eq!(
            json,
            br#"{"kind":"memory","result":"context-entries","entries_json":[],"next":null}"#
                .as_slice()
        );
    }
}
