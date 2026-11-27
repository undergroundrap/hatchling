use std::str::FromStr as _;

use super::*;
use crate::item::{MemoryScope, RetentionPolicy};
use crate::record::KeptRecord;

fn item(id: &str, content: &str, updated: u64) -> MemoryItem {
    MemoryItem {
        id: MemoryId::from_str(id).unwrap(),
        scope: MemoryScope::User,
        content: content.to_owned(),
        tags: vec![Tag::from_str("recall").unwrap()],
        created_at_secs: 1,
        updated_at_secs: updated,
        retention: RetentionPolicy::Keep,
        record: None,
    }
}

#[test]
fn a_title_is_the_first_line_cut_at_a_word() {
    assert_eq!(title_of("First line\nsecond"), "First line");
    assert_eq!(title_of("  padded  "), "padded");
    assert_eq!(title_of(""), "");
    let words = "word ".repeat(20);
    assert_eq!(
        title_of(&words),
        format!("{}…", "word ".repeat(12).trim_end())
    );
    let one = "x".repeat(80);
    assert_eq!(title_of(&one), format!("{}…", "x".repeat(60)));
    let exact = "y".repeat(60);
    assert_eq!(title_of(&exact), exact);
    // Cut mid-word, at the last space before it.
    let mid = format!("{}longwordlongword more", "word ".repeat(11));
    assert_eq!(
        title_of(&mid),
        format!("{}…", "word ".repeat(11).trim_end())
    );
    // A last space at 20 characters or before cuts at 60 instead: a word
    // cut there would leave too little.
    let early = format!("{} {}", "x".repeat(20), "y".repeat(50));
    assert_eq!(
        title_of(&early),
        format!("{} {}…", "x".repeat(20), "y".repeat(39))
    );
    let later = format!("{} {}", "x".repeat(30), "y".repeat(40));
    assert_eq!(title_of(&later), format!("{}…", "x".repeat(30)));
    // Characters, not bytes: an accented title of 60 isn't cut.
    let accented = "é".repeat(60);
    assert_eq!(title_of(&accented), accented);
}

#[test]
fn terms_are_the_words_found_by_lowercased_once_each_without_common_ones() {
    let mut kept = item("kept-1", "They live in Leeds, and LEEDS is rainy.", 1);
    kept.record = Some(KeptRecord {
        why: Some(String::from("Umbrellas matter.")),
        how_to_apply: Some(String::from("Weather questions.")),
        ..KeptRecord::agent_words(None, None, None, 1)
    });
    assert_eq!(
        terms_of(&kept),
        [
            "live",
            "leeds",
            "rainy",
            "umbrellas",
            "matter",
            "weather",
            "questions"
        ]
    );
    let many = (0..40).fold(String::new(), |mut words, n| {
        use std::fmt::Write as _;
        let _ = write!(words, "word{n:02} ");
        words
    });
    let terms = terms_of(&item("kept-2", &many, 1));
    assert_eq!(terms.len(), MAX_TERMS);
    assert_eq!(terms.last().map(String::as_str), Some("word31"));
    // Two-letter words and the commonest aren't kept.
    assert!(terms_of(&item("kept-3", "it is the and of", 1)).is_empty());
}

#[test]
fn an_entry_says_what_its_index_line_and_choosing_need() {
    let mut kept = item("kept-1", "They have no car.", 7);
    kept.record = Some(KeptRecord {
        kind: Some(MemoryKind::Owner),
        absence: true,
        ..KeptRecord::agent_words(None, None, None, 5)
    });
    let entry = RecallEntry::of(&kept);
    assert_eq!(entry.id.as_str(), "kept-1");
    assert_eq!(
        (entry.kind, entry.absence, entry.approved),
        (Some(MemoryKind::Owner), true, true)
    );
    assert_eq!((entry.kept_at_secs, entry.updated_at_secs), (5, 7));
    assert_eq!(entry.title, "They have no car.");
    assert_eq!(entry.body_bytes, "They have no car.".len());
    let written = RecallEntry::of(&item("note", "A run's note.", 3));
    assert_eq!(
        (written.approved, written.kept_at_secs, written.kind),
        (false, 1, None)
    );
    // It reads back as it was written.
    let json = serde_json::to_string(&entry).unwrap();
    assert_eq!(serde_json::from_str::<RecallEntry>(&json).unwrap(), entry);
}

#[test]
fn an_entry_is_recalled_by_its_tag_until_it_expires() {
    let recall = Tag::from_str("recall").unwrap();
    let other = Tag::from_str("other").unwrap();
    let mut expiring = item("kept-1", "x", 1);
    expiring.retention = RetentionPolicy::Ttl { ttl_secs: 100 };
    let entry = RecallEntry::of(&expiring);
    assert!(entry.recalled_by(&recall, 100));
    assert!(!entry.recalled_by(&recall, 101), "past its deadline");
    assert!(!entry.recalled_by(&other, 50));
    assert!(RecallEntry::of(&item("kept-2", "x", 1)).recalled_by(&recall, u64::MAX));
    assert!(still_given(&expiring, 100));
    assert!(!still_given(&expiring, 101));
}

