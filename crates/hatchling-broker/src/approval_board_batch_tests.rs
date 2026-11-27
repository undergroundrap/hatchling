//! WO-055: a task's memory proposals gather on one card, as revisions, and
//! never quietly.

use std::sync::Arc;

use hatchling_core::ui::{ConfirmationRequest, Decision, DecisionRejection, UiEventBody};

use super::{Approved, Board, CardRefusal, MAX_BATCH};
use crate::api_stream::Bus;
use crate::approvals::{Approvals, Refusal, State, UNWAITED_DECIDE_SECS};
use crate::memory_keep::ProposedIn;
use crate::memory_proposal::{Checked, Replaced};

fn watched() -> (Board, Arc<Bus>) {
    let board = Board {
        approvals: Approvals::new(),
        bus: std::sync::Mutex::new(None),
        queue: std::sync::Mutex::new(std::collections::VecDeque::new()),
        wake: std::sync::Condvar::new(),
        cards: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        decisions: std::sync::Mutex::new(None),
    };
    let bus = Arc::new(Bus::starting_at(1));
    board.attach(Arc::clone(&bus));
    (board, bus)
}

fn cards(bus: &Bus) -> Vec<ConfirmationRequest> {
    bus.after(None)
        .into_iter()
        .filter_map(|event| {
            if let UiEventBody::Confirmation(card) = event.body {
                Some(card)
            } else {
                None
            }
        })
        .collect()
}

fn ask(
    board: &Board,
    words: &str,
    task: &str,
    now: i64,
) -> Result<crate::approvals::Requested, CardRefusal> {
    board.propose_memory(&Checked::owner(words), task, ProposedIn::default(), now)
}

fn correcting(words: &str, id: &str) -> Checked {
    let mut checked = Checked::owner(words);
    checked.supersedes = Some(Replaced {
        id: id.to_owned(),
        content: String::from("what it was"),
    });
    checked
}

/// The memory card `run` holds, if it holds one.
const fn memory_card_of(run: &Approved) -> Option<&super::MemoryKeep> {
    if let Approved::Memory(card) = run {
        Some(card)
    } else {
        None
    }
}

/// The words on a card's items, in order.
fn words(card: &ConfirmationRequest) -> Vec<&str> {
    card.items
        .iter()
        .map(|item| {
            item.fields
                .iter()
                .find(|field| field.label == "What it would remember")
                .map(|field| field.value.as_str())
                .unwrap()
        })
        .collect()
}

/// Three asks from one task are one card, grown a revision at a time, each
/// shown; a decision on what was shown before is refused, and approving
/// the card as it stands runs all three.
#[test]
fn a_tasks_memories_gather_on_one_card_shown_at_each_revision() {
    let (board, bus) = watched();
    let first = ask(&board, "Pepper is a cat.", "t1", 100).unwrap();
    let second = ask(&board, "Use metric.", "t1", 101).unwrap();
    let third = ask(&board, "No car.", "t1", 102).unwrap();
    assert_eq!(second.id, first.id);
    assert_eq!(third.id, first.id);
    assert_eq!([first.revision, second.revision, third.revision], [1, 2, 3]);
    assert_eq!(
        third.expires_at,
        100 + UNWAITED_DECIDE_SECS,
        "the first's deadline"
    );
    let shown = cards(&bus);
    assert_eq!(shown.len(), 3, "each revision shown");
    // One memory is a card as it always was.
    let one = shown.first().unwrap();
    assert!(one.items.is_empty());
    assert_eq!(one.summary, "Remember this about you?");
    assert_eq!(one.approve.as_deref(), Some("Remember it"));
    // Several are a batch: each its own fields, the agent's words labeled.
    let three = shown.last().unwrap();
    assert_eq!(
        (three.confirmation_id.as_str(), three.revision),
        (first.id.as_str(), 3)
    );
    assert_eq!(three.summary, "Remember these 3 things?");
    assert_eq!(three.approve.as_deref(), Some("Remember them"));
    assert_eq!(words(three), ["Pepper is a cat.", "Use metric.", "No car."]);
    let item = three.items.first().unwrap();
    assert_eq!(
        item.fields
            .iter()
            .map(|field| (field.label.as_str(), field.source))
            .collect::<Vec<_>>(),
        [
            (
                "What it would remember",
                hatchling_core::ui::FieldSource::Agent
            ),
            ("What it is", hatchling_core::ui::FieldSource::Broker),
        ]
    );
    assert_eq!(three.fields.len(), 1, "the operation, the broker's");
    assert_eq!(three.digest, third.digest);
    assert_eq!(
        three.digest,
        hatchling_core::action::digest(&serde_json::json!({
            "format": 3, "scope": "user", "op": "memory.keep",
            "items": [
                { "op": "memory.keep", "content": "Pepper is a cat.", "kind": "owner" },
                { "op": "memory.keep", "content": "Use metric.", "kind": "owner" },
                { "op": "memory.keep", "content": "No car.", "kind": "owner" },
            ]
        }))
        .unwrap()
    );
    // What the owner saw at revision 2 can't be approved now.
    assert_eq!(
        board.decide(&first.id, 2, &second.digest, Decision::Approve, 103),
        Err(DecisionRejection::StaleRevision)
    );
    assert_eq!(
        board.decide(&first.id, 3, &third.digest, Decision::Approve, 104),
        Ok(State::Approved)
    );
    let (run, claim) = board.approvals().claim(&first.id).unwrap();
    let card = memory_card_of(&run).unwrap();
    assert_eq!(
        card.items()
            .map(|item| item.content.as_str())
            .collect::<Vec<_>>(),
        ["Pepper is a cat.", "Use metric.", "No car."]
    );
    claim.succeeded();
    // Decided, the task's next memory starts a card of its own.
    let next = ask(&board, "Likes tea.", "t1", 105).unwrap();
    assert_ne!(next.id, first.id);
    assert_eq!(next.revision, 1);
}

