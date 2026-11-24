//! Broker-side goals, tracking, and query operations: the session
//! handlers for [`GoalsOp`] and [`QueryOp`].
//!
//! The same shape as [`memory_ops`](crate::memory_ops): every op is
//! authorized against the configured grants before any store is
//! opened, every name is parsed before the filesystem is touched, and
//! every op is bound to the [`MemoryPrincipal`] the broker derived from
//! kernel peer credentials — the agent names goals, never users.
//!
//! Writes and reads are split by type, not by care. [`handle_goals_op`]
//! is the only caller of a [`GoalsStore`]. [`handle_query_op`] builds a
//! [`FeedReader`] and nothing else: the query path holds no handle that
//! can write, so the feed's "reads, never acts" rule holds here as well
//! as in the protocol, where no goals op can be spelled as a query.
//!
//! Backend failures become the opaque `GoalsError` / `QueryError`
//! replies and the session survives them, as with the vault. A
//! malformed id, label, tag, status, outcome, range, or cursor is a
//! protocol violation and ends the session: unknown traffic is never
//! acted on.

use std::time::{SystemTime, UNIX_EPOCH};

use hatchling_core::{GoalsOp, GoalsResult, QueryOp, QueryResult};
use hatchling_goals::{
    FeedItem, FeedQuery, FeedReader, GoalChange, GoalEdit, GoalId, GoalOutcome, GoalsStore,
    PAGE_BUDGET_BYTES, SourceLabel, StatusFilter, TimeRange, page,
};
use hatchling_memory::{MemoryId, MemoryPrincipal, Tag};

use crate::Error;
use crate::config::{GoalsAction, GoalsConfig, MemoryAction, MemoryConfig};

/// Handles one goals op: authorize, parse, bind, execute.
///
/// # Errors
///
/// Returns [`Error::SessionProtocol`] for a malformed goal id, source
/// label, or outcome. Grant denials, refused content, reached limits,
/// and backend failures are not errors: they are replies.
pub fn handle_goals_op(
    config: &GoalsConfig,
    op: &GoalsOp,
    principal: &MemoryPrincipal,
) -> Result<GoalsResult, Error> {
    if !config.grants.contains(&goals_action(op)) {
        return Ok(GoalsResult::Denied);
    }
    let now = now_secs();
    let reply = match op {
        GoalsOp::GoalCreate { title, detail } => with_store(config, |store| {
            store
                .create_goal(principal, title, detail, now)
                .map(|goal| GoalsResult::GoalCreated {
                    id: goal.id.to_string(),
                })
        }),
        GoalsOp::GoalUpdate { id, title, detail } => {
            let id = parse_goal_id(id)?;
            let edit = GoalEdit {
                title: title.as_deref(),
                detail: detail.as_deref(),
            };
            with_store(config, |store| {
                store
                    .update_goal(principal, &id, &edit, now)
                    .map(|change| change_reply(&change))
            })
        }
        GoalsOp::GoalClose { id, outcome } => {
            let id = parse_goal_id(id)?;
            let outcome: GoalOutcome = outcome.parse().map_err(|error| protocol(&error))?;
            with_store(config, |store| {
                store
                    .close_goal(principal, &id, outcome, now)
                    .map(|change| change_reply(&change))
            })
        }
        GoalsOp::TrackingAppend { source, text } => {
            let source: SourceLabel = source.parse().map_err(|error| protocol(&error))?;
            with_store(config, |store| {
                store
                    .append_tracking(principal, &source, text, now)
                    .map(|entry| GoalsResult::TrackingAppended { seq: entry.seq })
            })
        }
    };
    Ok(reply)
}

