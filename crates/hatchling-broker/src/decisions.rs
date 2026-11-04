//! The owner's decisions, kept: each approval they gave or refused, as they
//! were shown it, so the app's history survives a restart.
//!
//! A line per decision in one file beside the schedule store
//! (`<scheduler dir>.owner/decisions`): when, the card's summary and fields, and
//! the decision. Only what the owner was shown, which the broker already
//! held; nothing an approval ran. Bounded as every [`crate::kept_log`] is:
//! past [`MAX_LINES`] it keeps the newest [`MAX_KEPT`].

use std::path::Path;

use hatchling_core::ui::DecisionRecord;

pub use crate::kept_log::{MAX_KEPT, MAX_LINES};

/// Keeps `record` in the log at `path`. A decision that can't be kept is
/// logged; the decision itself stands.
pub fn record(path: &Path, record: &DecisionRecord) {
    if let Err(error) = crate::kept_log::append(path, record) {
        tracing::warn!(%error, "a decision wasn't kept in the history");
    }
}

/// The decisions kept at `path`, the newest first; none when there's no
/// file or it can't be read. A line that doesn't read is skipped.
#[must_use]
pub fn list(path: &Path) -> Vec<DecisionRecord> {
    crate::kept_log::list(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchling_core::ui::{ConfirmationField, Decision, FieldSource};

    fn decided(id: usize, decision: Decision) -> DecisionRecord {
        DecisionRecord {
            confirmation_id: format!("req-{id}"),
            summary: String::from("Remember this about you?"),
            fields: vec![ConfirmationField {
                label: String::from("What it would remember"),
                value: format!("fact {id}"),
                source: FieldSource::Agent,
            }],
            decision,
            decided_at: String::from("2026-11-03T12:00:00Z"),
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-decisions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn decisions_are_kept_and_listed_newest_first() {
        let dir = scratch("kept");
        let path = dir.join("schedules.decisions");
        assert!(list(&path).is_empty(), "no file: none");
        record(&path, &decided(1, Decision::Approve));
        record(&path, &decided(2, Decision::Deny));
        assert_eq!(
            list(&path),
            [decided(2, Decision::Deny), decided(1, Decision::Approve)]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_history_is_bounded_to_the_newest() {
        let dir = scratch("bounded");
        let path = dir.join("schedules.decisions");
        for id in 0..=MAX_LINES {
            record(&path, &decided(id, Decision::Approve));
        }
        let listed = list(&path);
        assert_eq!(listed.len(), MAX_KEPT);
        assert_eq!(
            listed.first().map(|kept| kept.confirmation_id.as_str()),
            Some(format!("req-{MAX_LINES}").as_str())
        );
        assert!(
            !path.with_extension("decisions.next").exists(),
            "nothing left over"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_decision_that_cant_be_kept_still_stands() {
        let dir = scratch("broken");
        let path = dir.join("missing").join("schedules.decisions");
        record(&path, &decided(1, Decision::Approve));
        assert!(list(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
