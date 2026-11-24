use std::str::FromStr as _;

use hatchling_goals::{
    FeedBody, FeedQuery, FeedReader, GoalChange, GoalEdit, GoalOutcome, GoalsStore, SourceLabel,
    StatusFilter, TimeRange,
};
use hatchling_memory::{MemoryPrincipal, SessionId};
use hatchling_vault::MasterKey;

use super::*;

/// Words no test writes anywhere else: what must never be on disk.
const WORDS: &str = "the owner's marmalade-coloured canoe";

fn principal(uid: u32) -> MemoryPrincipal {
    MemoryPrincipal {
        peer_uid: uid,
        session_id: SessionId::from_str("session-1").unwrap(),
    }
}

fn label(text: &str) -> SourceLabel {
    text.parse().unwrap()
}

/// The goals root of a test, and a key to seal it under.
fn sealed(name: &str) -> (crate::api_auth::tests::Scratch, MasterKey, PathBuf) {
    let scratch = crate::api_auth::tests::scratch(&format!("goals-sealed-{name}"));
    let root = scratch.0.join("goals");
    (scratch, MasterKey::generate().unwrap(), root)
}

/// The sealed store at `root`, its segments `per` entries long.
fn store_with(root: &Path, key: &MasterKey, per: u64) -> GoalsStore {
    let backing = SealedGoals {
        per_segment: per,
        ..SealedGoals::new(root, key).unwrap()
    };
    GoalsStore::with_backing(root, Box::new(backing)).unwrap()
}

/// Every byte of every file under `dir`, concatenated.
fn every_byte(dir: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                dirs.push(entry.path());
            } else {
                bytes.extend(std::fs::read(entry.path()).unwrap());
            }
        }
    }
    bytes
}

fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Each entry's sequence number and text, from the feed.
fn tracked(reader: &FeedReader, uid: u32) -> Vec<(u64, String)> {
    let query = FeedQuery::Tracking {
        range: TimeRange::ALL,
        source: None,
        after_seq: None,
    };
    reader
        .query(&principal(uid), &query, 0)
        .unwrap()
        .into_iter()
        .map(|item| match item.body() {
            FeedBody::Tracking(entry) => (entry.seq, entry.text.clone()),
            FeedBody::Goal(_) | FeedBody::Memory(_) => unreachable!("a tracking query"),
        })
        .collect()
}

/// Each goal's title and whether it's open, from the feed, by id.
fn goals(reader: &FeedReader, uid: u32) -> Vec<(String, bool)> {
    let query = FeedQuery::Goals {
        status: StatusFilter::Any,
        updated_since_secs: None,
        after: None,
    };
    reader
        .query(&principal(uid), &query, 0)
        .unwrap()
        .into_iter()
        .map(|item| match item.body() {
            FeedBody::Goal(goal) => (goal.title.clone(), goal.is_open()),
            FeedBody::Tracking(_) | FeedBody::Memory(_) => unreachable!("a goals query"),
        })
        .collect()
}

fn reader(root: &Path, key: &MasterKey) -> FeedReader {
    FeedReader::with_source(source(root, key).unwrap(), None)
}

#[test]
fn goals_and_tracking_read_and_write_as_before_and_no_word_is_on_the_disk() {
    let (_scratch, key, root) = sealed("round");
    let store = open(&root, &key).unwrap();
    let me = principal(4242);
    let kept = store
        .create_goal(&me, &format!("{WORDS} goal"), WORDS, 10)
        .unwrap();
    let other = store.create_goal(&me, "learn chess", "", 11).unwrap();
    let GoalChange::Changed(_) = store
        .update_goal(
            &me,
            &kept.id,
            &GoalEdit {
                title: Some(&format!("{WORDS} edited")),
                detail: None,
            },
            12,
        )
        .unwrap()
    else {
        unreachable!("the goal is open");
    };
    let GoalChange::Changed(_) = store
        .close_goal(&me, &other.id, GoalOutcome::Abandoned, 13)
        .unwrap()
    else {
        unreachable!("the goal is open");
    };
    assert_eq!(
        store
            .close_goal(&me, &other.id, GoalOutcome::Abandoned, 14)
            .unwrap(),
        GoalChange::AlreadyClosed
    );
    for n in 0..3 {
        let entry = store
            .append_tracking(&me, &label("user"), &format!("{WORDS} {n}"), 20 + n)
            .unwrap();
        assert_eq!(entry.seq, n);
    }
    let reader = reader(&root, &key);
    let mut titles = goals(&reader, 4242);
    titles.sort();
    assert_eq!(
        titles,
        [
            (format!("{WORDS} edited"), true),
            (String::from("learn chess"), false)
        ]
    );
    assert_eq!(
        tracked(&reader, 4242),
        (0..3)
            .map(|n| (n, format!("{WORDS} {n}")))
            .collect::<Vec<_>>()
    );
    // Another user's are kept apart.
    assert!(goals(&reader, 4243).is_empty());
    assert!(tracked(&reader, 4243).is_empty());
    let disk = every_byte(&root);
    assert!(!holds(&disk, WORDS.as_bytes()), "no word on the disk");
    assert!(!holds(&disk, b"learn chess"));
    assert!(!holds(&disk, b"\"user\""), "nor where they came from");
}