/// Handles one query: authorize, parse, then read through a
/// [`FeedReader`] and page the result into one frame.
///
/// Goal and tracking queries need their own grants. A memory query is
/// gated by the memory grants instead (`user:recall`): the query path
/// is a second door to memory, so it must not need less than the first.
///
/// # Errors
///
/// Returns [`Error::SessionProtocol`] for a malformed status, range,
/// source, tag, or cursor. Denials and backend failures are replies.
pub fn handle_query_op(
    goals: &GoalsConfig,
    memory: &MemoryConfig,
    op: &QueryOp,
    principal: &MemoryPrincipal,
) -> Result<QueryResult, Error> {
    let query = match op {
        QueryOp::Goals {
            status,
            updated_since_secs,
            after,
        } => {
            if !goals.grants.contains(&GoalsAction::QueryGoals) {
                return Ok(QueryResult::Denied);
            }
            FeedQuery::Goals {
                status: status
                    .parse::<StatusFilter>()
                    .map_err(|error| protocol(&error))?,
                updated_since_secs: *updated_since_secs,
                after: after.as_deref().map(parse_goal_id).transpose()?,
            }
        }
        QueryOp::Tracking {
            since_secs,
            until_secs,
            source,
            after,
        } => {
            if !goals.grants.contains(&GoalsAction::QueryTracking) {
                return Ok(QueryResult::Denied);
            }
            FeedQuery::Tracking {
                range: TimeRange::new(*since_secs, *until_secs)
                    .map_err(|error| protocol(&error))?,
                source: source
                    .as_deref()
                    .map(str::parse::<SourceLabel>)
                    .transpose()
                    .map_err(|error| protocol(&error))?,
                after_seq: after.as_deref().map(parse_seq).transpose()?,
            }
        }
        QueryOp::Memory { tag, after } => {
            let granted = memory
                .grants
                .iter()
                .any(|g| g.scope == "user" && g.action == MemoryAction::Recall);
            if !granted {
                return Ok(QueryResult::Denied);
            }
            FeedQuery::Memory {
                tag: tag.parse::<Tag>().map_err(|e| Error::SessionProtocol {
                    reason: format!("invalid memory tag: {e}"),
                })?,
                after: after
                    .as_deref()
                    .map(str::parse::<MemoryId>)
                    .transpose()
                    .map_err(|e| Error::SessionProtocol {
                        reason: format!("invalid memory cursor: {e}"),
                    })?,
            }
        }
    };
    let memory_store = if matches!(query, FeedQuery::Memory { .. }) {
        match crate::memory_sealed::open_published(&memory.root) {
            Ok(store) => Some(store),
            Err(source) => {
                tracing::error!(%source, "memory store open failed for a query");
                return Ok(QueryResult::QueryError);
            }
        }
    } else {
        None
    };
    let reader = FeedReader::open(&goals.root, memory_store);
    let result = reader
        .query(principal, &query, now_secs())
        .and_then(|items| page(&items, PAGE_BUDGET_BYTES));
    match result {
        Ok(page) => Ok(QueryResult::Items {
            items_json: page.items_json,
            next: page.next,
        }),
        Err(source) => {
            tracing::error!(%source, "query failed");
            Ok(QueryResult::QueryError)
        }
    }
}

/// Whether the configured grants let a session read `query`: the same
/// grants [`handle_query_op`] checks.
pub(crate) fn may_read(goals: &GoalsConfig, memory: &MemoryConfig, query: &FeedQuery) -> bool {
    match query {
        FeedQuery::Goals { .. } => goals.grants.contains(&GoalsAction::QueryGoals),
        FeedQuery::Tracking { .. } => goals.grants.contains(&GoalsAction::QueryTracking),
        FeedQuery::Memory { .. } => memory
            .grants
            .iter()
            .any(|g| g.scope == "user" && g.action == MemoryAction::Recall),
    }
}

/// Every item `query` finds for `principal`, through the read-only path,
/// or `None` if a store cannot be read. No grant check: the caller makes
/// it ([`may_read`], or a run's own grants).
pub(crate) fn read_feed(
    goals: &GoalsConfig,
    memory: &MemoryConfig,
    principal: &MemoryPrincipal,
    query: &FeedQuery,
) -> Option<Vec<FeedItem>> {
    let memory_store = if matches!(query, FeedQuery::Memory { .. }) {
        match crate::memory_sealed::open_published(&memory.root) {
            Ok(store) => Some(store),
            Err(source) => {
                tracing::error!(%source, "memory store open failed for a feed read");
                return None;
            }
        }
    } else {
        None
    };
    FeedReader::open(&goals.root, memory_store)
        .query(principal, query, now_secs())
        .map_err(|source| tracing::error!(%source, "feed read failed"))
        .ok()
}

/// The action a [`GoalsOp`] performs: the unit of grant matching.
const fn goals_action(op: &GoalsOp) -> GoalsAction {
    match op {
        GoalsOp::GoalCreate { .. } => GoalsAction::GoalCreate,
        GoalsOp::GoalUpdate { .. } => GoalsAction::GoalUpdate,
        GoalsOp::GoalClose { .. } => GoalsAction::GoalClose,
        GoalsOp::TrackingAppend { .. } => GoalsAction::TrackingAppend,
    }
}

