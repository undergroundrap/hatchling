//! The write side: goals and the tracking log, under a `0700` root.
//!
//! Only the broker holds a [`GoalsStore`], and only its grant-checked
//! goals ops call it. The feed never does: it reads through
//! [`FeedReader`](crate::FeedReader), which has no write method and
//! never touches this type.
//!
//! Goals are user-explicit. The store records what the user asked for
//! and changes a goal only when asked to: there is no inference, no
//! auto-close, and no delete (a closed goal is kept, and can no longer
//! be edited). Tracking is append-only: there is no edit or delete,
//! and a correction is a new entry.
//!
//! The store decides everything about a record; its backing
//! ([`GoalsBacking`]) keeps the bytes: [`Files`] as before WO-047, or
//! the broker's sealed store.

use std::path::Path;

use hatchling_memory::{MemoryPrincipal, owner_dir};

use crate::Error;
use crate::backing::{Files, GoalsBacking};
use crate::name::{GoalId, SourceLabel};
use crate::read;
use crate::record::{
    self, Goal, GoalOrigin, GoalOutcome, GoalStatus, MAX_GOALS_PER_USER, MAX_TRACKING_ENTRIES,
    TrackingEntry,
};

/// The per-user bounds the store enforces. Split out so tests can
/// exercise each limit without writing ten thousand entries.
#[derive(Debug, Clone, Copy)]
struct Limits {
    goals: usize,
    tracking_entries: u64,
}

impl Limits {
    /// The shipped bounds.
    const SHIPPED: Self = Self {
        goals: MAX_GOALS_PER_USER,
        tracking_entries: MAX_TRACKING_ENTRIES,
    };
}

/// The fields a goal update changes. `None` leaves a field as it is;
/// an edit that names no field is refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct GoalEdit<'a> {
    /// A new title.
    pub title: Option<&'a str>,
    /// A new detail.
    pub detail: Option<&'a str>,
}

/// What an update or close did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalChange {
    /// The goal changed; here it is as stored now.
    Changed(Goal),
    /// No goal of this user has the id.
    NotFound,
    /// The goal is closed: it can no longer be edited or closed again.
    AlreadyClosed,
}

/// The broker-owned goals and tracking store.
///
/// `GoalsStore` is `Sync`: every method takes `&self`, and the backing
/// serializes. Tracking appends hold the log's lock from counting to
/// keeping, so concurrent sessions of one user never share a sequence
/// number.
#[derive(Debug)]
pub struct GoalsStore {
    /// The per-user bounds.
    limits: Limits,
    /// Where the records are kept.
    backing: Box<dyn GoalsBacking>,
}

impl GoalsStore {
    /// Opens (creating if absent) the store rooted at `root`, its
    /// records as files ([`Files`]).
    ///
    /// The root must be a directory with mode `0700` owned by the
    /// current uid — created that way if absent, verified if present.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] if the root is not a directory, has an
    /// insecure mode or owner, or cannot be created.
    pub fn open(root: &Path) -> Result<Self, Error> {
        Self::open_as(root, rustix::process::getuid().as_raw(), Limits::SHIPPED)
    }

    /// The store rooted at `root`, its records kept by `backing`. The
    /// root is checked as [`GoalsStore::open`] checks it.
    ///
    /// # Errors
    ///
    /// As [`GoalsStore::open`].
    pub fn with_backing(root: &Path, backing: Box<dyn GoalsBacking>) -> Result<Self, Error> {
        owner_dir::ensure_owner_only_dir(root, rustix::process::getuid().as_raw())?;
        Ok(Self {
            limits: Limits::SHIPPED,
            backing,
        })
    }

    /// Opens the store, requiring the root to be owned by `owner`.
    /// Split out so tests exercise the ownership check and the limits
    /// hermetically.
    fn open_as(root: &Path, owner: u32, limits: Limits) -> Result<Self, Error> {
        owner_dir::ensure_owner_only_dir(root, owner)?;
        Ok(Self {
            limits,
            backing: Box::new(Files::new(root, owner)),
        })
    }

