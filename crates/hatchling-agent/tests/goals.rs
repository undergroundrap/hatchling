//! Component tests: the agent's goals API and the feed's read-only
//! handle against the real broker.
//!
//! The [`Goals`](common::BrokerBehavior::Goals) fixture runs the true
//! broker pipeline ([`serve_connection`](hatchling_broker::serve_connection)):
//! peer gate, deny-by-default grants, principal binding, and the goals
//! and memory stores on disk. These tests drive it only through
//! `hatchling_agent::goals` (and `memory` to seed a recalled item), so
//! the agent client and the broker's `goals_ops` are pinned together.

pub(crate) mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::{BrokerBehavior, FixtureBroker};
use hatchling_agent::goals::{self, FeedHandle, GoalChange, QueryPage};
use hatchling_agent::memory::{self, MemoryScope};
use hatchling_agent::{Cell, Config, Error, LogLevel, PeerUid, Ready};
use hatchling_broker::{GoalsAction, GoalsConfig, MemoryAction, MemoryConfig, MemoryGrant};
use hatchling_core::QueryOp;
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_goals::{FeedBody, FeedItem, GoalStatus, SourceStore};

const ALL_GOALS: [GoalsAction; 6] = [
    GoalsAction::GoalCreate,
    GoalsAction::GoalUpdate,
    GoalsAction::GoalClose,
    GoalsAction::TrackingAppend,
    GoalsAction::QueryGoals,
    GoalsAction::QueryTracking,
];

/// A `Ready` cell against the fixture broker's socket.
fn ready_cell(broker: &FixtureBroker) -> Result<Cell<Ready>, Error> {
    let uid = rustix::process::getuid().as_raw();
    let config = Config {
        cell_id: "cell-goals".parse()?,
        broker: hatchling_agent::BrokerAddress::Socket(broker.socket_path.parse()?),
        broker_uid: PeerUid::from_kernel(uid),
        log_level: LogLevel::Error,
        model_endpoint: None,
        model_name: None,
        model_max_steps: 10,
        model_context_window: None,
        model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
        run_subagent: false,
        run_chat: false,
        run_history: Vec::new(),
    };
    let cell = Cell::new(config)
        .check_preconditions()?
        .connect_broker()?
        .authorize()?
        .handshake()?;
    // Bounded replies: a broker that stops answering fails the test
    // instead of hanging it (and a mutation run with it).
    cell.broker()
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .map_err(|source| Error::GoalsIo { source })?;
    Ok(cell)
}

