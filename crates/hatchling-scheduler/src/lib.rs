//! `hatchling-scheduler`: scheduled work.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! A schedule is broker-owned state: when it fires (a cron expression or
//! a one-shot local time, always in an explicit IANA timezone), the task
//! a run is given, the grants that run may use, and the channel its
//! output goes to. This crate holds the parts that need no broker:
//!
//! - [`cron`]: five-field cron expressions, evaluated in a timezone
//!   across daylight-saving gaps and folds.
//! - [`schedule`]: timing, the stored record, and create-time validation,
//!   including subset-closed grants (a schedule's grants are a subset of
//!   its creator's).
//! - [`dispatcher`]: the state machine that fires due schedules (with
//!   overlap and missed-fire policies, and a fire-time grant re-check),
//!   delivers their output, and parks what it cannot deliver. It fires
//!   hooks on broker-asserted events through the same path.
//! - [`hook`]: hooks, validated against a closed set of event
//!   sources the broker can authenticate.
//!
//! Follow-ups are one-shot schedules a run asks for, bounded by
//! [`MAX_FOLLOWUP_DEPTH`] and by the run's own grants
//! ([`validate_followup`]).
//!
//! The broker owns the store, the session ops, and the real launcher
//! and delivery path; the cell never sees any of it.

pub mod cron;
pub mod dispatcher;
pub mod hook;
pub mod schedule;

pub use cron::{CronError, CronExpr};
pub use dispatcher::{
    Claim, Delivery, Dispatcher, Launcher, MISSED_GRACE_SECS, Parked, Run, RunRequest, Services,
    Store, TickReport,
};
pub use hook::{Hook, Sources, validate_hook};
pub use jiff::Timestamp;
pub use schedule::{
    Creator, FollowupParent, Ledger, MAX_FOLLOWUP_DELAY_SECS, MAX_FOLLOWUP_DEPTH, MAX_GRANTS,
    MAX_TASK_BYTES, Occurrence, OccurrenceState, Outcome, Schedule, Timing, effective_grants,
    validate, validate_followup, validate_space_task,
};