    /// Records a new open goal for the principal, broker-stamped, with
    /// a store-minted id. The binding is the peer uid from kernel
    /// credentials.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidText`] for a bad title or detail,
    /// [`Error::GoalLimit`] when the user already holds the maximum
    /// number of goals, or [`Error::Store`] / [`Error::Io`] on
    /// filesystem failure.
    pub fn create_goal(
        &self,
        principal: &MemoryPrincipal,
        title: &str,
        detail: &str,
        now_secs: u64,
    ) -> Result<Goal, Error> {
        record::check_title(title)?;
        record::check_detail(detail)?;
        if self.backing.goal_ids(principal.peer_uid)?.len() >= self.limits.goals {
            return Err(Error::GoalLimit {
                max: self.limits.goals,
            });
        }
        let goal = Goal {
            id: GoalId::generate()?,
            title: title.to_owned(),
            detail: detail.to_owned(),
            status: GoalStatus::Open,
            origin: GoalOrigin::UserExplicit,
            created_at_secs: now_secs,
            updated_at_secs: now_secs,
        };
        self.store_goal(principal, &goal)?;
        tracing::info!(goal_id = %goal.id, "created goal");
        Ok(goal)
    }

    /// Changes the title and/or detail of one of the principal's open
    /// goals.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyEdit`] when the edit names no field,
    /// [`Error::InvalidText`] for a bad field, [`Error::Corrupt`] for
    /// an unreadable goal, or [`Error::Store`] on filesystem failure.
    pub fn update_goal(
        &self,
        principal: &MemoryPrincipal,
        id: &GoalId,
        edit: &GoalEdit<'_>,
        now_secs: u64,
    ) -> Result<GoalChange, Error> {
        if edit.title.is_none() && edit.detail.is_none() {
            return Err(Error::EmptyEdit);
        }
        if let Some(title) = edit.title {
            record::check_title(title)?;
        }
        if let Some(detail) = edit.detail {
            record::check_detail(detail)?;
        }
        let Some(mut goal) = read::goal(self.backing.as_ref(), principal.peer_uid, id)? else {
            return Ok(GoalChange::NotFound);
        };
        if !goal.is_open() {
            return Ok(GoalChange::AlreadyClosed);
        }
        if let Some(title) = edit.title {
            title.clone_into(&mut goal.title);
        }
        if let Some(detail) = edit.detail {
            detail.clone_into(&mut goal.detail);
        }
        goal.updated_at_secs = now_secs;
        self.store_goal(principal, &goal)?;
        tracing::info!(goal_id = %id, "updated goal");
        Ok(GoalChange::Changed(goal))
    }

    /// Closes one of the principal's open goals with the outcome the
    /// user named. Closing is final: a closed goal is kept but can no
    /// longer change.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] for an unreadable goal, or
    /// [`Error::Store`] on filesystem failure.
    pub fn close_goal(
        &self,
        principal: &MemoryPrincipal,
        id: &GoalId,
        outcome: GoalOutcome,
        now_secs: u64,
    ) -> Result<GoalChange, Error> {
        let Some(mut goal) = read::goal(self.backing.as_ref(), principal.peer_uid, id)? else {
            return Ok(GoalChange::NotFound);
        };
        if !goal.is_open() {
            return Ok(GoalChange::AlreadyClosed);
        }
        goal.status = GoalStatus::Closed {
            outcome,
            closed_at_secs: now_secs,
        };
        goal.updated_at_secs = now_secs;
        self.store_goal(principal, &goal)?;
        tracing::info!(goal_id = %id, "closed goal");
        Ok(GoalChange::Changed(goal))
    }

    /// Appends one entry to the principal's tracking log, returning it
    /// with its broker-assigned sequence number and timestamp.
    ///
    /// The backing holds the log's lock from counting to keeping, so two
    /// sessions of one user never share a sequence number, and nothing
    /// already in the log can be altered.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidText`] for bad text,
    /// [`Error::TrackingFull`] when the log is at its bound, or
    /// [`Error::Store`] / [`Error::Io`] on filesystem failure.
    pub fn append_tracking(
        &self,
        principal: &MemoryPrincipal,
        source: &SourceLabel,
        text: &str,
        now_secs: u64,
    ) -> Result<TrackingEntry, Error> {
        record::check_tracking_text(text)?;
        let max = self.limits.tracking_entries;
        let mut made = None;
        self.backing
            .append_tracking(principal.peer_uid, &mut |seq| {
                if seq >= max {
                    return Err(Error::TrackingFull { max });
                }
                let entry = TrackingEntry {
                    seq,
                    at_secs: now_secs,
                    source: source.clone(),
                    text: text.to_owned(),
                };
                let line = serde_json::to_vec(&entry).map_err(|_| Error::Corrupt)?;
                made = Some(entry);
                Ok(line)
            })?;
        let entry = made.ok_or(Error::Corrupt)?;
        tracing::info!(seq = entry.seq, source = %source, "appended tracking entry");
        Ok(entry)
    }

