//! `hatchling-core`: the shared foundation every hatchling binary builds on.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! This crate owns the cross-binary contract: identity newtypes, the
//! protocol framing and message types, kernel peer credentials, and the
//! small error type the contract can produce. It has no binaries, no
//! configuration, and no opinions about what runs on either end of the
//! socket — the agent and the broker each add their own behavior.

pub mod error;
pub mod framing;
pub mod log_level;
pub mod messages;
pub mod peer;
pub mod secret;
pub mod subagent;
#[cfg(test)]
mod test_socket;
pub mod types;
pub mod ui;

pub use error::Error;
pub use framing::{MAX_FRAME_BYTES, PROTOCOL_VERSION};
pub use log_level::LogLevel;
pub use messages::{
    ActivationInput, ActivationProposal, AgentMessage, ArtifactInput, ArtifactInspection,
    ArtifactOp, ArtifactResult, ArtifactView, BrokerMessage, BuildFailure, BuildState,
    ChannelAccount, ChannelOp, ChannelResult, ConflictEntry, DeliveryRefusal, DeliveryTarget,
    DeviceChallenge, DeviceHello, DeviceRefusal, DeviceWelcome, EgressOp, EgressResult, FeedSpec,
    GoalsOp, GoalsResult, HookOp, HookResult, HookSource, IdeaRef, IdeaRefusal, IdeaSource,
    IdeaState, IdeaSummary, InspectionFinding, InspectionVerdict, MAX_SKILL_INPUT_BYTES,
    MAX_SKILL_OUTPUT_BYTES, MediaHeader, MemoryOp, MemoryResult, QueryOp, QueryResult,
    ScheduleGrant, ScheduleKind, ScheduleOp, ScheduleRefusal, ScheduleResult, ScheduleSpec,
    ScheduleSummary, SessionRequest, SessionResponse, SkillOp, SkillResult, SpaceOp, SpaceRefusal,
    SpaceResult, SyncChange, SyncItem, VaultAction, VaultOp, VaultResult,
};
pub use peer::{PeerCredentials, peer_credentials};
pub use secret::{MAX_SECRET_BYTES, decode as decode_secret, encode as encode_secret};
pub use subagent::{
    ActivityEvent, ChildEnvelope, ChildFailure, ChildOutcome, ChildReport, ChildVerdict,
    SubagentEvent, SubagentOp, SubagentRefusal, SubagentResult, grant_label, parse_grant_label,
};
pub use types::{CapabilityName, CellId, PeerPid, PeerUid, SecretName, SocketPath};
