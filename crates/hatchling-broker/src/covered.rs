//! The requests the owner's standing grants covered (WO-036): kept, the
//! owner's alone, in a log of their own beside the grants store.
//!
//! A standing site grant is a standing outbound channel: the address the
//! agent asks for carries whatever it puts there. So what a grant covered
//! is kept with that address, and the owner sees it in Activity.
//!
//! - **Its own log** (`<grants dir>.covered`), bounded as every
//!   [`crate::kept_log`] is, with a bound of its own ([`MAX_KEPT`]). Never
//!   the activity log or the decisions log: a looping agent mustn't flush
//!   either.
//! - **Aggregated** by task and grant. A task's first [`FIRST_KEPT`] requests
//!   under a grant are each kept, with their address. Past those, a record
//!   that counts them is kept only as the count reaches a power of two, so
//!   a task that loops adds a few records, not one per request.
//! - **Kept, or not covered.** A request the grant would cover whose record
//!   can't be kept isn't covered: the owner is asked.

use std::path::{Path, PathBuf};

use hatchling_core::ui::CoveredRecord;

use crate::Config;

/// How many records are kept, and listed: the newest.
pub const MAX_KEPT: usize = 1000;

/// How many lines the log may grow to before it's cut back to
/// [`MAX_KEPT`].
pub const MAX_LINES: usize = 2 * MAX_KEPT;

/// How many of a task's requests under one grant are each kept with their
/// address.
pub const FIRST_KEPT: u64 = 20;

/// The log `config`'s grants are recorded in; `None` when the broker keeps
/// no grants.
#[must_use]
pub fn log_path(config: &Config) -> Option<PathBuf> {
    let grants = &config.web.grants;
    if grants.as_os_str().is_empty() {
        return None;
    }
    let mut name = grants.file_name()?.to_os_string();
    name.push(".covered");
    Some(grants.with_file_name(name))
}

/// Whether a task's `count`th request under one grant is kept: each of the
/// first [`FIRST_KEPT`], then each that reaches a power of two.
#[must_use]
pub const fn kept(count: u64) -> bool {
    count <= FIRST_KEPT || count.is_power_of_two()
}

/// Keeps `record` at `path`.
///
/// # Errors
///
/// Why it couldn't be kept: the request it records isn't covered.
pub fn record(path: &Path, record: &CoveredRecord) -> std::io::Result<()> {
    crate::kept_log::append_within(path, record, MAX_LINES, MAX_KEPT)
}

/// The records `config` keeps, the newest first; none when it keeps no
/// grants.
#[must_use]
pub fn list(config: &Config) -> Vec<CoveredRecord> {
    log_path(config).map_or_else(Vec::new, |path| {
        crate::kept_log::list_within(&path, MAX_KEPT)
    })
}

#[cfg(test)]
mod tests {
    use hatchling_core::ui::{CoveredVia, GrantScope};

    use super::*;

    /// The first requests are each kept, then as the count doubles.
    #[test]
    fn a_tasks_requests_are_kept_first_then_as_they_double() {
        assert_eq!(FIRST_KEPT, 20);
        let kept: Vec<u64> = (1..=300).filter(|count| kept(*count)).collect();
        let mut expected: Vec<u64> = (1..=20).collect();
        expected.extend([32, 64, 128, 256]);
        assert_eq!(kept, expected);
    }

    /// Beside the grants store, and none where there's no store.
    #[test]
    fn the_log_is_beside_the_grants() {
        let mut config = crate::api_auth::tests::owner_config(None);
        assert_eq!(log_path(&config), None);
        assert!(list(&config).is_empty());
        config.web.grants = PathBuf::from("/var/lib/hatchling/sites");
        assert_eq!(
            log_path(&config),
            Some(PathBuf::from("/var/lib/hatchling/sites.covered"))
        );
    }

    /// Kept, listed the newest first, within its own bound.
    #[test]
    fn records_are_kept_and_listed_within_their_own_bound() {
        let scratch = crate::api_auth::tests::scratch("covered-log");
        let mut config = crate::api_auth::tests::owner_config(None);
        config.web.grants = scratch.0.join("sites");
        let path = log_path(&config).unwrap();
        let one = |count: u64| CoveredRecord {
            key: String::from("web.visit:https://a.example"),
            scope: GrantScope::Always,
            url: Some(format!("https://a.example/{count}")),
            via: CoveredVia::Fetch,
            task: String::from("t"),
            count,
            at: String::from("2026-11-20T08:00:00Z"),
        };
        for count in 1..=3 {
            record(&path, &one(count)).unwrap();
        }
        let listed = list(&config);
        assert_eq!(
            listed.iter().map(|record| record.count).collect::<Vec<_>>(),
            [3, 2, 1]
        );
        assert_eq!(MAX_LINES, 2 * MAX_KEPT);
        assert_eq!(
            (MAX_KEPT, crate::kept_log::MAX_KEPT),
            (1000, 200),
            "a bound of its own"
        );
    }
}
