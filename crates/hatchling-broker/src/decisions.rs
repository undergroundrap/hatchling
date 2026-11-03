//! The owner's decisions, kept: each approval they gave or refused, as they
//! were shown it, so the app's history survives a restart.
//!
//! A line per decision in one file beside the schedule store
//! (`<scheduler dir>.decisions`): when, the card's summary and fields, and
//! the decision. Only what the owner was shown, which the broker already
//! held; nothing an approval ran. Bounded: past [`MAX_LINES`] it keeps the
//! newest [`MAX_KEPT`].

use std::fs::OpenOptions;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::ui::DecisionRecord;

/// How many decisions are kept, and listed: the newest.
pub const MAX_KEPT: usize = 200;

/// How many lines the file may grow to before it's cut back to
/// [`MAX_KEPT`].
pub const MAX_LINES: usize = 2 * MAX_KEPT;

/// The largest file read.
const MAX_FILE_BYTES: u64 = 8 << 20;

/// Serializes the broker's writers.
static WRITES: Mutex<()> = Mutex::new(());

/// Keeps `record` in the log at `path`. A decision that can't be kept is
/// logged; the decision itself stands.
pub fn record(path: &Path, record: &DecisionRecord) {
    if let Err(error) = append(path, record) {
        tracing::warn!(%error, "a decision wasn't kept in the history");
    }
}

fn append(path: &Path, record: &DecisionRecord) -> std::io::Result<()> {
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?
        .write_all(line.as_bytes())?;
    let lines = lines(path)?;
    if lines.len() > MAX_LINES {
        let kept = lines.get(lines.len() - MAX_KEPT..).unwrap_or_default();
        let next = path.with_extension("decisions.next");
        let mut text = kept.join("\n");
        text.push('\n');
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&next)?
            .write_all(text.as_bytes())?;
        std::fs::rename(&next, path)?;
    }
    Ok(())
}

/// The file's lines, if it's small enough to read.
fn lines(path: &Path) -> std::io::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_FILE_BYTES {
        return Err(std::io::Error::other(
            "the decision history is too large to read",
        ));
    }
    BufReader::new(file).lines().collect()
}

/// The decisions kept at `path`, the newest first; none when there's no
/// file or it can't be read. A line that doesn't read is skipped.
#[must_use]
pub fn list(path: &Path) -> Vec<DecisionRecord> {
    let Ok(lines) = lines(path) else {
        return Vec::new();
    };
    lines
        .iter()
        .rev()
        .filter_map(|line| serde_json::from_str(line).ok())
        .take(MAX_KEPT)
        .collect()
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
        let listed = list(&path);
        assert_eq!(
            listed,
            [decided(2, Decision::Deny), decided(1, Decision::Approve)]
        );
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // A line that doesn't read is skipped.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("not a decision\n");
        std::fs::write(&path, text).unwrap();
        assert_eq!(list(&path).len(), 2);
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
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            MAX_KEPT,
            "cut back past {MAX_LINES} lines"
        );
        assert!(
            !path.with_extension("decisions.next").exists(),
            "nothing left over"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_history_that_cant_be_written_or_read_keeps_nothing() {
        let dir = scratch("broken");
        let path = dir.join("missing").join("schedules.decisions");
        record(&path, &decided(1, Decision::Approve));
        assert!(list(&path).is_empty());
        let big = dir.join("big.decisions");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(list(&big).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
