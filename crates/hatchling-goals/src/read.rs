//! The store layout and its read path, shared by the writer
//! ([`GoalsStore`](crate::GoalsStore)) and the feed's reader
//! ([`FeedReader`](crate::FeedReader)).
//!
//! As files ([`Files`](crate::Files)), the layout before WO-047:
//!
//! ```text
//! <root>/goals/<uid>/<goal-id>.json      one 0600 file per goal
//! <root>/tracking/<uid>/log.ndjson       one 0600 append-only log per user
//! ```
//!
//! The uid is the kernel-authenticated peer, never a name the agent
//! chose: one user's goals and log are kept apart from another's, so no
//! op can name its way into them.
//!
//! Nothing in this module writes: it reads through a
//! [`GoalsSource`](crate::GoalsSource), which has no write method. The
//! feed reader is read-only by construction partly because this is the
//! only store code it calls (pinned by
//! `the_feed_reader_exposes_no_write_surface`).

use std::path::{Path, PathBuf};

use crate::Error;
use crate::backing::GoalsSource;
use crate::name::GoalId;
use crate::record::{Goal, TrackingEntry};

/// Directory of per-user goal directories.
pub(crate) const GOALS_DIR: &str = "goals";
/// Directory of per-user tracking directories.
pub(crate) const TRACKING_DIR: &str = "tracking";
/// Goal file suffix. Only files with this suffix are goals; temp files
/// and anything else in the directory are ignored.
pub(crate) const GOAL_SUFFIX: &str = ".json";
/// The one tracking log file in a user's tracking directory.
pub(crate) const TRACKING_LOG: &str = "log.ndjson";

/// The file of one goal. The id alphabet has no separator, so joining
/// is safe by construction.
pub(crate) fn goal_path(dir: &Path, id: &GoalId) -> PathBuf {
    dir.join(format!("{}{GOAL_SUFFIX}", id.as_str()))
}

/// Parses one goal's bytes.
///
/// The goal must name itself: a record whose contents carry another id
/// was copied or renamed outside the store, and is corrupt rather than
/// silently served under the wrong name.
pub(crate) fn parse_goal(bytes: &[u8], id: &GoalId) -> Result<Goal, Error> {
    let goal: Goal = serde_json::from_slice(bytes).map_err(|_| Error::Corrupt)?;
    if goal.id != *id {
        return Err(Error::Corrupt);
    }
    Ok(goal)
}

/// User `uid`'s goal `id`, or `None` when there's none.
pub(crate) fn goal<S: GoalsSource + ?Sized>(
    source: &S,
    uid: u32,
    id: &GoalId,
) -> Result<Option<Goal>, Error> {
    source
        .goal(uid, id)?
        .map(|bytes| parse_goal(&bytes, id))
        .transpose()
}

/// Every goal of user `uid`, ordered by id.
///
/// A goal that fails to parse, or doesn't name itself, is corrupt: a
/// listing never silently skips a record. One listed and gone by the
/// time it's read is skipped.
pub(crate) fn goals<S: GoalsSource + ?Sized>(source: &S, uid: u32) -> Result<Vec<Goal>, Error> {
    let mut goals = Vec::new();
    for id in source.goal_ids(uid)? {
        if let Some(goal) = goal(source, uid, &id)? {
            goals.push(goal);
        }
    }
    goals.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(goals)
}

/// The entries of a log's bytes: each line, without its newline. A last
/// line without one is an entry too.
pub(crate) fn lines(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut entries: Vec<Vec<u8>> = bytes
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::to_vec)
        .collect();
    if bytes.ends_with(b"\n") || bytes.is_empty() {
        entries.pop();
    }
    entries
}

/// User `uid`'s tracking log in sequence order, keeping the entries
/// `keep` admits. An absent log is empty.
///
/// Every entry must be one whose `seq` is its place in the log: a
/// corrupt entry, a gap, or a reordering means the log was changed
/// outside the append path, and the read fails rather than present a
/// false history.
pub(crate) fn tracking<S: GoalsSource + ?Sized>(
    source: &S,
    uid: u32,
    keep: impl FnMut(&TrackingEntry) -> bool,
) -> Result<Vec<TrackingEntry>, Error> {
    parse_tracking(&source.tracking(uid)?, keep)
}

