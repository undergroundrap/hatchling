use hatchling_vault::MasterKey;

use super::*;

/// 2026-11-26T11:31:00Z.
const T0: i64 = 1_795_692_660;

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

struct Root(std::path::PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn store(name: &str) -> (Root, SchedulerConfig) {
    let root = std::env::temp_dir().join(format!(
        "hatchling-test-review-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let mut scheduler = SchedulerConfig::disabled();
    scheduler.root = root.clone();
    (Root(root), scheduler)
}

fn evidence() -> Vec<String> {
    vec![String::from("message:c1/m1")]
}

const fn proposal<'a>(change: &'a str, evidence: &'a [String]) -> Proposal<'a> {
    Proposal {
        proposed_by: "memory",
        kind: ReviewKind::Replace,
        target: Some("memory:k1"),
        change,
        why: "the owner said otherwise since",
        evidence,
    }
}

/// The id it was queued under, or none, which no later check accepts.
fn queued(proposed: Proposed) -> String {
    let Proposed::Queued(id) = proposed else {
        return String::new();
    };
    id
}

/// Proposed items wait, the newest first, sealed under the store's key.
#[test]
fn proposals_wait_newest_first_sealed() {
    let (_root, scheduler) = store("wait");
    let key = MasterKey::generate().unwrap();
    let cited = evidence();
    let first = queued(
        propose(
            &scheduler,
            &key,
            &proposal("tea, not coffee", &cited),
            at(T0),
        )
        .unwrap(),
    );
    let second =
        queued(propose(&scheduler, &key, &proposal("mornings", &cited), at(T0 + 60)).unwrap());
    assert_ne!(first, second);
    let items = list(&scheduler, &key).unwrap();
    let ids: Vec<&str> = items.iter().map(|item| item.item_id.as_str()).collect();
    assert_eq!(ids, [second.as_str(), first.as_str()]);
    let newest = items.first().unwrap();
    assert_eq!(newest.state, ReviewState::Waiting);
    assert_eq!(newest.proposed_by, "memory");
    assert_eq!(newest.target.as_deref(), Some("memory:k1"));
    assert_eq!(newest.evidence, cited);
    assert_eq!(newest.proposed_at, at(T0 + 60).to_string());
    assert_eq!(newest.decided_at, None);
    let other = MasterKey::generate().unwrap();
    assert!(list(&scheduler, &other).is_err(), "read under another key");
}

/// The owner approves or rejects a waiting item, once: a decided item
/// isn't decided again, and an id that names none is not found.
#[test]
fn a_waiting_item_is_decided_once() {
    let (_root, scheduler) = store("decide");
    let key = MasterKey::generate().unwrap();
    let cited = evidence();
    let yes = queued(propose(&scheduler, &key, &proposal("one", &cited), at(T0)).unwrap());
    let no = queued(propose(&scheduler, &key, &proposal("two", &cited), at(T0)).unwrap());
    let approved = decide(&scheduler, &key, &yes, true, at(T0 + 5)).unwrap();
    assert!(
        matches!(&approved, ReviewResult::Decided { item }
            if item.state == ReviewState::Approved && item.decided_at.as_deref() == Some(at(T0 + 5).to_string().as_str())),
        "{approved:?}"
    );
    let rejected = decide(&scheduler, &key, &no, false, at(T0 + 6)).unwrap();
    assert!(
        matches!(&rejected, ReviewResult::Decided { item } if item.state == ReviewState::Rejected)
    );
    assert_eq!(
        decide(&scheduler, &key, &yes, false, at(T0 + 7)).unwrap(),
        ReviewResult::NotWaiting {
            state: ReviewState::Approved
        }
    );
    let states: Vec<ReviewState> = list(&scheduler, &key)
        .unwrap()
        .iter()
        .map(|item| item.state)
        .collect();
    assert_eq!(
        states,
        [ReviewState::Rejected, ReviewState::Approved],
        "kept as decided"
    );
    for id in ["", "deadbeef", "../review-index", &"a".repeat(65)] {
        assert_eq!(
            decide(&scheduler, &key, id, true, at(T0)).unwrap(),
            ReviewResult::NotFound,
            "{id:?}"
        );
    }
}

/// What breaks a bound isn't kept.
#[test]
fn what_breaks_a_bound_is_refused() {
    let (_root, scheduler) = store("bounds");
    let key = MasterKey::generate().unwrap();
    let cited = evidence();
    let long = "x".repeat(MAX_CHANGE_BYTES + 1);
    let fits = "x".repeat(MAX_CHANGE_BYTES);
    let many = vec![String::from("memory:k1"); MAX_EVIDENCE + 1];
    let enough = vec![String::from("memory:k1"); MAX_EVIDENCE];
    let none: Vec<String> = Vec::new();
    let why = "y".repeat(MAX_WHY_BYTES + 1);
    for (what, bad) in [
        ("change", proposal(&long, &cited)),
        ("blank", proposal("  ", &cited)),
        ("evidence", proposal("c", &many)),
        ("no evidence", proposal("c", &none)),
        (
            "why",
            Proposal {
                why: &why,
                ..proposal("c", &cited)
            },
        ),
        (
            "loop",
            Proposal {
                proposed_by: "",
                ..proposal("c", &cited)
            },
        ),
        (
            "target",
            Proposal {
                target: Some(""),
                ..proposal("c", &cited)
            },
        ),
    ] {
        assert!(
            matches!(
                propose(&scheduler, &key, &bad, at(T0)).unwrap(),
                Proposed::Refused(_)
            ),
            "{what}"
        );
    }
    assert!(
        list(&scheduler, &key).unwrap_or_default().is_empty(),
        "nothing kept"
    );
    assert!(matches!(
        propose(&scheduler, &key, &proposal(&fits, &enough), at(T0)).unwrap(),
        Proposed::Queued(_)
    ));
    let untargeted = Proposal {
        target: None,
        kind: ReviewKind::Guidance,
        ..proposal("c", &cited)
    };
    assert!(matches!(
        propose(&scheduler, &key, &untargeted, at(T0)).unwrap(),
        Proposed::Queued(_)
    ));
}

/// Full, the oldest decided item gives way; a waiting one never does, and
/// a queue of waiting items alone refuses another.
#[test]
fn a_full_queue_lets_only_a_decided_item_go() {
    let (_root, scheduler) = store("full");
    let key = MasterKey::generate().unwrap();
    let cited = evidence();
    let mut ids = Vec::new();
    for n in 0..MAX_ITEMS {
        let change = format!("change {n}");
        ids.push(queued(
            propose(&scheduler, &key, &proposal(&change, &cited), at(T0)).unwrap(),
        ));
    }
    assert_eq!(
        propose(&scheduler, &key, &proposal("one more", &cited), at(T0)).unwrap(),
        Proposed::Full
    );
    // Decide the second-oldest: it, not the oldest waiting one, gives way.
    let second = ids.get(1).unwrap();
    let _ = decide(&scheduler, &key, second, false, at(T0)).unwrap();
    let newest = queued(propose(&scheduler, &key, &proposal("one more", &cited), at(T0)).unwrap());
    let kept = list(&scheduler, &key).unwrap();
    assert_eq!(kept.len(), MAX_ITEMS);
    assert!(
        kept.iter().all(|item| item.item_id != *second),
        "the decided one went"
    );
    assert!(
        kept.iter()
            .any(|item| item.item_id == *ids.first().unwrap()),
        "the oldest waiting one stays"
    );
    assert_eq!(
        kept.first().map(|item| item.item_id.as_str()),
        Some(newest.as_str())
    );
}

/// The owner's operations, answered: off with no store.
#[test]
fn the_owners_operations_are_answered() {
    let (_root, scheduler) = store("handle");
    let key = MasterKey::generate().unwrap();
    let mut config = crate::api_auth::tests::owner_config(None);
    assert_eq!(handle(&config, &key, &ReviewOp::List), ReviewResult::Off);
    config.scheduler.root = scheduler.root.clone();
    assert_eq!(
        handle(&config, &key, &ReviewOp::List),
        ReviewResult::Listed { items: Vec::new() }
    );
    let cited = evidence();
    let id = queued(propose(&scheduler, &key, &proposal("one", &cited), at(T0)).unwrap());
    assert!(matches!(
        handle(&config, &key, &ReviewOp::Decide { item_id: id, approve: true }),
        ReviewResult::Decided { item } if item.state == ReviewState::Approved
    ));
    let other = MasterKey::generate().unwrap();
    assert_eq!(
        handle(&config, &other, &ReviewOp::List),
        ReviewResult::Failed
    );
}

/// Within every bound, an item whose text JSON escapes past one sealed
/// entry is refused, never failed on; and a full index fits one.
#[test]
fn what_cant_be_kept_whole_is_refused_and_the_index_fits() {
    let (_root, scheduler) = store("escaped");
    let key = MasterKey::generate().unwrap();
    let control = |bytes: usize| "\u{1}".repeat(bytes);
    let change = control(MAX_CHANGE_BYTES);
    let why = control(MAX_WHY_BYTES);
    let cited = vec![control(MAX_NAME_BYTES); MAX_EVIDENCE];
    let escaped = Proposal {
        why: &why,
        ..proposal(&change, &cited)
    };
    assert_eq!(
        propose(&scheduler, &key, &escaped, at(T0)).unwrap(),
        Proposed::Refused("too long to keep, written out")
    );
    let ids = vec!["f".repeat(64); MAX_ITEMS];
    assert!(serde_json::to_vec(&ids).unwrap().len() <= hatchling_core::MAX_SECRET_BYTES);
}