#[test]
fn choosing_puts_guidance_first_then_the_newest_within_each_budget() {
    let mut rule = item("kept-r", "Use metric units.", 1);
    rule.record = Some(KeptRecord {
        kind: Some(MemoryKind::Guidance),
        ..KeptRecord::agent_words(None, None, None, 1)
    });
    let entries: Vec<RecallEntry> = [
        item("kept-a", "Older fact.", 10),
        item("kept-b", "Newer fact.", 20),
        rule,
        item("kept-c", &"z".repeat(300), 30),
        item("kept-d", "Same time as b.", 20),
    ]
    .iter()
    .map(RecallEntry::of)
    .collect();
    let ids = |positions: &[usize]| -> Vec<&str> {
        positions
            .iter()
            .filter_map(|&at| entries.get(at))
            .map(|entry| entry.id.as_str())
            .collect()
    };
    let all = choose(&entries, &[], 10_000, 10_000);
    assert_eq!(
        ids(&all.index),
        ["kept-r", "kept-c", "kept-b", "kept-d", "kept-a"]
    );
    assert_eq!(ids(&all.whole), ids(&all.index));
    assert_eq!(all.left_out, 0);
    // The index's budget: lines while they fit, the rest counted.
    let first_two: usize = all
        .index
        .iter()
        .take(2)
        .filter_map(|&at| entries.get(at))
        .map(|entry| crate::prompt::index_line(entry).len() + 1)
        .sum();
    let cut = choose(&entries, &[], first_two, 0);
    assert_eq!(ids(&cut.index), ["kept-r", "kept-c"]);
    assert_eq!(cut.left_out, 3);
    assert!(cut.whole.is_empty());
    let short = choose(&entries, &[], first_two - 1, 0);
    assert_eq!(ids(&short.index), ["kept-r"]);
    // Whole items: a large one is skipped for smaller ones after it.
    let room = "Use metric units.".len() + "Newer fact.".len();
    assert_eq!(
        ids(&choose(&entries, &[], 0, room).whole),
        ["kept-r", "kept-b"]
    );
    assert_eq!(ids(&choose(&entries, &[], 0, room - 1).whole), ["kept-r"]);
    assert!(choose(&[], &[], 100, 100).index.is_empty());
}

#[test]
fn an_ask_is_matched_by_its_words() {
    assert_eq!(
        query_terms("When is the DENTIST, and the dentist's address?"),
        ["when", "dentist", "address"]
    );
    assert!(query_terms("it is").is_empty());
}

/// BM25: an entry holding an asked word scores above one that doesn't,
/// a rare word above a common one, and guidance or work whose *how to
/// apply* names the word above one that only mentions it.
#[test]
fn entries_are_scored_by_how_well_they_match() {
    let entry = |id: &str, content: &str| RecallEntry::of(&item(id, content, 1));
    let entries = [
        entry("kept-a", "The dentist is Dr Lee on Mill Road."),
        entry("kept-b", "They walk on Mill Road."),
        entry("kept-c", "They like tea."),
    ];
    let score = scores(&entries, &query_terms("dentist"));
    assert!(
        matches!(score.as_slice(), [a, b, c] if *a > 0.0 && *b <= 0.0 && *c <= 0.0),
        "{score:?}"
    );
    let road = scores(&entries, &query_terms("mill road dentist"));
    assert!(
        matches!(road.as_slice(), [a, b, c] if a > b && b > c),
        "{road:?}"
    );
    // The rarer word weighs more.
    let first = |query: &str| {
        scores(&entries, &query_terms(query))
            .first()
            .copied()
            .unwrap_or(0.0)
    };
    let (rare, common) = (first("dentist"), first("road"));
    assert!(rare > common, "{rare} {common}");
    assert!(
        scores(&entries, &[])
            .iter()
            .all(|score| score.abs() < f64::EPSILON)
    );
    assert!(scores(&[], &query_terms("dentist")).is_empty());
    // When it applies counts for guidance and work, not for the rest.
    let applied = |kind| {
        let mut rule = item("kept-r", "Keep answers short.", 1);
        rule.record = Some(KeptRecord {
            kind: Some(kind),
            how_to_apply: Some(String::from("Any answer about the dentist.")),
            ..KeptRecord::agent_words(None, None, None, 1)
        });
        RecallEntry::of(&rule)
    };
    for kind in [MemoryKind::Guidance, MemoryKind::Work] {
        let rule = applied(kind);
        assert_eq!(rule.apply_terms, ["answer", "about", "dentist"]);
        let [dentist, ..] = &entries;
        let with = scores(&[rule.clone(), dentist.clone()], &query_terms("dentist"));
        let as_fact = RecallEntry {
            kind: Some(MemoryKind::Owner),
            ..rule
        };
        let without = scores(&[as_fact, dentist.clone()], &query_terms("dentist"));
        assert!(
            with.first() > without.first(),
            "{kind:?}: {with:?} {without:?}"
        );
    }
}

/// With an ask, the best match comes before what's newer; guidance still
/// comes first.
#[test]
fn choosing_ranks_by_the_ask_after_guidance() {
    let mut rule = item("kept-r", "Use metric units.", 1);
    rule.record = Some(KeptRecord {
        kind: Some(MemoryKind::Guidance),
        ..KeptRecord::agent_words(None, None, None, 1)
    });
    let entries: Vec<RecallEntry> = [
        item("kept-old", "The dentist is Dr Lee.", 1),
        item("kept-new", "They like tea.", 50),
        rule,
    ]
    .iter()
    .map(RecallEntry::of)
    .collect();
    let ids = |positions: &[usize]| -> Vec<&str> {
        positions
            .iter()
            .filter_map(|&at| entries.get(at))
            .map(|entry| entry.id.as_str())
            .collect()
    };
    let asked = choose(&entries, &query_terms("the dentist"), 10_000, 10_000);
    assert_eq!(ids(&asked.index), ["kept-r", "kept-old", "kept-new"]);
    let unasked = choose(&entries, &[], 10_000, 10_000);
    assert_eq!(ids(&unasked.index), ["kept-r", "kept-new", "kept-old"]);
}