#[test]
fn a_log_runs_across_its_segments_in_order() {
    let (_scratch, key, root) = sealed("segments");
    let store = store_with(&root, &key, 3);
    let me = principal(7);
    for n in 0..8 {
        let entry = store
            .append_tracking(&me, &label("user"), &format!("entry {n}"), n)
            .unwrap();
        assert_eq!(entry.seq, n);
    }
    let backing = SealedGoals {
        per_segment: 3,
        ..SealedGoals::new(&root, &key).unwrap()
    };
    let read: Vec<u64> = GoalsSource::tracking(&backing, 7)
        .unwrap()
        .iter()
        .map(|line| {
            serde_json::from_slice::<hatchling_goals::TrackingEntry>(line)
                .unwrap()
                .seq
        })
        .collect();
    assert_eq!(read, (0..8).collect::<Vec<_>>());
    let vault = backing.vault().unwrap();
    assert_eq!(SealedGoals::count_locked(&vault, 7).unwrap(), 8);
    for (segment, len) in [(0, 3), (1, 3), (2, 2)] {
        assert_eq!(
            SealedGoals::segment_locked(&vault, 7, segment)
                .unwrap()
                .len(),
            len
        );
    }
    assert!(
        SealedGoals::segment_locked(&vault, 7, 3)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_entry_a_crash_left_past_the_head_is_never_read_and_written_over() {
    let (_scratch, key, root) = sealed("crash");
    let store = store_with(&root, &key, 3);
    let me = principal(7);
    for n in 0..2 {
        store
            .append_tracking(&me, &label("user"), &format!("entry {n}"), n)
            .unwrap();
    }
    // A crash between a segment and its head: the segment holds a third
    // entry the head doesn't count.
    let backing = SealedGoals {
        per_segment: 3,
        ..SealedGoals::new(&root, &key).unwrap()
    };
    let vault = backing.vault().unwrap();
    let mut left = SealedGoals::segment_locked(&vault, 7, 0).unwrap();
    left.push(br#"{"seq":2,"at_secs":9,"source":"user","text":"lost"}"#.to_vec());
    sealed_blobs::write(&vault, &segment_key(7, 0), &bytes_of(&left)).unwrap();
    let reader = FeedReader::with_source(
        Box::new(SealedGoals {
            per_segment: 3,
            ..SealedGoals::new(&root, &key).unwrap()
        }),
        None,
    );
    assert_eq!(tracked(&reader, 7).len(), 2, "never counted, never read");
    let entry = store
        .append_tracking(&me, &label("user"), "kept", 5)
        .unwrap();
    assert_eq!(entry.seq, 2);
    assert_eq!(
        tracked(&reader, 7).last().map(|(_, text)| text.as_str()),
        Some("kept")
    );
    assert_eq!(SealedGoals::segment_locked(&vault, 7, 0).unwrap().len(), 3);
}

#[test]
fn a_log_missing_a_segment_or_sealed_under_another_key_is_refused() {
    let (_scratch, key, root) = sealed("broken");
    let store = store_with(&root, &key, 3);
    let me = principal(7);
    for n in 0..5 {
        store
            .append_tracking(&me, &label("user"), &format!("entry {n}"), n)
            .unwrap();
    }
    let goal = store.create_goal(&me, "run a 5k", "", 1).unwrap();
    // Opened with another key, nothing reads.
    let other = MasterKey::generate().unwrap();
    let stranger = SealedGoals::new(&root, &other).unwrap();
    assert!(GoalsSource::tracking(&stranger, 7).is_err());
    assert!(GoalsSource::goal(&stranger, 7, &goal.id).is_err());
    // A goal under another goal's key doesn't open as it.
    let backing = SealedGoals {
        per_segment: 3,
        ..SealedGoals::new(&root, &key).unwrap()
    };
    let vault = backing.vault().unwrap();
    let elsewhere: GoalId = "g-elsewhere".parse().unwrap();
    let copy = |from: &str, to: &str| {
        for (from, to) in [
            (format!("h{from}"), format!("h{to}")),
            (
                format!("d{from}000000000000000100"),
                format!("d{to}000000000000000100"),
            ),
        ] {
            let _ = std::fs::copy(sealed_root(&root).join(from), sealed_root(&root).join(to));
        }
    };
    copy(&goal_key(7, &goal.id), &goal_key(7, &elsewhere));
    assert!(matches!(
        GoalsSource::goal(&backing, 7, &elsewhere),
        Err(hatchling_goals::Error::Corrupt | hatchling_goals::Error::Io { .. })
    ));
    // The second segment gone: the head counts what isn't there.
    vault
        .remove(&sealed_blobs_head(&segment_key(7, 1)))
        .unwrap();
    assert!(matches!(
        GoalsSource::tracking(&backing, 7),
        Err(hatchling_goals::Error::Corrupt)
    ));
    // Nor is anything appended to it.
    assert!(matches!(
        store.append_tracking(&me, &label("user"), "more", 9),
        Err(hatchling_goals::Error::Corrupt)
    ));
}

/// The vault name of a sealed value's head.
fn sealed_blobs_head(key: &str) -> hatchling_vault::SecretName {
    format!("h{key}").parse().unwrap()
}

#[test]
fn every_key_is_a_name_of_its_own() {
    let a: GoalId = "g-a".parse().unwrap();
    let b: GoalId = "g-b".parse().unwrap();
    let keys = [
        goal_key(7, &a),
        goal_key(7, &b),
        goal_key(8, &a),
        index_key(7),
        index_key(8),
        head_key(7),
        head_key(8),
        segment_key(7, 0),
        segment_key(7, 1),
        segment_key(8, 0),
    ];
    for key in &keys {
        assert_eq!(key.len(), 40, "{key}");
        assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit()), "{key}");
    }
    let distinct: BTreeSet<&String> = keys.iter().collect();
    assert_eq!(distinct.len(), keys.len());
    assert_eq!(head_key(7), sealed_blobs::key_of(&[b"tracking", b"7"]));
    assert_eq!(
        segment_key(7, 12),
        sealed_blobs::key_of(&[b"segment", b"7", b"12"])
    );
}

#[test]
fn a_segment_reads_back_as_its_entries() {
    let entries = [b"a".to_vec(), b"bc".to_vec(), Vec::new()];
    assert_eq!(bytes_of(&entries), b"a\nbc\n\n");
    assert_eq!(entries_of(&bytes_of(&entries)), entries);
    assert!(entries_of(b"").is_empty());
}

/// What 0.9 kept for user 4242: two goals, one closed, and three entries.
fn plaintext(root: &Path) -> Vec<String> {
    let files = GoalsStore::open(root).unwrap();
    let me = principal(4242);
    let one = files
        .create_goal(&me, &format!("{WORDS} goal"), "", 1)
        .unwrap();
    let two = files.create_goal(&me, "learn chess", "", 2).unwrap();
    files
        .close_goal(&me, &two.id, GoalOutcome::Achieved, 3)
        .unwrap();
    for n in 0..3 {
        files
            .append_tracking(&me, &label("user"), &format!("{WORDS} {n}"), n)
            .unwrap();
    }
    vec![one.id.to_string(), two.id.to_string()]
}

#[test]
fn plaintext_goals_and_tracking_move_into_the_sealed_store_whole() {
    let (_scratch, key, root) = sealed("migrate");
    plaintext(&root);
    assert_eq!(
        migrate(&root, &key).unwrap(),
        Migrated {
            goals: 2,
            entries: 3,
            left: 0
        }
    );
    assert!(!root.join("goals").exists() && !root.join("tracking").exists());
    let reader = reader(&root, &key);
    let mut titles = goals(&reader, 4242);
    titles.sort();
    assert_eq!(
        titles,
        [
            (format!("{WORDS} goal"), true),
            (String::from("learn chess"), false)
        ]
    );
    assert_eq!(tracked(&reader, 4242).len(), 3);
    assert!(!holds(&every_byte(&root), WORDS.as_bytes()));
    // Run again, nothing more is moved.
    assert_eq!(migrate(&root, &key).unwrap(), Migrated::default());
    // And what's sealed carries on.
    let store = open(&root, &key).unwrap();
    let entry = store
        .append_tracking(&principal(4242), &label("user"), "next", 9)
        .unwrap();
    assert_eq!(entry.seq, 3);
}

#[test]
fn a_migration_cut_short_finishes_when_run_again_and_doubles_nothing() {
    let (_scratch, key, root) = sealed("migrate-cut");
    let ids = plaintext(&root);
    // Cut short after a goal was sealed and the log imported, before
    // either file was removed.
    let files = hatchling_goals::Files::of_this_user(&root);
    let backing = SealedGoals::new(&root, &key).unwrap();
    let first: GoalId = ids.first().unwrap().parse().unwrap();
    let bytes = GoalsSource::goal(&files, 4242, &first).unwrap().unwrap();
    backing.put_goal(4242, &first, &bytes).unwrap();
    let lines = GoalsSource::tracking(&files, 4242).unwrap();
    backing.import_locked(4242, &lines).unwrap();
    assert_eq!(
        migrate(&root, &key).unwrap(),
        Migrated {
            goals: 2,
            entries: 3,
            left: 0
        }
    );
    let feed = reader(&root, &key);
    assert_eq!(goals(&feed, 4242).len(), 2, "once, not twice");
    assert_eq!(tracked(&feed, 4242).len(), 3, "once, not twice");
    // An import cut short before its head counts nothing, and is
    // written over.
    let (_scratch, key, root) = sealed("migrate-head");
    plaintext(&root);
    let backing = SealedGoals::new(&root, &key).unwrap();
    let vault = backing.vault().unwrap();
    sealed_blobs::write(&vault, &segment_key(4242, 0), b"half\n").unwrap();
    assert_eq!(migrate(&root, &key).unwrap().entries, 3);
    assert_eq!(tracked(&reader(&root, &key), 4242).len(), 3);
}

#[test]
fn what_isnt_a_goal_or_a_whole_log_is_left_where_it_is() {
    let (_scratch, key, root) = sealed("migrate-odd");
    let ids = plaintext(&root);
    let dir = root.join("goals").join("4242");
    // A goal file naming another goal, and one named for no goal.
    std::fs::copy(
        dir.join(format!("{}.json", ids.first().unwrap())),
        dir.join("g-copied.json"),
    )
    .unwrap();
    std::fs::write(dir.join("bad name.json"), b"{}").unwrap();
    // Another user's log with a gap in it.
    let theirs = root.join("tracking").join("4243");
    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::set_permissions(
        &theirs,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(
        theirs.join("log.ndjson"),
        b"{\"seq\":0,\"at_secs\":1,\"source\":\"user\",\"text\":\"a\"}\n{\"seq\":2,\"at_secs\":1,\"source\":\"user\",\"text\":\"b\"}\n",
    )
    .unwrap();
    assert_eq!(
        migrate(&root, &key).unwrap(),
        Migrated {
            goals: 2,
            entries: 3,
            left: 3
        }
    );
    assert!(dir.join("g-copied.json").exists() && dir.join("bad name.json").exists());
    assert!(theirs.join("log.ndjson").exists());
    assert!(tracked(&reader(&root, &key), 4243).is_empty());
}

#[test]
fn a_log_whose_sealed_copy_differs_is_left_where_it_is() {
    let (_scratch, key, root) = sealed("migrate-differs");
    plaintext(&root);
    let backing = SealedGoals::new(&root, &key).unwrap();
    backing
        .import_locked(
            4242,
            &[br#"{"seq":0,"at_secs":1,"source":"user","text":"other"}"#.to_vec()],
        )
        .unwrap();
    let done = migrate(&root, &key).unwrap();
    assert_eq!((done.entries, done.left), (0, 1));
    assert!(
        root.join("tracking")
            .join("4242")
            .join("log.ndjson")
            .exists()
    );
    // An import over a log that holds entries is refused.
    assert!(matches!(
        backing.import_locked(4242, &[]),
        Err(Failure::Corrupt)
    ));
}