/// Opens the store and runs `op` on it; any failure becomes a reply.
/// The store is opened per op, so the root's mode and owner are
/// re-validated every time.
fn with_store(
    config: &GoalsConfig,
    op: impl FnOnce(&GoalsStore) -> Result<GoalsResult, hatchling_goals::Error>,
) -> GoalsResult {
    match GoalsStore::open(&config.root) {
        Ok(store) => op(&store).unwrap_or_else(|error| failure_reply(&error)),
        Err(error) => failure_reply(&error),
    }
}

/// The reply for an update or close.
const fn change_reply(change: &GoalChange) -> GoalsResult {
    match change {
        GoalChange::Changed(_) => GoalsResult::GoalChanged,
        GoalChange::NotFound => GoalsResult::GoalNotFound,
        GoalChange::AlreadyClosed => GoalsResult::GoalAlreadyClosed,
    }
}

/// The reply for a failed goals op. Refused content and reached limits
/// are the agent's to know; everything else is logged and opaque.
fn failure_reply(error: &hatchling_goals::Error) -> GoalsResult {
    use hatchling_goals::Error as E;
    match error {
        E::InvalidText { .. } | E::EmptyEdit | E::InvalidName(_) => GoalsResult::Rejected,
        E::GoalLimit { .. } | E::TrackingFull { .. } => GoalsResult::LimitReached,
        E::Store { .. }
        | E::Io { .. }
        | E::InvalidRange { .. }
        | E::MemoryUnavailable
        | E::ProvenanceMismatch
        | E::Corrupt => {
            tracing::error!(source = %error, "goals op failed");
            GoalsResult::GoalsError
        }
    }
}

fn parse_goal_id(raw: &str) -> Result<GoalId, Error> {
    raw.parse::<GoalId>().map_err(|error| protocol(&error))
}

fn parse_seq(raw: &str) -> Result<u64, Error> {
    raw.parse::<u64>().map_err(|_| Error::SessionProtocol {
        reason: format!("invalid tracking cursor {raw:?}: expected a sequence number"),
    })
}

