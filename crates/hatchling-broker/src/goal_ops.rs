//! The owner's goals, from their app: listed, set, changed and closed.
//!
//! They're the goals the agent reads in a chat and a feed: the store keeps
//! each user's under the uid their sessions present, and the owner's agent
//! presents [`Config::agent_uid`], so the app acts as that principal too.
//! Only the owner reaches this (the API checks), and the agent's goals
//! grants don't bound it: those say what the agent may do, not the owner.

use std::time::{SystemTime, UNIX_EPOCH};

use hatchling_core::ui::{GoalOp, GoalResult, GoalState, GoalView};
use hatchling_goals::{
    FeedBody, FeedQuery, Goal, GoalChange, GoalEdit, GoalId, GoalOutcome, GoalStatus, GoalsStore,
    StatusFilter,
};
use hatchling_memory::{MemoryPrincipal, SessionId};

use crate::Config;

/// Does `op` on the owner's goals in the store `config` names.
#[must_use]
pub fn handle_goal_op(config: &Config, op: &GoalOp) -> GoalResult {
    if config.goals.root.as_os_str().is_empty() {
        return GoalResult::Off;
    }
    let Ok(session_id) = SessionId::generate() else {
        tracing::error!("no session id for the owner's goals");
        return GoalResult::Failed;
    };
    let principal = MemoryPrincipal {
        peer_uid: config.agent_uid.as_u32(),
        session_id,
    };
    let store = || GoalsStore::open(&config.goals.root);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let changed = match op {
        GoalOp::List => return list(config, &principal),
        GoalOp::Create { title, detail } => {
            return store()
                .and_then(|store| store.create_goal(&principal, title, detail, now))
                .map_or_else(|error| failed(&error), |goal| saved(&goal));
        }
        GoalOp::Update { id, title, detail } => {
            let Ok(id) = id.parse::<GoalId>() else {
                return GoalResult::NotFound;
            };
            let edit = GoalEdit {
                title: title.as_deref(),
                detail: detail.as_deref(),
            };
            store().and_then(|store| store.update_goal(&principal, &id, &edit, now))
        }
        GoalOp::Close { id, achieved } => {
            let Ok(id) = id.parse::<GoalId>() else {
                return GoalResult::NotFound;
            };
            let outcome = if *achieved {
                GoalOutcome::Achieved
            } else {
                GoalOutcome::Abandoned
            };
            store().and_then(|store| store.close_goal(&principal, &id, outcome, now))
        }
    };
    match changed {
        Ok(GoalChange::Changed(goal)) => saved(&goal),
        Ok(GoalChange::NotFound) => GoalResult::NotFound,
        Ok(GoalChange::AlreadyClosed) => GoalResult::AlreadyClosed,
        Err(error) => failed(&error),
    }
}

/// Every goal of `principal`'s: open first, the most recently changed
/// first within each.
fn list(config: &Config, principal: &MemoryPrincipal) -> GoalResult {
    let query = FeedQuery::Goals {
        status: StatusFilter::Any,
        updated_since_secs: None,
        after: None,
    };
    let Some(items) =
        crate::goals_ops::read_feed(&config.goals, &config.memory, principal, &query)
    else {
        return GoalResult::Failed;
    };
    let mut goals: Vec<GoalView> = items
        .iter()
        .filter_map(|item| {
            if let FeedBody::Goal(goal) = item.body() {
                Some(view(goal))
            } else {
                None
            }
        })
        .collect();
    goals.sort_by_key(|goal| {
        (
            goal.state != GoalState::Open,
            std::cmp::Reverse(goal.updated_at),
        )
    });
    GoalResult::Listed { goals }
}

/// `goal` as the app shows it.
fn view(goal: &Goal) -> GoalView {
    GoalView {
        id: goal.id.to_string(),
        title: goal.title.clone(),
        detail: goal.detail.clone(),
        state: match goal.status {
            GoalStatus::Open => GoalState::Open,
            GoalStatus::Closed {
                outcome: GoalOutcome::Achieved,
                ..
            } => GoalState::Achieved,
            GoalStatus::Closed {
                outcome: GoalOutcome::Abandoned,
                ..
            } => GoalState::Abandoned,
        },
        created_at: goal.created_at_secs,
        updated_at: goal.updated_at_secs,
    }
}

fn saved(goal: &Goal) -> GoalResult {
    GoalResult::Saved { goal: view(goal) }
}

