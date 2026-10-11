//! `hatchling-research`: deep research runs.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! A user asks a hard question; the run plans, gathers, synthesizes, and
//! delivers a cited report as an artifact. The run is a **mode of the
//! agent loop**, not a second loop: each phase is a bounded run of
//! [`hatchling_loop::Loop`] with its own tools ([`run`]).
//!
//! Three rules shape it:
//!
//! - **Sources are evidence, never instructions.** Gathering never shows
//!   the model what a source says, so a source cannot steer what is read
//!   next. Synthesis sees every source in the untrusted-data envelope,
//!   placed below the system prompt and the task, and has no tools, so
//!   nothing a source says can make the run act.
//! - **The approved plan is the gather phase's whole reach.** Sources are
//!   fetched by plan id, each once; a new source needs an amendment the
//!   user approves, and every amendment is in the report ([`gather`]).
//! - **Citations are load-bearing.** Claims bind only to sources the run
//!   read, with their depth; uncited claims are flagged; disagreements
//!   keep their sides; freshness is stated up front ([`report`]).
//!
//! Every run is bounded in sources and time ([`bounds`]). Fetching goes
//! through a [`Fetcher`]: in the agent, the egress proxy, whose
//! allowlist decides what may be reached at all.

pub mod bounds;
pub mod gather;
pub mod plan;
pub mod report;
pub mod run;
pub mod url;

pub use bounds::{Bounds, MAX_RUN_SECS, MAX_SOURCES};
pub use gather::{
    Amendment, Approver, Clock, Decision, Fetch, FetchFailure, Fetched, Fetcher, Gathered,
    MAX_EVIDENCE_BYTES, SourceRecord, StopReason, SystemClock,
};
pub use plan::{MAX_FIELD_BYTES, Origin, Plan, PlanError, PlannedSource};
pub use report::{Cite, Claim, Depth, Disagreement, Flag, Report, ReportError, Status};
pub use run::{Outcome, Research, ResearchError};
pub use url::{MAX_URL_BYTES, SourceUrl};

/// `secs` as a UTC timestamp, `2026-10-02T12:00:00Z`.
#[must_use]
pub fn timestamp(secs: i64) -> String {
    jiff::Timestamp::from_second(secs).map_or_else(|_| format!("@{secs}"), |at| at.to_string())
}

#[cfg(test)]
mod tests {
    use super::timestamp;

    #[test]
    fn a_time_is_stated_in_utc() {
        assert_eq!(timestamp(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(i64::MAX), format!("@{}", i64::MAX));
    }
}