/// Store roots unique to this test process and test; the broker
/// creates them on first write.
fn roots(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir();
    let goals = base.join(format!("hlfx-goals-{}-{name}", std::process::id()));
    let memory = base.join(format!("hlfx-goals-memory-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&goals);
    let _ = std::fs::remove_dir_all(&memory);
    (goals, memory)
}

fn user_grant(action: MemoryAction) -> MemoryGrant {
    MemoryGrant {
        scope: String::from("user"),
        action,
    }
}

fn spawn(
    name: &str,
    goals_root: &Path,
    goals_grants: &[GoalsAction],
    memory_root: &Path,
    memory_grants: Vec<MemoryGrant>,
) -> std::io::Result<FixtureBroker> {
    FixtureBroker::spawn(
        name,
        BrokerBehavior::Goals {
            goals: GoalsConfig {
                root: goals_root.to_path_buf(),
                grants: goals_grants.to_vec(),
            },
            memory: MemoryConfig {
                root: memory_root.to_path_buf(),
                grants: memory_grants,
            },
        },
    )
}

fn goals_query(status: &str) -> QueryOp {
    QueryOp::Goals {
        status: status.to_owned(),
        updated_since_secs: None,
        after: None,
    }
}

fn tracking_query(source: Option<&str>) -> QueryOp {
    QueryOp::Tracking {
        since_secs: None,
        until_secs: None,
        source: source.map(str::to_owned),
        after: None,
    }
}

fn parsed(page: &QueryPage) -> Result<Vec<FeedItem>, hatchling_goals::Error> {
    page.items_json
        .iter()
        .map(|json| FeedItem::from_json(json))
        .collect()
}

#[test]
fn goal_lifecycle_and_tracking_through_the_real_broker() {
    let (goals_root, memory_root) = roots("lifecycle");
    let broker = spawn(
        "goals-lifecycle",
        &goals_root,
        &ALL_GOALS,
        &memory_root,
        vec![],
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();
    let feed = FeedHandle::new(stream);

    let id = goals::create_goal(stream, "run a 5k", "by spring").unwrap();
    let open = parsed(&feed.query(&goals_query("open")).unwrap()).unwrap();
    assert_eq!(open.len(), 1, "exactly one open goal: {open:?}");
    assert!(
        matches!(open.first().map(FeedItem::body), Some(FeedBody::Goal(g)) if g.id.as_str() == id)
    );

    assert_eq!(
        goals::update_goal(stream, &id, Some("run a 10k"), None).unwrap(),
        GoalChange::Changed
    );
    assert_eq!(
        goals::close_goal(stream, &id, "achieved").unwrap(),
        GoalChange::Changed
    );
    assert_eq!(
        goals::close_goal(stream, &id, "achieved").unwrap(),
        GoalChange::AlreadyClosed
    );
    assert_eq!(
        goals::update_goal(stream, "no-such-goal", Some("x"), None).unwrap(),
        GoalChange::NotFound
    );
    let closed = parsed(&feed.query(&goals_query("closed")).unwrap()).unwrap();
    assert!(matches!(
        closed.first().map(FeedItem::body),
        Some(FeedBody::Goal(g)) if g.title == "run a 10k" && matches!(g.status, GoalStatus::Closed { .. })
    ));
    assert!(
        feed.query(&goals_query("open"))
            .unwrap()
            .items_json
            .is_empty()
    );

    assert_eq!(goals::append_tracking(stream, "user", "ran 3k").unwrap(), 0);
    assert_eq!(
        goals::append_tracking(stream, "whatsapp", "see you at the race").unwrap(),
        1
    );
    let from_whatsapp = parsed(&feed.query(&tracking_query(Some("whatsapp"))).unwrap()).unwrap();
    let labels: Vec<String> = from_whatsapp
        .iter()
        .map(|i| i.provenance().label())
        .collect();
    assert_eq!(labels, vec!["from tracking (originally from whatsapp)"]);
    assert_eq!(
        parsed(&feed.query(&tracking_query(None)).unwrap())
            .unwrap()
            .len(),
        2
    );

    // Refused content comes back as a reply; the session survives it.
    assert!(matches!(
        goals::create_goal(stream, "", ""),
        Err(Error::GoalsRejected { action }) if action == "goal-create"
    ));
    assert_eq!(
        goals::append_tracking(stream, "user", "still here").unwrap(),
        2
    );
    drop(cell);
    let _ = std::fs::remove_dir_all(&goals_root);
}

#[test]
fn one_query_path_returns_labeled_items_from_all_three_stores() {
    // Acceptance: through one session and one query handle, memory,
    // tracking, and goal items all arrive carrying their source.
    let (goals_root, memory_root) = roots("labels");
    let broker = spawn(
        "goals-labels",
        &goals_root,
        &ALL_GOALS,
        &memory_root,
        vec![
            user_grant(MemoryAction::Write),
            user_grant(MemoryAction::Recall),
        ],
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();
    goals::create_goal(stream, "run a 5k", "").unwrap();
    goals::append_tracking(stream, "calendar", "race day booked").unwrap();
    memory::store(
        stream,
        MemoryScope::User,
        "pref-1",
        "prefers mornings",
        &["feed"],
        "keep",
    )
    .unwrap();

    let feed = FeedHandle::new(stream);
    let mut stores = BTreeSet::new();
    for op in [
        goals_query("any"),
        tracking_query(None),
        QueryOp::Memory {
            tag: String::from("feed"),
            after: None,
        },
    ] {
        for item in parsed(&feed.query(&op).unwrap()).unwrap() {
            let label = item.provenance().label();
            assert!(
                label.starts_with(&format!("from {} (", item.provenance().store.as_str())),
                "{label}"
            );
            stores.insert(item.provenance().store.as_str());
        }
    }
    let expected: BTreeSet<&str> = [
        SourceStore::Goal,
        SourceStore::Tracking,
        SourceStore::Memory,
    ]
    .iter()
    .map(|s| s.as_str())
    .collect();
    assert_eq!(stores, expected);
    drop(cell);
    let _ = std::fs::remove_dir_all(&goals_root);
    let _ = std::fs::remove_dir_all(&memory_root);
}

#[test]
fn a_write_smuggled_through_the_query_kind_ends_the_session_and_writes_nothing() {
    // Negative acceptance, end to end: even holding every grant, a goals
    // write re-labeled as a query is not a query. The broker refuses to
    // decode it, ends the session, and the store never sees it.
    let (goals_root, memory_root) = roots("smuggle");
    let broker = spawn(
        "goals-smuggle",
        &goals_root,
        &ALL_GOALS,
        &memory_root,
        vec![],
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();
    let smuggled = br#"{"kind":"query","op":"goal-create","title":"planted","detail":""}"#;
    write_frame(stream, smuggled).unwrap();
    assert!(
        read_frame(stream).is_err(),
        "the broker must not answer a smuggled write"
    );
    assert!(
        !goals_root.join("goals").exists(),
        "the smuggled write must not reach the store"
    );
    drop(cell);
    let _ = std::fs::remove_dir_all(&goals_root);
}

#[test]
fn every_op_is_denied_without_its_grant() {
    let (goals_root, memory_root) = roots("denied");
    // Queries only: the shape a feed's session would be configured with.
    let broker = spawn(
        "goals-denied",
        &goals_root,
        &[GoalsAction::QueryGoals],
        &memory_root,
        vec![],
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();
    let denied = |result: Result<(), Error>, expected: &str| matches!(result, Err(Error::GoalsDenied { action }) if action == expected);
    assert!(denied(
        goals::create_goal(stream, "t", "").map(|_| ()),
        "goal-create"
    ));
    assert!(denied(
        goals::update_goal(stream, "g1", Some("t"), None).map(|_| ()),
        "goal-update"
    ));
    assert!(denied(
        goals::close_goal(stream, "g1", "achieved").map(|_| ()),
        "goal-close"
    ));
    assert!(denied(
        goals::append_tracking(stream, "user", "t").map(|_| ()),
        "tracking-append"
    ));
    let feed = FeedHandle::new(stream);
    assert!(denied(
        feed.query(&tracking_query(None)).map(|_| ()),
        "query-tracking"
    ));
    let memory = QueryOp::Memory {
        tag: String::from("feed"),
        after: None,
    };
    assert!(denied(feed.query(&memory).map(|_| ()), "user:recall"));
    // The one granted query still answers, on the same session.
    assert!(
        feed.query(&goals_query("any"))
            .unwrap()
            .items_json
            .is_empty()
    );
    assert!(
        !goals_root.exists(),
        "denied ops and empty reads create nothing"
    );
    drop(cell);
}
