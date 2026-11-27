//! WO-057: a memory kept at once, only when the broker's rule holds.

use std::sync::Arc;

use hatchling_core::MemoryResult;
use hatchling_core::ui::{ChatAuthor, Decision, UiEventBody};
use hatchling_memory::{KeptBy, KeptRecord, MemoryPrincipal, MemoryScope, SessionId, Tag};

use super::{
    DAY_SECS, MAX_AUTOMATIC_PER_DAY, ProposedIn, RECALL_TAG, SUPERSEDED_TAG, open, propose_in,
};
use crate::Config;
use crate::approval_board::board;
use crate::config::{MemoryAction, MemoryGrant};
use crate::memory_mode::Mode;
use crate::memory_proposal::Asked;

/// A broker that may propose memory, with scheduling on, so the owner's
/// mode is kept: Automatic until they choose.
fn broker(name: &str) -> (Config, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "hatchling-memory-automatic-{name}-{}",
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
    (config, dir)
}

/// Where the chat's run asked.
fn in_chat() -> ProposedIn {
    ProposedIn {
        run: Some(String::from("run-0123456789abcdef0123456789abcdef")),
        chat: Some(String::from("local")),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// What's recalled for the owner, with its record.
fn recalled(config: &Config, tag: &str) -> Vec<hatchling_memory::MemoryItem> {
    let principal = MemoryPrincipal {
        peer_uid: config.agent_uid.as_u32(),
        session_id: SessionId::generate().unwrap(),
    };
    crate::memory_sealed::open_published(&config.memory.root)
        .unwrap()
        .recall_by_tag(
            &principal,
            &MemoryScope::User,
            &tag.parse().unwrap(),
            u64::MAX / 2,
        )
        .unwrap()
}

/// What the owner was told in the chat, as the broker's own.
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

/// The id a proposal kept at once was kept as.
fn kept_as(result: &MemoryResult) -> String {
    if let MemoryResult::Kept { id } = result {
        Some(id.clone())
    } else {
        None
    }
    .expect("kept at once")
}

/// Whether a card was put to the owner.
fn carded(bus: &crate::api_stream::Bus) -> bool {
    bus.after(None)
        .into_iter()
        .any(|event| matches!(event.body, UiEventBody::Confirmation(_)))
}

/// Keeps `count` memories for the owner with `record` and the tag `tag`,
/// as if kept before.
fn kept_before(config: &Config, count: usize, record: &KeptRecord, tag: &str) {
    let (principal, store) = open(config).unwrap();
    for at in 0..count {
        store
            .keep(
                &principal,
                &MemoryScope::User,
                &format!("kept-before-{at}").parse().unwrap(),
                &format!("Something said before, number {at}."),
                &[tag.parse::<Tag>().unwrap()],
                record.clone(),
            )
            .unwrap();
    }
}

#[test]
fn a_clean_chat_proposal_is_kept_at_once_and_the_owner_told() {
    let (config, dir) = broker("clean");
    let bus = Arc::new(crate::api_stream::Bus::starting_at(1));
    board(&config).attach(Arc::clone(&bus));
    let result = propose_in(
        &config,
        "task-a",
        &Asked::about_owner("Pepper is a cat."),
        in_chat(),
        true,
    );
    let id = kept_as(&result);
    let kept = recalled(&config, RECALL_TAG);
    assert_eq!(kept.len(), 1, "{kept:?}");
    let item = kept.first().unwrap();
    assert_eq!(item.id.as_str(), id);
    assert_eq!(item.content, "Pepper is a cat.");
    // Its record says it was kept automatically, from which run and chat,
    // with no approval, since none was asked.
    let record = item.record.as_ref().unwrap();
    assert_eq!(record.kept_by, KeptBy::Automatic);
    assert_eq!(record.approval, None);
    assert_eq!(record.run, in_chat().run);
    assert_eq!(record.chat.as_deref(), Some("local"));
    assert!(record.kept_at_secs.abs_diff(now()) < 60);
    assert!(!item.approved_by_owner() && item.kept_automatically());
    assert!(!carded(&bus), "no card");
    assert_eq!(
        told(&bus),
        [
            "Remembered: “Pepper is a cat.” The agent is given it from now on. Kept automatically: nothing in this conversation came from outside. If it's wrong, forget it on the Memory page."
        ]
    );
    // Kept is kept once: asked again, it's said to be.
    assert!(matches!(
        propose_in(&config, "task-a", &Asked::about_owner("Pepper is a cat."), in_chat(), true),
        MemoryResult::Unasked { reason } if reason.starts_with("it's already kept")
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_conversation_that_read_from_outside_or_ask_me_puts_it_on_a_card() {
    let (config, dir) = broker("card");
    let bus = Arc::new(crate::api_stream::Bus::starting_at(1));
    board(&config).attach(Arc::clone(&bus));
    // Not clean: the broker saw the conversation read from outside, or the
    // run isn't the chat's.
    assert_eq!(
        propose_in(
            &config,
            "task-b",
            &Asked::about_owner("They live in Bath."),
            in_chat(),
            false
        ),
        MemoryResult::Proposed
    );
    assert!(carded(&bus));
    // Ask me: every proposal is a card, clean or not.
    crate::memory_mode::set(&config.scheduler, Mode::Ask).unwrap();
    assert_eq!(
        propose_in(
            &config,
            "task-c",
            &Asked::about_owner("They cycle to work."),
            in_chat(),
            true
        ),
        MemoryResult::Proposed
    );
    assert!(recalled(&config, RECALL_TAG).is_empty(), "nothing kept");
    // Without scheduling, there's no mode kept and no chat: a card.
    let mut config = config;
    config.scheduler = crate::SchedulerConfig::disabled();
    assert_eq!(
        propose_in(
            &config,
            "task-d",
            &Asked::about_owner("They like tea."),
            in_chat(),
            true
        ),
        MemoryResult::Proposed
    );
    assert!(recalled(&config, RECALL_TAG).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn past_the_days_cap_it_is_a_card_and_replaced_ones_count() {
    let (config, dir) = broker("cap");
    let recent = KeptRecord::automatic(None, Some(String::from("local")), now() - 60);
    // Kept automatically in the last day, then replaced: still counted, so
    // correcting in a loop can't pass the cap.
    kept_before(&config, MAX_AUTOMATIC_PER_DAY, &recent, SUPERSEDED_TAG);
    assert_eq!(
        propose_in(
            &config,
            "task-e",
            &Asked::about_owner("One too many."),
            in_chat(),
            true
        ),
        MemoryResult::Proposed
    );
    let _ = std::fs::remove_dir_all(&dir);
    // One fewer, recalled: the next is kept.
    let (config, dir) = broker("cap-under");
    kept_before(&config, MAX_AUTOMATIC_PER_DAY - 1, &recent, RECALL_TAG);
    assert!(matches!(
        propose_in(
            &config,
            "task-f",
            &Asked::about_owner("Just under."),
            in_chat(),
            true
        ),
        MemoryResult::Kept { .. }
    ));
    assert_eq!(
        propose_in(
            &config,
            "task-f",
            &Asked::about_owner("Now at it."),
            in_chat(),
            true
        ),
        MemoryResult::Proposed
    );
    let _ = std::fs::remove_dir_all(&dir);
    // What's older than a day, or what the owner approved, isn't counted.
    let (config, dir) = broker("cap-old");
    let old = KeptRecord::automatic(None, None, now() - DAY_SECS - 60);
    kept_before(&config, MAX_AUTOMATIC_PER_DAY, &old, RECALL_TAG);
    let approved = KeptRecord::agent_words(Some(String::from("confirm-1")), None, None, now());
    let (principal, store) = open(&config).unwrap();
    for at in 0..MAX_AUTOMATIC_PER_DAY {
        store
            .keep(
                &principal,
                &MemoryScope::User,
                &format!("kept-owner-{at}").parse().unwrap(),
                &format!("Approved by the owner, number {at}."),
                &[RECALL_TAG.parse::<Tag>().unwrap()],
                approved.clone(),
            )
            .unwrap();
    }
    assert!(matches!(
        propose_in(
            &config,
            "task-g",
            &Asked::about_owner("Not capped."),
            in_chat(),
            true
        ),
        MemoryResult::Kept { .. }
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_the_owner_said_no_to_or_skipped_is_never_kept_at_once() {
    let (config, dir) = broker("denied");
    let bus = Arc::new(crate::api_stream::Bus::starting_at(1));
    board(&config).attach(Arc::clone(&bus));
    let asked = Asked::about_owner("They hate mornings.");
    assert_eq!(
        propose_in(&config, "task-h", &asked, in_chat(), false),
        MemoryResult::Proposed
    );
    let board = board(&config);
    let id = board
        .approvals()
        .find_run("task-h", crate::approvals::State::Pending, |_| true)
        .unwrap();
    let request = board.approvals().get(&id).unwrap();
    board
        .decide(&id, request.revision, &request.digest, Decision::Deny, 1)
        .unwrap();
    // Asked again in a clean conversation: refused, as on a card.
    assert!(matches!(
        propose_in(&config, "task-i", &asked, in_chat(), true),
        MemoryResult::Unasked { reason } if reason == "the owner said no to keeping this"
    ));
    assert!(recalled(&config, RECALL_TAG).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_correction_kept_at_once_retires_what_it_corrects() {
    let (config, dir) = broker("correct");
    let old = kept_as(&propose_in(
        &config,
        "task-j",
        &Asked::about_owner("They live in Bath."),
        in_chat(),
        true,
    ));
    let correcting = Asked {
        supersedes: Some(old.as_str()),
        ..Asked::about_owner("They live in Bristol now.")
    };
    let new = kept_as(&propose_in(&config, "task-j", &correcting, in_chat(), true));
    let recalled_now: Vec<String> = recalled(&config, RECALL_TAG)
        .into_iter()
        .map(|item| item.content)
        .collect();
    assert_eq!(recalled_now, ["They live in Bristol now."]);
    let retired = recalled(&config, SUPERSEDED_TAG);
    assert_eq!(retired.len(), 1);
    assert_eq!(retired.first().unwrap().id.as_str(), old);
    let kept = recalled(&config, RECALL_TAG);
    let kept = kept.first().unwrap();
    assert_eq!(kept.id.as_str(), new);
    assert_eq!(
        kept.record
            .as_ref()
            .unwrap()
            .supersedes
            .as_ref()
            .map(ToString::to_string),
        Some(old)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The day's count takes what was kept after its start, not at it.
#[test]
fn the_days_count_starts_after_its_first_second() {
    let (config, dir) = broker("since");
    kept_before(
        &config,
        1,
        &KeptRecord::automatic(None, None, 1_000),
        RECALL_TAG,
    );
    let (principal, store) = open(&config).unwrap();
    assert_eq!(
        super::kept_automatically_since(&store, &principal, 999, 2_000),
        1
    );
    assert_eq!(
        super::kept_automatically_since(&store, &principal, 1_000, 2_000),
        0
    );
    let _ = std::fs::remove_dir_all(&dir);
}