    /// Keeps one goal of the principal's, whole or not at all.
    fn store_goal(&self, principal: &MemoryPrincipal, goal: &Goal) -> Result<(), Error> {
        let bytes = serde_json::to_vec(goal).map_err(|_| Error::Corrupt)?;
        self.backing.put_goal(principal.peer_uid, &goal.id, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::PathBuf;
    use std::str::FromStr as _;

    use hatchling_memory::SessionId;

    use super::*;
    use crate::read::{GOALS_DIR, TRACKING_DIR, TRACKING_LOG};

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-goals-store-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn principal(uid: u32) -> MemoryPrincipal {
        MemoryPrincipal {
            peer_uid: uid,
            session_id: SessionId::from_str("sess-goals").unwrap(),
        }
    }

    fn me() -> u32 {
        rustix::process::getuid().as_raw()
    }

    fn store_with(root: &Path, limits: Limits) -> GoalsStore {
        GoalsStore::open_as(root, me(), limits).unwrap()
    }

    fn goals_of(store: &GoalsStore, uid: u32) -> Vec<Goal> {
        read::goals(store.backing.as_ref(), uid).unwrap()
    }

    fn log_of(store: &GoalsStore, uid: u32) -> Vec<TrackingEntry> {
        read::tracking(store.backing.as_ref(), uid, |_| true).unwrap()
    }

    fn label(raw: &str) -> SourceLabel {
        raw.parse().unwrap()
    }

    #[test]
    fn create_records_an_open_user_explicit_goal() {
        let root = test_root("create");
        let store = GoalsStore::open(&root).unwrap();
        let goal = store
            .create_goal(&principal(7), "run a 5k", "by spring", 100)
            .unwrap();
        assert_eq!(goal.title, "run a 5k");
        assert_eq!(goal.detail, "by spring");
        assert_eq!(goal.status, GoalStatus::Open);
        assert_eq!(goal.origin, GoalOrigin::UserExplicit);
        assert_eq!((goal.created_at_secs, goal.updated_at_secs), (100, 100));
        assert_eq!(goals_of(&store, 7), vec![goal.clone()]);
        // The file is owner-only, under the peer's uid.
        let path = read::goal_path(&root.join(GOALS_DIR).join("7"), &goal.id);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_mints_a_fresh_id_every_time() {
        let root = test_root("fresh");
        let store = GoalsStore::open(&root).unwrap();
        let a = store.create_goal(&principal(7), "same", "", 1).unwrap();
        let b = store.create_goal(&principal(7), "same", "", 1).unwrap();
        assert_ne!(a.id, b.id, "a create must never overwrite a goal");
        assert_eq!(goals_of(&store, 7).len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_validates_before_touching_the_tree() {
        let root = test_root("validate");
        let store = GoalsStore::open(&root).unwrap();
        assert!(matches!(
            store.create_goal(&principal(7), "", "", 1),
            Err(Error::InvalidText { field: "title", .. })
        ));
        assert!(matches!(
            store.create_goal(&principal(7), "ok", "bell\u{7}", 1),
            Err(Error::InvalidText {
                field: "detail",
                ..
            })
        ));
        assert!(!root.join(GOALS_DIR).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_goal_limit_is_exact_and_per_user() {
        let root = test_root("goal-limit");
        let store = store_with(
            &root,
            Limits {
                goals: 3,
                tracking_entries: 10,
            },
        );
        for n in 0..3 {
            store
                .create_goal(&principal(7), &format!("g{n}"), "", 1)
                .unwrap();
        }
        assert!(matches!(
            store.create_goal(&principal(7), "one too many", "", 1),
            Err(Error::GoalLimit { max: 3 })
        ));
        assert_eq!(goals_of(&store, 7).len(), 3);
        // Another user's budget is untouched.
        store.create_goal(&principal(8), "mine", "", 1).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn update_changes_only_the_named_fields_of_an_open_goal() {
        let root = test_root("update");
        let store = GoalsStore::open(&root).unwrap();
        let p = principal(7);
        let goal = store.create_goal(&p, "run a 5k", "by spring", 100).unwrap();
        let edit = GoalEdit {
            title: Some("run a 10k"),
            detail: None,
        };
        let GoalChange::Changed(after) = store.update_goal(&p, &goal.id, &edit, 150).unwrap()
        else {
            unreachable!("the goal exists and is open");
        };
        assert_eq!(after.title, "run a 10k");
        assert_eq!(after.detail, "by spring");
        assert_eq!((after.created_at_secs, after.updated_at_secs), (100, 150));
        let edit = GoalEdit {
            title: None,
            detail: Some("by summer"),
        };
        let GoalChange::Changed(after) = store.update_goal(&p, &goal.id, &edit, 160).unwrap()
        else {
            unreachable!("the goal exists and is open");
        };
        assert_eq!(
            (after.title.as_str(), after.detail.as_str()),
            ("run a 10k", "by summer")
        );
        assert_eq!(goals_of(&store, 7), vec![after]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn update_refuses_an_empty_or_invalid_edit() {
        let root = test_root("update-invalid");
        let store = GoalsStore::open(&root).unwrap();
        let p = principal(7);
        let goal = store.create_goal(&p, "run", "", 1).unwrap();
        assert!(matches!(
            store.update_goal(&p, &goal.id, &GoalEdit::default(), 2),
            Err(Error::EmptyEdit)
        ));
        let bad_title = GoalEdit {
            title: Some("two\nlines"),
            detail: None,
        };
        assert!(matches!(
            store.update_goal(&p, &goal.id, &bad_title, 2),
            Err(Error::InvalidText { field: "title", .. })
        ));
        let long_detail = "d".repeat(record::MAX_DETAIL_BYTES + 1);
        let bad_detail = GoalEdit {
            title: None,
            detail: Some(&long_detail),
        };
        assert!(matches!(
            store.update_goal(&p, &goal.id, &bad_detail, 2),
            Err(Error::InvalidText {
                field: "detail",
                ..
            })
        ));
        assert_eq!(goals_of(&store, 7), vec![goal]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn close_is_final_and_records_the_outcome() {
        let root = test_root("close");
        let store = GoalsStore::open(&root).unwrap();
        let p = principal(7);
        let goal = store.create_goal(&p, "run", "", 1).unwrap();
        let GoalChange::Changed(closed) = store
            .close_goal(&p, &goal.id, GoalOutcome::Achieved, 50)
            .unwrap()
        else {
            unreachable!("the goal exists and is open");
        };
        assert_eq!(
            closed.status,
            GoalStatus::Closed {
                outcome: GoalOutcome::Achieved,
                closed_at_secs: 50
            }
        );
        assert_eq!(closed.updated_at_secs, 50);
        // Closed means closed: no second close, no edit.
        assert_eq!(
            store
                .close_goal(&p, &goal.id, GoalOutcome::Abandoned, 60)
                .unwrap(),
            GoalChange::AlreadyClosed
        );
        let edit = GoalEdit {
            title: Some("again"),
            detail: None,
        };
        assert_eq!(
            store.update_goal(&p, &goal.id, &edit, 60).unwrap(),
            GoalChange::AlreadyClosed
        );
        assert_eq!(goals_of(&store, 7), vec![closed]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn goals_are_bound_to_the_peer_uid() {
        // User 8 cannot change user 7's goal by naming its id: the id
        // is looked up in user 8's own directory, where it is absent.
        let root = test_root("isolation");
        let store = GoalsStore::open(&root).unwrap();
        let goal = store.create_goal(&principal(7), "mine", "", 1).unwrap();
        let edit = GoalEdit {
            title: Some("stolen"),
            detail: None,
        };
        assert_eq!(
            store
                .update_goal(&principal(8), &goal.id, &edit, 2)
                .unwrap(),
            GoalChange::NotFound
        );
        assert_eq!(
            store
                .close_goal(&principal(8), &goal.id, GoalOutcome::Abandoned, 2)
                .unwrap(),
            GoalChange::NotFound
        );
        assert_eq!(goals_of(&store, 7), vec![goal]);
        assert!(goals_of(&store, 8).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn update_and_close_report_a_corrupt_goal() {
        let root = test_root("corrupt");
        let store = GoalsStore::open(&root).unwrap();
        let p = principal(7);
        let goal = store.create_goal(&p, "run", "", 1).unwrap();
        std::fs::write(
            read::goal_path(&root.join(GOALS_DIR).join("7"), &goal.id),
            b"{",
        )
        .unwrap();
        let edit = GoalEdit {
            title: Some("x"),
            detail: None,
        };
        assert!(matches!(
            store.update_goal(&p, &goal.id, &edit, 2),
            Err(Error::Corrupt)
        ));
        assert!(matches!(
            store.close_goal(&p, &goal.id, GoalOutcome::Achieved, 2),
            Err(Error::Corrupt)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tracking_appends_are_sequenced_stamped_and_immutable() {
        let root = test_root("append");
        let store = GoalsStore::open(&root).unwrap();
        let p = principal(7);
        let first = store
            .append_tracking(&p, &label("user"), "ran 3k", 10)
            .unwrap();
        let second = store
            .append_tracking(&p, &label("whatsapp"), "friend: nice run!", 11)
            .unwrap();
        assert_eq!((first.seq, first.at_secs), (0, 10));
        assert_eq!((second.seq, second.at_secs), (1, 11));
        assert_eq!(second.source, label("whatsapp"));
        // Appending never disturbs what is already there.
        assert_eq!(log_of(&store, 7), vec![first, second]);
        let log = root.join(TRACKING_DIR).join("7").join(TRACKING_LOG);
        assert_eq!(std::fs::metadata(&log).unwrap().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tracking_text_is_validated_before_the_log_is_touched() {
        let root = test_root("append-invalid");
        let store = GoalsStore::open(&root).unwrap();
        assert!(matches!(
            store.append_tracking(&principal(7), &label("user"), "", 1),
            Err(Error::InvalidText { .. })
        ));
        assert!(!root.join(TRACKING_DIR).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tracking_bound_is_exact_and_per_user() {
        let root = test_root("tracking-limit");
        let store = store_with(
            &root,
            Limits {
                goals: 3,
                tracking_entries: 2,
            },
        );
        let p = principal(7);
        store.append_tracking(&p, &label("user"), "a", 1).unwrap();
        store.append_tracking(&p, &label("user"), "b", 2).unwrap();
        assert!(matches!(
            store.append_tracking(&p, &label("user"), "c", 3),
            Err(Error::TrackingFull { max: 2 })
        ));
        assert_eq!(log_of(&store, 7).len(), 2);
        store
            .append_tracking(&principal(8), &label("user"), "a", 1)
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tracking_logs_are_bound_to_the_peer_uid() {
        let root = test_root("tracking-isolation");
        let store = GoalsStore::open(&root).unwrap();
        store
            .append_tracking(&principal(7), &label("user"), "mine", 1)
            .unwrap();
        let other = store
            .append_tracking(&principal(8), &label("user"), "theirs", 2)
            .unwrap();
        // Each user's log starts at 0: they are separate logs.
        assert_eq!(other.seq, 0);
        assert_eq!(log_of(&store, 7).len(), 1);
        assert_eq!(log_of(&store, 8), vec![other]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_appends_never_share_a_sequence_number() {
        let root = test_root("concurrent");
        let store = GoalsStore::open(&root).unwrap();
        std::thread::scope(|scope| {
            for t in 0..8 {
                let store = &store;
                scope.spawn(move || {
                    for n in 0..25 {
                        store
                            .append_tracking(&principal(7), &label("user"), &format!("{t}-{n}"), 1)
                            .unwrap();
                    }
                });
            }
        });
        // `scan_tracking` refuses any gap or repeat, so a clean read of
        // 200 entries proves the sequence is exactly 0..200.
        assert_eq!(log_of(&store, 7).len(), 200);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn count_lines_counts_every_newline_across_buffer_refills() {
        let root = test_root("count");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("lines");
        // 20000 bytes: more than two 8 KiB refills, with a newline
        // every 100 bytes.
        let mut bytes = Vec::new();
        for _ in 0..200 {
            bytes.extend_from_slice(&[b'x'; 99]);
            bytes.push(b'\n');
        }
        std::fs::write(&path, &bytes).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        assert_eq!(Files::count_lines(&file).unwrap(), 200);
        std::fs::write(&path, b"").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        assert_eq!(Files::count_lines(&file).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_repairs_a_loose_root_but_refuses_a_foreign_owner() {
        let root = test_root("open");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        GoalsStore::open(&root).unwrap();
        assert_eq!(std::fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
        assert!(matches!(
            GoalsStore::open_as(&root, u32::MAX, Limits::SHIPPED),
            Err(Error::Store {
                source: hatchling_memory::Error::InsecurePermissions { .. }
            })
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_shipped_limits_are_the_documented_constants() {
        assert_eq!(Limits::SHIPPED.goals, 256);
        assert_eq!(Limits::SHIPPED.tracking_entries, 10_000);
    }
}