/// The same words again are the memory already on the card: nothing new is
/// shown.
#[test]
fn the_same_words_again_are_the_memory_already_waiting() {
    let (board, bus) = watched();
    let first = ask(&board, "Pepper is a cat.", "t1", 100).unwrap();
    ask(&board, "Use metric.", "t1", 100).unwrap();
    let again = ask(&board, "Pepper is a cat.", "t1", 101).unwrap();
    assert!(!again.new);
    assert_eq!((again.id.as_str(), again.revision), (first.id.as_str(), 2));
    assert_eq!(cards(&bus).len(), 2, "not shown again");
}

/// A card holds five: a sixth is refused, and the card stays as it was.
#[test]
fn a_card_holds_five_and_no_more() {
    let (board, _bus) = watched();
    let mut id = String::new();
    for n in 0..MAX_BATCH {
        id = ask(&board, &format!("Fact {n}."), "t1", 100).unwrap().id;
    }
    assert_eq!(ask(&board, "Fact 5.", "t1", 100), Err(CardRefusal::Full));
    let held = board.approvals().get(&id).unwrap();
    assert_eq!(held.revision, u64::try_from(MAX_BATCH).unwrap());
    let card = memory_card_of(held.run()).unwrap();
    assert_eq!(card.items().count(), MAX_BATCH);
}

/// Two memories on one card can't correct the same one; corrections of
/// different ones can.
#[test]
fn one_card_corrects_a_memory_once() {
    let (board, _bus) = watched();
    let first = board
        .propose_memory(
            &correcting("Pepper is a cat.", "kept-1"),
            "t1",
            ProposedIn::default(),
            100,
        )
        .unwrap();
    assert_eq!(
        board.propose_memory(
            &correcting("Pepper is a hamster.", "kept-1"),
            "t1",
            ProposedIn::default(),
            101
        ),
        Err(CardRefusal::CorrectsTheSame)
    );
    let other = board
        .propose_memory(
            &correcting("Lives in Madrid.", "kept-2"),
            "t1",
            ProposedIn::default(),
            102,
        )
        .unwrap();
    assert_eq!((other.id.as_str(), other.revision), (first.id.as_str(), 2));
    // One that corrects nothing joins beside them.
    assert_eq!(ask(&board, "Use metric.", "t1", 103).unwrap().revision, 3);
}

/// A denied card refuses each memory on it, alone or on another card, from
/// any task; each task's memories are its own card; and a task's other
/// requests still wait one at a time.
#[test]
fn a_denied_card_refuses_each_memory_and_cards_are_per_task() {
    let (board, _bus) = watched();
    let first = ask(&board, "Pepper is a cat.", "t1", 100).unwrap();
    let card = ask(&board, "Use metric.", "t1", 100).unwrap();
    let theirs = ask(&board, "Use metric.", "t2", 100).unwrap();
    assert_ne!(theirs.id, first.id, "another task's card");
    assert_eq!(
        board.decide(&first.id, card.revision, &card.digest, Decision::Deny, 101),
        Ok(State::Denied)
    );
    assert_eq!(
        ask(&board, "Pepper is a cat.", "t3", 102),
        Err(CardRefusal::Ledger(Refusal::Denied))
    );
    // On t2's waiting card too.
    assert_eq!(
        board.approvals().get(&theirs.id).unwrap().state,
        State::Pending
    );
    assert_eq!(
        ask(&board, "Pepper is a cat.", "t2", 103),
        Err(CardRefusal::Ledger(Refusal::Denied))
    );
    let site = board.propose_site(
        &super::SiteAsk {
            site: "https://a.example",
            url: "https://a.example/x",
            reason: "to read it",
            led: None,
            standing: false,
            visit: false,
            warning: None,
        },
        "t2",
        104,
    );
    assert_eq!(
        site.map(|r| r.id),
        Err(Refusal::QueueFull),
        "one request per task otherwise"
    );
}

/// A decided batch is recorded whole: each memory's fields named by its
/// place.
#[test]
fn a_decided_batch_is_recorded_whole() {
    let (board, _bus) = watched();
    let dir = std::env::temp_dir().join(format!("hatchling-batch-record-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("decisions");
    board.keep_decisions(path.clone());
    ask(&board, "Pepper is a cat.", "t1", 100).unwrap();
    let card = ask(&board, "Use metric.", "t1", 100).unwrap();
    board
        .decide(
            &card.id,
            card.revision,
            &card.digest,
            Decision::Approve,
            101,
        )
        .unwrap();
    let records = crate::decisions::list(&path);
    let record = records.first().unwrap();
    assert_eq!(record.summary, "Remember these 2 things?");
    let labeled: Vec<(&str, &str)> = record
        .fields
        .iter()
        .map(|field| (field.label.as_str(), field.value.as_str()))
        .collect();
    assert!(
        labeled.contains(&("Memory 1: What it would remember", "Pepper is a cat.")),
        "{labeled:?}"
    );
    assert!(
        labeled.contains(&("Memory 2: What it would remember", "Use metric.")),
        "{labeled:?}"
    );
    assert!(
        labeled.contains(&("Memory 2: What it is", "Something about you.")),
        "{labeled:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
