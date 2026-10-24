//! `hatchling-broker`: the daemon agents connect to.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! The broker owns the host side of the trust boundary. It binds the
//! Unix socket, authorizes every connecting peer against kernel-reported
//! credentials before reading a single byte of its traffic, and completes
//! the handshake. Everything security-relevant is typed; every failure is
//! fail-closed.
//!
//! The shared contract (framing, messages, identity types, peer
//! credentials) lives in [`hatchling_core`].

pub mod activation;
pub use activation::{ConfirmError, confirm_activation};
pub mod api;
pub use api::{Api, open_api};
pub mod api_auth;
pub use api_auth::{ApiConfig, Caller, PeerProvider, Provider};
pub mod api_ops;
pub mod api_stream;
pub mod approval;
pub mod approval_board;
pub mod approvals;
pub mod artifact_ops;
pub mod browser_board;
pub mod browser_bundle;
pub mod browser_cell;
pub mod browser_credentials;
pub mod browser_ops;
pub mod browser_profiles;
pub mod browser_proxy;
pub mod browser_session;
pub use artifact_ops::{SealedArtifactStore, handle_artifact_op, held_secrets, is_workspace_name};
pub mod cell;
pub use cell::{CellConfig, CellHandle, CellIds, CellLimits, ScheduledRun, spawn_cell};
pub mod channel;
pub mod chat_run;
pub mod cli;
pub use approval::{ApprovalLedger, ApprovalToken, MintError, Verdict};
pub use channel::{ChannelRegistry, handle_channel_op};
pub mod config;
pub mod connections;
pub use connections::{ConnectionSlots, Refused, Slot};
pub mod custody;
pub mod device;
pub mod egress_authority;
pub mod egress_pages;
pub mod egress_path;
pub use custody::{AccountBook, HandleRejection};
pub use device::{DeviceEndpoint, open_device_endpoint, pairing_refusals, serve_device};
pub mod environment;
pub mod error;
pub mod feed_run;
pub use feed_run::{FEED_TASK, FeedLauncher, FeedRun};
pub mod goals_ops;
pub mod harness;
pub mod harness_relay;
pub mod harness_tree;
pub use harness::{HarnessKind, Harnesses};
pub mod ingress;
pub use ingress::{Ingress, open_ingress};
pub mod listener;
pub mod machine;
pub mod memory_ops;
pub mod operator;
pub mod output_gate;
pub use output_gate::{RedactionConfig, Rules, RulesHandle, RulesWorker};
pub mod run_launcher;
pub use run_launcher::{AGENT_RUN_SUBCOMMAND, CellLauncher, RunEnv, agent_run_main, scoped_config};
pub mod redactions;
pub use redactions::{FileAudit, FileOverrides, run_operator, run_redactions};
pub mod keeper;
pub mod schedule_ops;
pub mod secrets;
pub use schedule_ops::{
    HubDelivery, SealedScheduleStore, authenticated_channels, dispatch_once, handle_hook_op,
    handle_schedule_op, held_grants,
};
pub mod serve;
pub mod site_grants;
pub mod skill_cell;
pub mod skill_invoke;
pub mod skill_ops;
pub mod skill_run;
pub mod skills;
pub mod space_ops;
pub mod subagent_ops;
pub mod spawner;
pub mod sweep;
pub use space_ops::{EndAdmissions, MediaMeta, SealedSpaceStore, SpaceSession, handle_space_op};
#[cfg(test)]
mod test_log;
#[cfg(test)]
mod test_socket;
pub mod trust_roots;
pub mod vault_access;
pub mod vault_keyring;
pub mod vault_ops;
pub mod web_ops;

pub use config::{
    ArtifactConfig, ChannelAccountSpec, Config, DEFAULT_MAX_CONNECTIONS, EgressAllowlist,
    EgressConfig, FollowupScope, GoalsAction, GoalsConfig, MAX_MAX_CONNECTIONS, MemoryAction,
    MemoryConfig, MemoryGrant, SchedulerConfig, SearchLog, SkillConfig, SpaceConfig, VaultConfig,
    VaultGrant, WebConfig,
};
pub use error::Error;
pub use goals_ops::{handle_goals_op, handle_query_op};
pub use listener::{BIND_UMASK, OwnerOnlyListener, bind_listener, bind_socket};
pub use serve::{
    AgentCell, run_serve, serve_cell_connection, serve_cell_forever, serve_connection,
    serve_connection_with_ingress, serve_connection_with_runs, serve_forever, spawn_agent_cell,
};
pub use skill_invoke::{SkillDenial, check_skill_call};
pub use skill_ops::handle_skill_op;
pub use skill_run::SkillRuntime;
pub use skills::{Reload, SkillSet, Skills};
pub use subagent_ops::{Ledger, Subagents, WITHHELD, check_spawn, child_config, partial_of};
pub use trust_roots::TrustRoots;
pub use vault_ops::{
    check_sealed_stores, hand_store_keys, load_master_key, migrate_vault, rekey_vault,
    reseal_stores, run_migrate_vault, run_rekey_vault, sealed_stores, validate_sealed_vault,
};
