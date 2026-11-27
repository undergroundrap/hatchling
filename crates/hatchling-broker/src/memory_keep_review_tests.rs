//! WO-057 slice 2: in automatic memory, one asked in a conversation that
//! read from outside, or by a background run, waits on the owner's Review
//! page, and their decision there is applied.

use std::sync::Arc;

use hatchling_core::MemoryResult;
use hatchling_core::ui::{
    ChatAuthor, ReviewItem, ReviewKind, ReviewOp, ReviewResult, ReviewState, UiEventBody,
};
use hatchling_memory::{KeptBy, MemoryPrincipal, MemoryScope, SessionId};
use hatchling_vault::MasterKey;

use super::{ProposedIn, RECALL_TAG, SUPERSEDED_TAG, propose_in};
use crate::Config;
use crate::approval_board::board;
use crate::config::{MemoryAction, MemoryGrant};
use crate::memory_mode::Mode;
use crate::memory_proposal::Asked;

const RUN: &str = "run-0123456789abcdef0123456789abcdef";

/// A broker that may propose memory, with scheduling on, so the owner's
/// mode is kept (Automatic) and their Review page is there.
fn broker(name: &str) -> (Config, std::path::PathBuf, Arc<crate::api_stream::Bus>) {
    let dir = std::env::temp_dir().join(format!(
        "hatchling-memory-review-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = crate::api_auth::tests::owner_config(None);
    config.vault.root = dir.join("vault");
    config.memory.root = dir.join("memory");
    config.scheduler.root = dir.join("schedules");
    config.memory.grants = vec![MemoryGrant {
        scope: String::from("user"),
        action: MemoryAction::Propose,
    }];
    let bus = Arc::new(crate::api_stream::Bus::starting_at(1));
    board(&config).attach(Arc::clone(&bus));
    (config, dir, bus)
}

fn in_chat() -> ProposedIn {
    ProposedIn {
        run: Some(String::from(RUN)),
        chat: Some(String::from("local")),
    }
}

fn in_background() -> ProposedIn {
    ProposedIn {
        run: Some(String::from(RUN)),
        chat: None,
    }
}

fn recalled(config: &Config, tag: &str) -> Vec<hatchling_memory::MemoryItem> {
    let principal = MemoryPrincipal {
        peer_uid: config.agent_uid.as_u32(),
        session_id: SessionId::generate().unwrap(),
    };
    crate::memory_sealed::open_published(&config.memory.root)
        .map(|store| {
            store
                .recall_by_tag(
                    &principal,
                    &MemoryScope::User,
                    &tag.parse().unwrap(),
                    u64::MAX / 2,
                )
                .unwrap()
        })
        .unwrap_or_default()
}

fn told(bus: &crate::api_stream::Bus) -> Vec<String> {
    bus.after(None)
        .into_iter()
        .filter_map(|event| {
            if let UiEventBody::ChatMessage(message) = event.body {
                (message.author == ChatAuthor::System).then_some(message.text)
            } else {
                None
            }
        })
        .collect()
}

fn carded(bus: &crate::api_stream::Bus) -> bool {
    bus.after(None)
        .into_iter()
        .any(|event| matches!(event.body, UiEventBody::Confirmation(_)))
}

/// What waits on the owner's Review page.
fn listed(config: &Config, key: &MasterKey) -> Vec<ReviewItem> {
    let ReviewResult::Listed { items } = crate::review_queue::handle(config, key, &ReviewOp::List)
    else {
        return Vec::new();
    };
    items
}

fn decide(config: &Config, key: &MasterKey, item: &ReviewItem, approve: bool) {
    let decided = crate::review_queue::handle(
        config,
        key,
        &ReviewOp::Decide {
            item_id: item.item_id.clone(),
            approve,
        },
    );
    assert!(
        matches!(decided, ReviewResult::Decided { .. }),
        "{decided:?}"
    );
}

#[test]
fn one_asked_after_reading_from_outside_waits_on_the_review_page_once() {
    let (config, dir, bus) = broker("waits");
    let key = MasterKey::generate().unwrap();
    let asked = Asked {
        why: Some("They said so."),
        ..Asked::about_owner("They live in Bath.")
    };
    assert_eq!(
        propose_in(&config, Some(&key), "task-a", &asked, in_chat(), false),
        MemoryResult::Proposed
    );
    let items = listed(&config, &key);
    assert_eq!(items.len(), 1, "{items:?}");
    let item = items.first().unwrap();
    assert_eq!(item.kind, ReviewKind::Remember);
    assert_eq!(item.proposed_by, "chat");
    assert_eq!(item.change, "They live in Bath.");
    assert_eq!(item.state, ReviewState::Waiting);
    assert_eq!(
        item.evidence,
        [format!("run:{RUN}"), String::from("chat:local")]
    );
    assert_eq!(
        item.why,
        "Asked in a conversation that read from outside, so it's kept only if you approve it. The agent's reason: They said so."
    );
    assert!(!carded(&bus), "no card to stop for");
    assert_eq!(
        told(&bus),
        [
            "Waiting on your Memory page: “They live in Bath.” This conversation read from outside, so it's kept only if you approve it there."
        ]
    );
    assert!(recalled(&config, RECALL_TAG).is_empty(), "nothing kept yet");
    // Asked again in the same words: still one.
    assert_eq!(
        propose_in(&config, Some(&key), "task-a", &asked, in_chat(), false),
        MemoryResult::Proposed
    );
    assert_eq!(listed(&config, &key).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn approved_on_the_review_page_it_is_kept_on_the_owners_word() {
    let (config, dir, bus) = broker("approve");
    let key = MasterKey::generate().unwrap();
    let asked = Asked {
        kind: Some("guidance"),
        why: Some("They live in Europe."),
        how_to_apply: Some("Any measurement."),
        ..Asked::about_owner("Use metric units.")
    };
    assert_eq!(
        propose_in(&config, Some(&key), "task-b", &asked, in_chat(), false),
        MemoryResult::Proposed
    );
    let item = listed(&config, &key).into_iter().next().unwrap();
    decide(&config, &key, &item, true);
    let kept = recalled(&config, RECALL_TAG);
    assert_eq!(kept.len(), 1, "{kept:?}");
    let kept = kept.first().unwrap();
    assert_eq!(kept.content, "Use metric units.");
    let record = kept.record.as_ref().unwrap();
    // The owner approved it: their word, by the item they decided.
    assert_eq!(record.kept_by, KeptBy::Owner);
    assert_eq!(record.approval.as_deref(), Some(item.item_id.as_str()));
    assert_eq!(record.run.as_deref(), Some(RUN));
    assert_eq!(record.chat.as_deref(), Some("local"));
    assert_eq!(record.kind, Some(hatchling_memory::MemoryKind::Guidance));
    assert_eq!(record.why.as_deref(), Some("They live in Europe."));
    assert_eq!(record.how_to_apply.as_deref(), Some("Any measurement."));
    assert!(told(&bus).contains(&String::from(
        "Remembered: “Use metric units.” The agent is given it from now on."
    )));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rejected_on_the_review_page_its_words_are_refused_from_then_on() {
    let (config, dir, _bus) = broker("reject");
    let key = MasterKey::generate().unwrap();
    let asked = Asked::about_owner("They hate mornings.");
    assert_eq!(
        propose_in(&config, Some(&key), "task-c", &asked, in_chat(), false),
        MemoryResult::Proposed
    );
    let item = listed(&config, &key).into_iter().next().unwrap();
    decide(&config, &key, &item, false);
    assert!(recalled(&config, RECALL_TAG).is_empty());
    // Not kept at once in a clean conversation, nor asked again.
    for clean in [true, false] {
        assert!(
            matches!(
                propose_in(&config, Some(&key), "task-d", &asked, in_chat(), clean),
                MemoryResult::Unasked { reason } if reason == "the owner said no to keeping this"
            ),
            "{clean}"
        );
    }
    assert_eq!(listed(&config, &key).len(), 1, "not queued again");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_runs_memory_waits_without_a_word_in_the_chat() {
    let (config, dir, bus) = broker("background");
    let key = MasterKey::generate().unwrap();
    assert_eq!(
        propose_in(
            &config,
            Some(&key),
            "task-e",
            &Asked::about_owner("They take the 8:10 train."),
            in_background(),
            false
        ),
        MemoryResult::Proposed
    );
    let items = listed(&config, &key);
    let item = items.first().unwrap();
    assert_eq!(item.proposed_by, "run");
    assert_eq!(item.evidence, [format!("run:{RUN}")]);
    assert!(
        item.why
            .starts_with("Asked by a scheduled or background run")
    );
    assert!(told(&bus).is_empty() && !carded(&bus));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn in_ask_me_or_with_no_review_page_it_is_a_card() {
    let (config, dir, bus) = broker("ask");
    let key = MasterKey::generate().unwrap();
    crate::memory_mode::set(&config.scheduler, Mode::Ask).unwrap();
    assert_eq!(
        propose_in(
            &config,
            Some(&key),
            "task-f",
            &Asked::about_owner("They cycle."),
            in_chat(),
            false
        ),
        MemoryResult::Proposed
    );
    assert!(carded(&bus));
    assert!(listed(&config, &key).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_correction_waits_naming_what_it_corrects_and_approved_retires_it() {
    let (config, dir, _bus) = broker("correct");
    let key = MasterKey::generate().unwrap();
    let kept = propose_in(
        &config,
        Some(&key),
        "task-g",
        &Asked::about_owner("They live in Bath."),
        in_chat(),
        true,
    );
    let old = if let MemoryResult::Kept { id } = kept {
        Some(id)
    } else {
        None
    }
    .expect("kept at once in a clean conversation");
    let correcting = Asked {
        supersedes: Some(old.as_str()),
        ..Asked::about_owner("They live in Bristol now.")
    };
    assert_eq!(
        propose_in(&config, Some(&key), "task-g", &correcting, in_chat(), false),
        MemoryResult::Proposed
    );
    let item = listed(&config, &key).into_iter().next().unwrap();
    assert_eq!(item.target, Some(format!("memory:{old}")));
    decide(&config, &key, &item, true);
    let words: Vec<String> = recalled(&config, RECALL_TAG)
        .into_iter()
        .map(|kept| kept.content)
        .collect();
    assert_eq!(words, ["They live in Bristol now."]);
    assert_eq!(recalled(&config, SUPERSEDED_TAG).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn approved_without_its_proposal_the_owner_is_told_it_wasnt_kept() {
    let (config, dir, bus) = broker("lost");
    let key = MasterKey::generate().unwrap();
    let evidence = [format!("run:{RUN}")];
    crate::review_queue::propose(
        &config.scheduler,
        &key,
        &crate::review_queue::Proposal {
            proposed_by: "chat",
            kind: ReviewKind::Remember,
            target: None,
            change: "They like tea.",
            why: "",
            evidence: &evidence,
        },
        hatchling_scheduler::Timestamp::now(),
    )
    .unwrap();
    let item = listed(&config, &key).into_iter().next().unwrap();
    decide(&config, &key, &item, true);
    assert!(recalled(&config, RECALL_TAG).is_empty());
    assert_eq!(
        told(&bus),
        [
            "Approved, but it wasn't kept: what it would keep couldn't be read. The broker's log says why."
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Each memory waits as its own item, with its own proposal sealed beside
/// it: only the same words waiting as a memory count as asked already.
#[test]
fn each_memory_waits_alone_and_is_kept_as_it_was_asked() {
    let (config, dir, _bus) = broker("several");
    let key = MasterKey::generate().unwrap();
    let evidence = [String::from("message:c1/m1")];
    // Another kind of change, in the same words, waits too.
    crate::review_queue::propose(
        &config.scheduler,
        &key,
        &crate::review_queue::Proposal {
            proposed_by: "memory",
            kind: ReviewKind::Replace,
            target: Some("memory:k1"),
            change: "They cycle to work.",
            why: "",
            evidence: &evidence,
        },
        hatchling_scheduler::Timestamp::now(),
    )
    .unwrap();
    for words in ["They live in Bath.", "They cycle to work."] {
        assert_eq!(
            propose_in(
                &config,
                Some(&key),
                "task-h",
                &Asked::about_owner(words),
                in_chat(),
                false
            ),
            MemoryResult::Proposed,
            "{words}"
        );
    }
    let items = listed(&config, &key);
    let remembered: Vec<&str> = items
        .iter()
        .filter(|item| item.kind == ReviewKind::Remember)
        .map(|item| item.change.as_str())
        .collect();
    assert_eq!(remembered, ["They cycle to work.", "They live in Bath."]);
    // The older one, approved, keeps its own words.
    let older = items
        .iter()
        .find(|item| item.change == "They live in Bath.")
        .unwrap();
    decide(&config, &key, older, true);
    let words: Vec<String> = recalled(&config, RECALL_TAG)
        .into_iter()
        .map(|kept| kept.content)
        .collect();
    assert_eq!(words, ["They live in Bath."]);
    let _ = std::fs::remove_dir_all(&dir);
}
