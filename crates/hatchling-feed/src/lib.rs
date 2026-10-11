//! `hatchling-feed`: the feed, the user's personal newspaper.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! A feed is a schedule whose run is a **digest** of the user's own
//! records: what changed in their goals, tracking, and tagged memory over
//! a window, delivered to a channel. Three rules shape it:
//!
//! - **Provenance is load-bearing.** Every entry carries the transitive
//!   label of the record it observes, computed at the store layer
//!   ([`hatchling_goals::Provenance`]); an entry cannot be made without
//!   one ([`digest`]).
//! - **Observations, never prescriptions.** Entries say what a record
//!   says and when, quoting its text. A suggestion is an idea, with its
//!   own lifecycle and confirmation, never feed content.
//! - **The feed reads; it never acts.** A feed's grants are derived from
//!   its spec, one read grant per kind of section ([`spec`]), and a
//!   digest is generated through a read-only [`digest::Reader`], only for
//!   the sections whose grant the run still holds.
//!
//! v0.1 is rules: the user's spec, applied to their records. Nothing here
//! is learned, and no model writes the digest.

pub mod digest;
pub mod spec;

pub use digest::{Digest, Entry, Found, MAX_QUOTE_CHARS, Reader, generate};
pub use spec::{MAX_ITEMS, MAX_LOOKBACK_HOURS, MAX_TAGS, Section, Spec};

/// A digest's window: from `since`, included, to `until`, excluded, in
/// Unix seconds; the same as the tracking store's own ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// The start.
    pub since: u64,
    /// The end.
    pub until: u64,
}

impl Window {
    /// The window of `lookback_secs` ending at `due` (a fire's due time);
    /// a due time before the epoch ends at the epoch.
    #[must_use]
    pub fn ending(due: i64, lookback_secs: u64) -> Self {
        let until = u64::try_from(due).unwrap_or(0);
        Self {
            since: until.saturating_sub(lookback_secs),
            until,
        }
    }
}