/// A malformed request field ends the session.
fn protocol(error: &hatchling_goals::Error) -> Error {
    Error::SessionProtocol {
        reason: error.to_string(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::str::FromStr as _;

    use hatchling_core::{MAX_FRAME_BYTES, SessionResponse};
    use hatchling_goals::{FeedBody, FeedItem, Goal, GoalStatus, SourceStore};
    use hatchling_memory::{MemoryScope, RetentionPolicy, SessionId};

    use super::*;
    use crate::config::MemoryGrant;

    const ALL_GOALS: [GoalsAction; 6] = [
        GoalsAction::GoalCreate,
        GoalsAction::GoalUpdate,
        GoalsAction::GoalClose,
        GoalsAction::TrackingAppend,
        GoalsAction::QueryGoals,
        GoalsAction::QueryTracking,
    ];

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-broker-goals-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn principal(uid: u32) -> MemoryPrincipal {
        MemoryPrincipal {
            peer_uid: uid,
            session_id: SessionId::from_str("sess-broker-goals").unwrap(),
        }
    }

    fn goals_config(root: &Path, grants: &[GoalsAction]) -> GoalsConfig {
        GoalsConfig {
            root: root.to_path_buf(),
            grants: grants.to_vec(),
        }
    }

    fn no_memory() -> MemoryConfig {
        MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        }
    }

    fn create(config: &GoalsConfig, p: &MemoryPrincipal, title: &str) -> String {
        let op = GoalsOp::GoalCreate {
            title: title.to_owned(),
            detail: String::new(),
        };
        let reply = handle_goals_op(config, &op, p).unwrap();
        let GoalsResult::GoalCreated { id } = reply else {
            unreachable!("create failed: {reply:?}");
        };
        id
    }

    fn append(config: &GoalsConfig, p: &MemoryPrincipal, source: &str, text: &str) -> GoalsResult {
        let op = GoalsOp::TrackingAppend {
            source: source.to_owned(),
            text: text.to_owned(),
        };
        handle_goals_op(config, &op, p).unwrap()
    }

    fn items(result: QueryResult) -> (Vec<FeedItem>, Option<String>) {
        let QueryResult::Items { items_json, next } = result else {
            unreachable!("query failed: {result:?}");
        };
        (
            items_json
                .iter()
                .map(|json| FeedItem::from_json(json).unwrap())
                .collect(),
            next,
        )
    }

    fn list(config: &GoalsConfig, p: &MemoryPrincipal, status: &str) -> Vec<Goal> {
        let op = QueryOp::Goals {
            status: status.to_owned(),
            updated_since_secs: None,
            after: None,
        };
        items(handle_query_op(config, &no_memory(), &op, p).unwrap())
            .0
            .into_iter()
            .map(|item| match item.body() {
                FeedBody::Goal(goal) => goal.clone(),
                FeedBody::Tracking(_) | FeedBody::Memory(_) => unreachable!("not a goal"),
            })
            .collect()
    }

    fn tracking(
        config: &GoalsConfig,
        p: &MemoryPrincipal,
        since_secs: Option<u64>,
        until_secs: Option<u64>,
        source: Option<&str>,
    ) -> Vec<String> {
        let op = QueryOp::Tracking {
            since_secs,
            until_secs,
            source: source.map(str::to_owned),
            after: None,
        };
        items(handle_query_op(config, &no_memory(), &op, p).unwrap())
            .0
            .into_iter()
            .map(|item| match item.body() {
                FeedBody::Tracking(entry) => entry.text.clone(),
                FeedBody::Goal(_) | FeedBody::Memory(_) => unreachable!("not an entry"),
            })
            .collect()
    }

    #[test]
    fn goal_lifecycle_through_the_broker_ops() {
        // Acceptance: create → list → update → close, each through the
        // broker ops.
        let root = test_root("lifecycle");
        let config = goals_config(&root, &ALL_GOALS);
        let p = principal(7);
        let id = create(&config, &p, "run a 5k");
        let listed = list(&config, &p, "open");
        assert_eq!(listed.len(), 1);
        let goal = listed.first().unwrap();
        assert_eq!(
            (goal.id.as_str(), goal.title.as_str()),
            (id.as_str(), "run a 5k")
        );

        let update = GoalsOp::GoalUpdate {
            id: id.clone(),
            title: Some(String::from("run a 10k")),
            detail: Some(String::from("by summer")),
        };
        assert_eq!(
            handle_goals_op(&config, &update, &p).unwrap(),
            GoalsResult::GoalChanged
        );
        let listed = list(&config, &p, "any");
        let goal = listed.first().unwrap();
        assert_eq!(
            (goal.title.as_str(), goal.detail.as_str()),
            ("run a 10k", "by summer")
        );

        let close = GoalsOp::GoalClose {
            id,
            outcome: String::from("achieved"),
        };
        assert_eq!(
            handle_goals_op(&config, &close, &p).unwrap(),
            GoalsResult::GoalChanged
        );
        assert!(list(&config, &p, "open").is_empty());
        let closed = list(&config, &p, "closed");
        assert!(matches!(
            closed.first().map(|g| g.status),
            Some(GoalStatus::Closed {
                outcome: GoalOutcome::Achieved,
                ..
            })
        ));
        // Closed is final.
        assert_eq!(
            handle_goals_op(&config, &close, &p).unwrap(),
            GoalsResult::GoalAlreadyClosed
        );
        assert_eq!(
            handle_goals_op(&config, &update, &p).unwrap(),
            GoalsResult::GoalAlreadyClosed
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn one_principal_cannot_read_or_change_anothers_goals() {
        // Acceptance (negative): goals are user-scoped. User 8 sees none
        // of user 7's goals and cannot change one by naming its id.
        let root = test_root("isolation");
        let config = goals_config(&root, &ALL_GOALS);
        let (owner, other) = (principal(7), principal(8));
        let id = create(&config, &owner, "private goal");
        append(&config, &owner, "user", "private entry");
        assert!(list(&config, &other, "any").is_empty());
        assert!(tracking(&config, &other, None, None, None).is_empty());
        let update = GoalsOp::GoalUpdate {
            id: id.clone(),
            title: Some(String::from("hijacked")),
            detail: None,
        };
        assert_eq!(
            handle_goals_op(&config, &update, &other).unwrap(),
            GoalsResult::GoalNotFound
        );
        let close = GoalsOp::GoalClose {
            id,
            outcome: String::from("abandoned"),
        };
        assert_eq!(
            handle_goals_op(&config, &close, &other).unwrap(),
            GoalsResult::GoalNotFound
        );
        let mine = list(&config, &owner, "open");
        assert_eq!(mine.first().map(|g| g.title.as_str()), Some("private goal"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tracking_range_and_source_queries_through_the_broker_ops() {
        // Acceptance: append entries, then range (since/until) and
        // source-filtered queries.
        let root = test_root("tracking");
        let config = goals_config(&root, &ALL_GOALS);
        let p = principal(7);
        let seqs: Vec<GoalsResult> = [
            ("user", "ran 3k"),
            ("whatsapp", "nice run"),
            ("user", "ran 5k"),
        ]
        .iter()
        .map(|(source, text)| append(&config, &p, source, text))
        .collect();
        assert_eq!(
            seqs,
            vec![
                GoalsResult::TrackingAppended { seq: 0 },
                GoalsResult::TrackingAppended { seq: 1 },
                GoalsResult::TrackingAppended { seq: 2 },
            ]
        );
        assert_eq!(
            tracking(&config, &p, None, None, None),
            vec!["ran 3k", "nice run", "ran 5k"]
        );
        assert_eq!(
            tracking(&config, &p, None, None, Some("user")),
            vec!["ran 3k", "ran 5k"]
        );
        assert_eq!(
            tracking(&config, &p, None, None, Some("whatsapp")),
            vec!["nice run"]
        );
        // The broker stamps every entry with the host clock: a window
        // around now holds all three, one ending in the past holds none.
        let now = now_secs();
        assert_eq!(
            tracking(&config, &p, Some(now - 60), Some(now + 60), None).len(),
            3
        );
        assert!(tracking(&config, &p, None, Some(now - 60), None).is_empty());
        assert!(tracking(&config, &p, Some(now + 60), None, None).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_op_is_denied_without_its_grant_and_touches_nothing() {
        let root = test_root("denied");
        for missing in ALL_GOALS {
            let granted: Vec<GoalsAction> =
                ALL_GOALS.into_iter().filter(|a| *a != missing).collect();
            let config = goals_config(&root, &granted);
            let p = principal(7);
            let goals_ops = [
                (
                    GoalsAction::GoalCreate,
                    GoalsOp::GoalCreate {
                        title: String::from("t"),
                        detail: String::new(),
                    },
                ),
                (
                    GoalsAction::GoalUpdate,
                    GoalsOp::GoalUpdate {
                        id: String::from("g1"),
                        title: Some(String::from("t")),
                        detail: None,
                    },
                ),
                (
                    GoalsAction::GoalClose,
                    GoalsOp::GoalClose {
                        id: String::from("g1"),
                        outcome: String::from("achieved"),
                    },
                ),
                (
                    GoalsAction::TrackingAppend,
                    GoalsOp::TrackingAppend {
                        source: String::from("user"),
                        text: String::from("t"),
                    },
                ),
            ];
            for (action, op) in &goals_ops {
                if *action == missing {
                    assert_eq!(
                        handle_goals_op(&config, op, &p).unwrap(),
                        GoalsResult::Denied
                    );
                }
            }
            let queries = [
                (
                    GoalsAction::QueryGoals,
                    QueryOp::Goals {
                        status: String::from("any"),
                        updated_since_secs: None,
                        after: None,
                    },
                ),
                (
                    GoalsAction::QueryTracking,
                    QueryOp::Tracking {
                        since_secs: None,
                        until_secs: None,
                        source: None,
                        after: None,
                    },
                ),
            ];
            for (action, op) in &queries {
                if *action == missing {
                    assert_eq!(
                        handle_query_op(&config, &no_memory(), op, &p).unwrap(),
                        QueryResult::Denied
                    );
                }
            }
            assert!(
                !root.exists(),
                "a denied op must not touch the filesystem ({missing:?})"
            );
        }
    }

    #[test]
    fn a_memory_query_needs_the_memory_recall_grant() {
        let goals_root = test_root("memory-goals");
        let memory_root = test_root("memory-store");
        let p = principal(7);
        crate::memory_sealed::open_published(&memory_root)
            .unwrap()
            .write(
                &p,
                &MemoryScope::User,
                &"pref-1".parse().unwrap(),
                "prefers mornings",
                &["feed".parse().unwrap()],
                RetentionPolicy::Keep,
                1,
            )
            .unwrap();
        let config = goals_config(&goals_root, &ALL_GOALS);
        let op = QueryOp::Memory {
            tag: String::from("feed"),
            after: None,
        };
        let grant = |scope: &str, action| MemoryGrant {
            scope: scope.to_owned(),
            action,
        };
        // Every goals grant, and memory grants that are not user recall:
        // denied.
        for grants in [
            vec![],
            vec![grant("user", MemoryAction::Write)],
            vec![grant("project", MemoryAction::Recall)],
            vec![grant("session", MemoryAction::Recall)],
        ] {
            let memory = MemoryConfig {
                root: memory_root.clone(),
                grants,
            };
            assert_eq!(
                handle_query_op(&config, &memory, &op, &p).unwrap(),
                QueryResult::Denied
            );
        }
        // With user recall: the item comes back labeled as memory, even
        // with no goals grant at all.
        let memory = MemoryConfig {
            root: memory_root.clone(),
            grants: vec![grant("user", MemoryAction::Recall)],
        };
        let (found, next) =
            items(handle_query_op(&goals_config(&goals_root, &[]), &memory, &op, &p).unwrap());
        assert_eq!(next, None);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found.first().map(|i| i.provenance().store),
            Some(SourceStore::Memory)
        );
        let _ = std::fs::remove_dir_all(&goals_root);
        let _ = std::fs::remove_dir_all(&memory_root);
    }

    #[test]
    fn malformed_fields_end_the_session() {
        let root = test_root("protocol");
        let config = goals_config(&root, &ALL_GOALS);
        let memory = MemoryConfig {
            root: root.join("memory"),
            grants: vec![MemoryGrant {
                scope: String::from("user"),
                action: MemoryAction::Recall,
            }],
        };
        let p = principal(7);
        let bad_ops = [
            GoalsOp::GoalUpdate {
                id: String::from("../etc"),
                title: Some(String::from("t")),
                detail: None,
            },
            GoalsOp::GoalClose {
                id: String::from("g1"),
                outcome: String::from("done"),
            },
            GoalsOp::GoalClose {
                id: String::from("g/1"),
                outcome: String::from("achieved"),
            },
            GoalsOp::TrackingAppend {
                source: String::from("what sapp"),
                text: String::from("t"),
            },
        ];
        for op in &bad_ops {
            assert!(
                matches!(
                    handle_goals_op(&config, op, &p),
                    Err(Error::SessionProtocol { .. })
                ),
                "{op:?}"
            );
        }
        let bad_queries = [
            QueryOp::Goals {
                status: String::from("all"),
                updated_since_secs: None,
                after: None,
            },
            QueryOp::Goals {
                status: String::from("any"),
                updated_since_secs: None,
                after: Some(String::from("..")),
            },
            QueryOp::Tracking {
                since_secs: Some(10),
                until_secs: Some(9),
                source: None,
                after: None,
            },
            QueryOp::Tracking {
                since_secs: None,
                until_secs: None,
                source: Some(String::new()),
                after: None,
            },
            QueryOp::Tracking {
                since_secs: None,
                until_secs: None,
                source: None,
                after: Some(String::from("-1")),
            },
            QueryOp::Memory {
                tag: String::from("a/b"),
                after: None,
            },
            QueryOp::Memory {
                tag: String::from("feed"),
                after: Some(String::from("a b")),
            },
        ];
        for op in &bad_queries {
            assert!(
                matches!(
                    handle_query_op(&config, &memory, op, &p),
                    Err(Error::SessionProtocol { .. })
                ),
                "{op:?}"
            );
        }
        assert!(
            !root.exists(),
            "a malformed op must not touch the filesystem"
        );
    }

    #[test]
    fn refused_content_and_limits_are_replies_not_session_ends() {
        let root = test_root("refused");
        let config = goals_config(&root, &ALL_GOALS);
        let p = principal(7);
        let empty_title = GoalsOp::GoalCreate {
            title: String::from(" "),
            detail: String::new(),
        };
        assert_eq!(
            handle_goals_op(&config, &empty_title, &p).unwrap(),
            GoalsResult::Rejected
        );
        let id = create(&config, &p, "run");
        let empty_edit = GoalsOp::GoalUpdate {
            id,
            title: None,
            detail: None,
        };
        assert_eq!(
            handle_goals_op(&config, &empty_edit, &p).unwrap(),
            GoalsResult::Rejected
        );
        assert_eq!(
            append(&config, &p, "user", "esc \u{1b}[2J"),
            GoalsResult::Rejected
        );
        for n in 1..hatchling_goals::MAX_GOALS_PER_USER {
            create(&config, &p, &format!("goal {n}"));
        }
        let one_more = GoalsOp::GoalCreate {
            title: String::from("one too many"),
            detail: String::new(),
        };
        assert_eq!(
            handle_goals_op(&config, &one_more, &p).unwrap(),
            GoalsResult::LimitReached
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failures_map_to_the_documented_replies() {
        use hatchling_goals::Error as E;
        let rejected = [
            E::InvalidText {
                field: "title",
                reason: "must not be empty",
            },
            E::EmptyEdit,
            E::InvalidName(String::from("x")),
        ];
        for error in &rejected {
            assert_eq!(failure_reply(error), GoalsResult::Rejected, "{error:?}");
        }
        for error in [E::GoalLimit { max: 1 }, E::TrackingFull { max: 1 }] {
            assert_eq!(
                failure_reply(&error),
                GoalsResult::LimitReached,
                "{error:?}"
            );
        }
        let opaque = [
            E::Store {
                source: hatchling_memory::Error::Corrupt,
            },
            E::Io {
                source: std::io::Error::other("x"),
            },
            E::InvalidRange { since: 2, until: 1 },
            E::MemoryUnavailable,
            E::ProvenanceMismatch,
            E::Corrupt,
        ];
        for error in &opaque {
            assert_eq!(failure_reply(error), GoalsResult::GoalsError, "{error:?}");
        }
    }

    #[test]
    fn backend_failures_are_opaque_replies() {
        // A regular file where the root should be: every op answers with
        // the opaque error, and the session (the Ok) survives.
        let root = test_root("backend");
        std::fs::write(&root, b"not a directory").unwrap();
        let config = goals_config(&root, &ALL_GOALS);
        let p = principal(7);
        assert_eq!(append(&config, &p, "user", "t"), GoalsResult::GoalsError);
        let create = GoalsOp::GoalCreate {
            title: String::from("t"),
            detail: String::new(),
        };
        assert_eq!(
            handle_goals_op(&config, &create, &p).unwrap(),
            GoalsResult::GoalsError
        );
        let query = QueryOp::Goals {
            status: String::from("any"),
            updated_since_secs: None,
            after: None,
        };
        assert_eq!(
            handle_query_op(&config, &no_memory(), &query, &p).unwrap(),
            QueryResult::QueryError
        );
        // A memory store that cannot open answers the same way.
        let memory = MemoryConfig {
            root: root.clone(),
            grants: vec![MemoryGrant {
                scope: String::from("user"),
                action: MemoryAction::Recall,
            }],
        };
        let recall = QueryOp::Memory {
            tag: String::from("feed"),
            after: None,
        };
        assert_eq!(
            handle_query_op(&config, &memory, &recall, &p).unwrap(),
            QueryResult::QueryError
        );
        let _ = std::fs::remove_file(&root);
    }

    #[test]
    fn queries_never_create_the_store() {
        // The query path reads through a FeedReader only: against a
        // store that was never written, every query answers empty and
        // leaves no directory behind.
        let root = test_root("no-create");
        let config = goals_config(&root, &ALL_GOALS);
        let p = principal(7);
        assert!(list(&config, &p, "any").is_empty());
        assert!(tracking(&config, &p, None, None, None).is_empty());
        assert!(!root.exists());
    }

    #[test]
    fn a_long_result_pages_within_one_frame_and_resumes() {
        let root = test_root("paging");
        let config = goals_config(&root, &ALL_GOALS);
        let p = principal(7);
        let text = "\"".repeat(hatchling_goals::MAX_TRACKING_TEXT_BYTES);
        for _ in 0..12 {
            assert!(matches!(
                append(&config, &p, "user", &text),
                GoalsResult::TrackingAppended { .. }
            ));
        }
        let mut after = None;
        let mut pages = 0;
        let mut seen = 0;
        let mut finished = false;
        // Bounded: a cursor that stops advancing fails the test instead
        // of looping forever.
        for _ in 0..24 {
            let op = QueryOp::Tracking {
                since_secs: None,
                until_secs: None,
                source: None,
                after: after.clone(),
            };
            let reply = handle_query_op(&config, &no_memory(), &op, &p).unwrap();
            let frame = serde_json::to_vec(&SessionResponse::Query(reply.clone())).unwrap();
            assert!(
                frame.len() <= MAX_FRAME_BYTES,
                "a page overflowed its frame: {}",
                frame.len()
            );
            let (found, next) = items(reply);
            assert!(!found.is_empty());
            seen += found.len();
            pages += 1;
            let Some(cursor) = next else {
                finished = true;
                break;
            };
            after = Some(cursor);
        }
        assert!(finished, "paging never reached the end");
        assert_eq!(seen, 12);
        assert!(pages > 1, "twelve worst-case entries must not fit one page");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_goals_op_maps_to_its_own_action() {
        let ops = [
            GoalsOp::GoalCreate {
                title: String::new(),
                detail: String::new(),
            },
            GoalsOp::GoalUpdate {
                id: String::new(),
                title: None,
                detail: None,
            },
            GoalsOp::GoalClose {
                id: String::new(),
                outcome: String::new(),
            },
            GoalsOp::TrackingAppend {
                source: String::new(),
                text: String::new(),
            },
        ];
        let actions: Vec<GoalsAction> = ops.iter().map(goals_action).collect();
        assert_eq!(actions, ALL_GOALS.get(..4).unwrap().to_vec());
    }
}

#[cfg(test)]
mod frame_bound_tests {
    use std::path::PathBuf;
    use std::str::FromStr as _;

    use hatchling_core::{MAX_FRAME_BYTES, QueryOp, QueryResult, SessionResponse};
    use hatchling_memory::{
        MAX_ENCODED_ITEM_BYTES, MemoryPrincipal, MemoryScope, RetentionPolicy, SessionId,
        reply_encoded_len,
    };

    use super::handle_query_op;
    use crate::config::{GoalsConfig, MemoryAction, MemoryConfig, MemoryGrant};

    #[test]
    fn the_largest_accepted_memory_item_pages_within_one_frame() {
        // The feed's query path wraps an item in its provenance before
        // the reply encodes it again: the largest item the store accepts
        // must still leave that reply inside one frame.
        let base = std::env::temp_dir();
        let goals_root: PathBuf = base.join(format!("hatchling-w47-goals-{}", std::process::id()));
        let memory_root: PathBuf =
            base.join(format!("hatchling-w47-memory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&memory_root);
        let p = MemoryPrincipal {
            peer_uid: 7,
            session_id: SessionId::from_str("sess-w47").unwrap(),
        };
        let store = crate::memory_sealed::open_published(&memory_root).unwrap();
        let write = |content: &str| {
            store.write(
                &p,
                &MemoryScope::User,
                &"big".parse().unwrap(),
                content,
                &["feed".parse().unwrap()],
                RetentionPolicy::Keep,
                1,
            )
        };
        let empty = reply_encoded_len(&serde_json::to_vec(&write("").unwrap()).unwrap()).unwrap();
        let item = write(&"\"".repeat((MAX_ENCODED_ITEM_BYTES - empty) / 4)).unwrap();
        assert!(
            MAX_ENCODED_ITEM_BYTES
                - reply_encoded_len(&serde_json::to_vec(&item).unwrap()).unwrap()
                < 4,
            "the item sits at the bound"
        );
        let memory = MemoryConfig {
            root: memory_root.clone(),
            grants: vec![MemoryGrant {
                scope: String::from("user"),
                action: MemoryAction::Recall,
            }],
        };
        let query = QueryOp::Memory {
            tag: String::from("feed"),
            after: None,
        };
        let reply = handle_query_op(&GoalsConfig::disabled(), &memory, &query, &p).unwrap();
        assert!(
            matches!(&reply, QueryResult::Items { items_json, next: None } if items_json.len() == 1),
            "{reply:?}"
        );
        let frame = serde_json::to_vec(&SessionResponse::Query(reply)).unwrap();
        assert!(frame.len() <= MAX_FRAME_BYTES, "{} bytes", frame.len());
        let _ = std::fs::remove_dir_all(&memory_root);
        let _ = std::fs::remove_dir_all(&goals_root);
    }
}
