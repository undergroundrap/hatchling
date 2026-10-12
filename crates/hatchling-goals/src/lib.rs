//! `hatchling-goals`: the broker-owned goals and tracking store, and
//! the read-only query interface the feed reads from.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! - **Goals** the user is working toward: create, list, update, close.
//!   User-explicit only — no inferred goals, no auto-close, no delete.
//! - **Tracking**: an append-only, timestamped log of what happened,
//!   each entry labeled with the source it was reported from. No edit,
//!   no delete; a correction is a new entry.
//! - **The feed's queries** ([`FeedReader`]): one path over goals,
//!   tracking, and tag-scoped memory, where every item carries its
//!   [`Provenance`] from the store layer.
//!
//! Same trust shape as the memory store: the broker owns the
//! files, cells reach them only through grant-checked session ops, and
//! every op is bound to the kernel-authenticated peer uid, never to a
//! name the agent chose. The files live beside memory's under the same
//! directory discipline ([`hatchling_memory::owner_dir`]): `0700`
//! directories validated on every open, each path component checked
//! before anything beneath it is touched, `0600` records written
//! atomically.
//!
//! Writes go through [`GoalsStore`]; the feed never holds one. Its
//! [`FeedReader`] has no write method, and the read path it uses
//! validates directories without creating or repairing them.

pub mod error;
pub mod feed;
pub mod name;
pub mod read;
pub mod record;
pub mod store;

pub use error::Error;
pub use feed::{
    FeedBody, FeedItem, FeedPage, FeedQuery, FeedReader, Origin, PAGE_BUDGET_BYTES, Provenance,
    SourceStore, StatusFilter, page,
};
pub use name::{GoalId, SourceLabel};
pub use record::{
    Goal, GoalOrigin, GoalOutcome, GoalStatus, MAX_DETAIL_BYTES, MAX_GOALS_PER_USER,
    MAX_TITLE_BYTES, MAX_TRACKING_ENTRIES, MAX_TRACKING_TEXT_BYTES, TimeRange, TrackingEntry,
};
pub use store::{GoalChange, GoalEdit, GoalsStore};