/// What a store error says to the owner: refused text, or the limit, as
/// they are; anything else logged, and opaque.
fn failed(error: &hatchling_goals::Error) -> GoalResult {
    use hatchling_goals::Error as Goals;
    match error {
        Goals::InvalidText { .. } | Goals::EmptyEdit => GoalResult::Rejected,
        Goals::GoalLimit { .. } => GoalResult::LimitReached,
        Goals::Store { .. }
        | Goals::Io { .. }
        | Goals::InvalidName(_)
        | Goals::TrackingFull { .. }
        | Goals::InvalidRange { .. } => {
            tracing::error!(%error, "the owner's goals couldn't be read or changed");
            GoalResult::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(name: &str) -> (Config, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-goal-ops-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = crate::api_auth::tests::owner_config(None);
        config.goals.root = dir.join("goals");
        (config, dir)
    }

    /// The goal a result saved.
    fn saved_goal(result: GoalResult) -> GoalView {
        if let GoalResult::Saved { goal } = result {
            Some(goal)
        } else {
            None
        }
        .unwrap()
    }

    fn create(config: &Config, title: &str) -> GoalView {
        let op = GoalOp::Create {
            title: title.to_owned(),
            detail: String::from("what done looks like"),
        };
        saved_goal(handle_goal_op(config, &op))
    }

    fn listed(config: &Config) -> Vec<(String, GoalState)> {
        let goals = if let GoalResult::Listed { goals } = handle_goal_op(config, &GoalOp::List) {
            Some(goals)
        } else {
            None
        };
        goals
            .unwrap()
            .into_iter()
            .map(|goal| (goal.title, goal.state))
            .collect()
    }

    #[test]
    fn the_owner_sets_lists_changes_and_closes_goals() {
        let (config, dir) = config("lifecycle");
        assert_eq!(listed(&config), []);
        let run = create(&config, "Run a 10k");
        assert_eq!(run.state, GoalState::Open);
        assert_eq!(run.detail, "what done looks like");
        assert!(run.created_at > 0 && run.updated_at >= run.created_at);
        let read = create(&config, "Read twelve books");
        let update = GoalOp::Update {
            id: run.id.clone(),
            title: Some(String::from("Run a half marathon")),
            detail: None,
        };
        let goal = saved_goal(handle_goal_op(&config, &update));
        assert_eq!(
            (goal.title.as_str(), goal.detail.as_str()),
            ("Run a half marathon", "what done looks like")
        );
        let close = |id: &str, achieved| GoalOp::Close {
            id: id.to_owned(),
            achieved,
        };
        let goal = saved_goal(handle_goal_op(&config, &close(&read.id, false)));
        assert_eq!(goal.state, GoalState::Abandoned);
        // Open first; closed after.
        assert_eq!(
            listed(&config),
            [
                (String::from("Run a half marathon"), GoalState::Open),
                (String::from("Read twelve books"), GoalState::Abandoned),
            ]
        );
        let goal = saved_goal(handle_goal_op(&config, &close(&run.id, true)));
        assert_eq!(goal.state, GoalState::Achieved);
        // Closed is final.
        assert_eq!(
            handle_goal_op(&config, &close(&run.id, true)),
            GoalResult::AlreadyClosed
        );
        assert_eq!(handle_goal_op(&config, &update), GoalResult::AlreadyClosed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_goals_list_first_and_the_newest_first_within_each() {
        let (config, dir) = config("order");
        let first = create(&config, "First");
        create(&config, "Second");
        // Changed later, so newer.
        let touch = GoalOp::Update {
            id: first.id.clone(),
            title: None,
            detail: Some(String::from("changed")),
        };
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(matches!(
            handle_goal_op(&config, &touch),
            GoalResult::Saved { .. }
        ));
        let titles: Vec<String> = listed(&config).into_iter().map(|(title, _)| title).collect();
        assert_eq!(titles, ["First", "Second"]);
        let close = GoalOp::Close {
            id: first.id,
            achieved: true,
        };
        assert!(matches!(
            handle_goal_op(&config, &close),
            GoalResult::Saved { .. }
        ));
        let titles: Vec<String> = listed(&config).into_iter().map(|(title, _)| title).collect();
        assert_eq!(titles, ["Second", "First"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_the_store_refuses_is_said_and_nothing_is_kept() {
        let (config, dir) = config("refused");
        let op = |title: &str| GoalOp::Create {
            title: title.to_owned(),
            detail: String::new(),
        };
        assert_eq!(handle_goal_op(&config, &op("")), GoalResult::Rejected);
        assert_eq!(handle_goal_op(&config, &op("two\nlines")), GoalResult::Rejected);
        let nothing = GoalOp::Update {
            id: create(&config, "Kept").id,
            title: None,
            detail: None,
        };
        assert_eq!(handle_goal_op(&config, &nothing), GoalResult::Rejected);
        assert_eq!(listed(&config).len(), 1);
        for op in [
            GoalOp::Update {
                id: String::from("not an id"),
                title: Some(String::from("x")),
                detail: None,
            },
            GoalOp::Close {
                id: String::from("not an id"),
                achieved: true,
            },
        ] {
            assert_eq!(handle_goal_op(&config, &op), GoalResult::NotFound);
        }
        let unknown = GoalId::generate().unwrap().to_string();
        let close = GoalOp::Close {
            id: unknown,
            achieved: true,
        };
        assert_eq!(handle_goal_op(&config, &close), GoalResult::NotFound);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_no_store_there_are_no_goals_and_a_broken_one_fails() {
        let (mut config, dir) = config("off");
        let off = config.goals.root.clone();
        config.goals.root = std::path::PathBuf::new();
        assert_eq!(handle_goal_op(&config, &GoalOp::List), GoalResult::Off);
        // A file where the store should be: it can't be opened or read.
        std::fs::write(&off, "not a directory").unwrap();
        config.goals.root = off;
        assert_eq!(handle_goal_op(&config, &GoalOp::List), GoalResult::Failed);
        let create = GoalOp::Create {
            title: String::from("x"),
            detail: String::new(),
        };
        assert_eq!(handle_goal_op(&config, &create), GoalResult::Failed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_limit_is_said() {
        assert_eq!(
            failed(&hatchling_goals::Error::GoalLimit { max: 1 }),
            GoalResult::LimitReached
        );
        assert_eq!(
            failed(&hatchling_goals::Error::InvalidName(String::from("x"))),
            GoalResult::Failed
        );
    }
}