/// [`tracking`], from the entries' bytes.
pub(crate) fn parse_tracking(
    lines: &[Vec<u8>],
    mut keep: impl FnMut(&TrackingEntry) -> bool,
) -> Result<Vec<TrackingEntry>, Error> {
    let mut entries = Vec::new();
    let mut expected: u64 = 0;
    for line in lines {
        let entry: TrackingEntry = serde_json::from_slice(line).map_err(|_| Error::Corrupt)?;
        if entry.seq != expected {
            return Err(Error::Corrupt);
        }
        expected = expected.saturating_add(1);
        if keep(&entry) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::Files;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-goals-read-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    /// A user's directory under `kind`, `0700`, as a store makes it.
    fn user_dir(root: &Path, kind: &str) -> PathBuf {
        let dir = root.join(kind).join("1000");
        for level in [root.join(kind), dir.clone()] {
            std::fs::create_dir_all(&level).unwrap();
            std::fs::set_permissions(&level, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        dir
    }

    fn entry_line(seq: u64, at: u64) -> Vec<u8> {
        format!(r#"{{"seq":{seq},"at_secs":{at},"source":"user","text":"t{seq}"}}"#).into_bytes()
    }

    fn files(root: &Path) -> Files {
        Files::of_this_user(root)
    }

    #[test]
    fn a_logs_lines_are_its_entries() {
        assert_eq!(lines(b""), Vec::<Vec<u8>>::new());
        assert_eq!(lines(b"a\nb\n"), [b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(lines(b"a\nb"), [b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(
            lines(b"a\n\nb\n"),
            [b"a".to_vec(), Vec::new(), b"b".to_vec()]
        );
    }

    #[test]
    fn scan_keeps_what_the_filter_admits_in_order() {
        let log = [entry_line(0, 5), entry_line(1, 6), entry_line(2, 7)];
        let all = parse_tracking(&log, |_| true).unwrap();
        assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![0, 1, 2]);
        let late = parse_tracking(&log, |e| e.at_secs >= 6).unwrap();
        assert_eq!(late.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
        let root = test_dir("scan");
        assert!(tracking(&files(&root), 1000, |_| true).unwrap().is_empty());
        let dir = user_dir(&root, TRACKING_DIR);
        std::fs::write(
            dir.join(TRACKING_LOG),
            [log.join(&b'\n'), b"\n".to_vec()].concat(),
        )
        .unwrap();
        let read = tracking(&files(&root), 1000, |_| true).unwrap();
        assert_eq!(read, all);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_refuses_a_gap_a_reorder_or_a_corrupt_line() {
        let corrupt =
            |log: &[Vec<u8>]| matches!(parse_tracking(log, |_| true), Err(Error::Corrupt));
        assert!(corrupt(&[entry_line(0, 5), entry_line(2, 7)]));
        assert!(corrupt(&[entry_line(1, 5), entry_line(0, 7)]));
        assert!(matches!(
            parse_tracking(&[entry_line(0, 5), b"{not json".to_vec()], |_| false),
            Err(Error::Corrupt)
        ));
        assert!(corrupt(&[entry_line(0, 5), Vec::new(), entry_line(1, 6)]));
    }

    #[test]
    fn scan_reports_errors_other_than_absence() {
        let root = test_dir("unreadable");
        let dir = user_dir(&root, TRACKING_DIR);
        std::fs::create_dir(dir.join(TRACKING_LOG)).unwrap();
        assert!(matches!(
            tracking(&files(&root), 1000, |_| true),
            Err(Error::Store { .. })
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_goal_must_name_itself() {
        let body = br#"{"id":"g1","title":"t","detail":"","status":"open","origin":"user-explicit","created_at_secs":1,"updated_at_secs":1}"#;
        let g1: GoalId = "g1".parse().unwrap();
        let g2: GoalId = "g2".parse().unwrap();
        assert_eq!(parse_goal(body, &g1).unwrap().id, g1);
        assert!(matches!(parse_goal(body, &g2), Err(Error::Corrupt)));
        let root = test_dir("selfname");
        let dir = user_dir(&root, GOALS_DIR);
        std::fs::write(goal_path(&dir, &g2), body).unwrap();
        assert!(matches!(goals(&files(&root), 1000), Err(Error::Corrupt)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn goals_are_ordered_by_id_and_stray_names_refused() {
        let root = test_dir("list");
        let dir = user_dir(&root, GOALS_DIR);
        for id in ["g3", "g1", "g2"] {
            let body = format!(
                r#"{{"id":"{id}","title":"t","detail":"","status":"open","origin":"user-explicit","created_at_secs":1,"updated_at_secs":1}}"#
            );
            std::fs::write(dir.join(format!("{id}.json")), body).unwrap();
        }
        std::fs::write(dir.join(".123.0.tmp"), b"partial").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let ids: Vec<String> = goals(&files(&root), 1000)
            .unwrap()
            .into_iter()
            .map(|g| g.id.to_string())
            .collect();
        assert_eq!(ids, vec!["g1", "g2", "g3"]);
        std::fs::write(dir.join("bad name.json"), b"{}").unwrap();
        assert!(matches!(goals(&files(&root), 1000), Err(Error::Corrupt)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
