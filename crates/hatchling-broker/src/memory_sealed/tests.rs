use std::str::FromStr as _;

use hatchling_memory::{
    MemoryPrincipal, MemoryScope, ProjectName, RetentionPolicy, SessionId, Tag,
};
use hatchling_vault::MasterKey;

use super::*;

/// Words no test writes anywhere else: what must never be on disk.
const WORDS: &str = "the owner's quince-coloured umbrella";

fn principal() -> MemoryPrincipal {
    MemoryPrincipal {
        peer_uid: 4242,
        session_id: SessionId::from_str("session-1").unwrap(),
    }
}

fn id(text: &str) -> MemoryId {
    text.parse().unwrap()
}

fn tag(text: &str) -> Tag {
    text.parse().unwrap()
}

/// The memory root of a test, its store sealed under `key`.
fn sealed(name: &str) -> (crate::api_auth::tests::Scratch, MasterKey, PathBuf) {
    let scratch = crate::api_auth::tests::scratch(&format!("memory-sealed-{name}"));
    let root = scratch.0.join("memory");
    (scratch, MasterKey::generate().unwrap(), root)
}

/// Every byte of every file under `dir`.
fn every_file(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                dirs.push(entry.path());
            } else {
                found.push((entry.path(), std::fs::read(entry.path()).unwrap()));
            }
        }
    }
    found
}

fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn every_scope_reads_and_writes_as_before_and_no_word_is_on_the_disk() {
    let (_scratch, key, root) = sealed("scopes");
    let store = open(&root, &key).unwrap();
    let me = principal();
    let project = MemoryScope::Project(ProjectName::from_str("notes").unwrap());
    for (n, scope) in [MemoryScope::User, project.clone(), MemoryScope::Session]
        .iter()
        .enumerate()
    {
        let content = format!("{WORDS} {n}");
        store
            .write(
                &me,
                scope,
                &id("a"),
                &content,
                &[tag("recall")],
                RetentionPolicy::Keep,
                10,
            )
            .unwrap();
        store
            .write(
                &me,
                scope,
                &id("b"),
                "other",
                &[],
                RetentionPolicy::Keep,
                10,
            )
            .unwrap();
        let read = store.recall(&me, scope, &id("a"), 11).unwrap().unwrap();
        assert_eq!(read.content, content);
        let tagged = store.recall_by_tag(&me, scope, &tag("recall"), 11).unwrap();
        assert_eq!(tagged.len(), 1, "{scope:?}");
    }
    // A long item, past one sealed entry, reads back whole.
    let long = format!("{WORDS} {}", "x".repeat(30_000));
    store
        .write(
            &me,
            &MemoryScope::User,
            &id("long"),
            &long,
            &[],
            RetentionPolicy::Keep,
            10,
        )
        .unwrap();
    assert_eq!(
        store
            .recall(&me, &MemoryScope::User, &id("long"), 11)
            .unwrap()
            .unwrap()
            .content,
        long
    );
    // Scopes stay apart: the user scope's items aren't the project's.
    assert!(
        store
            .recall(&me, &project, &id("long"), 11)
            .unwrap()
            .is_none()
    );
    for (path, bytes) in every_file(&root) {
        assert!(
            !holds(&bytes, b"quince"),
            "{} holds the words",
            path.display()
        );
        assert!(
            !holds(&bytes, b"notes"),
            "{} names the project",
            path.display()
        );
    }
    // Removed, an item is gone, and its scope lists it no more.
    assert!(store.remove(&me, &MemoryScope::User, &id("a")).unwrap());
    assert!(!store.remove(&me, &MemoryScope::User, &id("a")).unwrap());
    assert!(
        store
            .recall(&me, &MemoryScope::User, &id("a"), 11)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .recall_by_tag(&me, &MemoryScope::User, &tag("recall"), 11)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_expired_item_is_purged_from_every_space() {
    let (_scratch, key, root) = sealed("purge");
    let store = open(&root, &key).unwrap();
    let me = principal();
    let soon = RetentionPolicy::Ttl { ttl_secs: 5 };
    for scope in [MemoryScope::User, MemoryScope::Session] {
        store
            .write(&me, &scope, &id("brief"), "brief", &[], soon, 100)
            .unwrap();
        store
            .write(
                &me,
                &scope,
                &id("kept"),
                "kept",
                &[],
                RetentionPolicy::Keep,
                100,
            )
            .unwrap();
    }
    assert_eq!(store.purge_expired(104).unwrap(), 0, "not yet");
    assert_eq!(store.purge_expired(106).unwrap(), 2);
    for scope in [MemoryScope::User, MemoryScope::Session] {
        assert!(
            store
                .recall(&me, &scope, &id("brief"), 106)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .recall(&me, &scope, &id("kept"), 106)
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn an_item_removed_with_its_index_left_is_skipped_not_an_error() {
    let (_scratch, key, root) = sealed("crash");
    let store = open(&root, &key).unwrap();
    let me = principal();
    store
        .write(
            &me,
            &MemoryScope::User,
            &id("a"),
            "a",
            &[tag("t")],
            RetentionPolicy::Keep,
            1,
        )
        .unwrap();
    // What a crash after the item's removal, before its index's, leaves.
    let backing = SealedMemory::new(&root, &key).unwrap();
    let vault = backing.vault().unwrap();
    let space = Space::new(vec![String::from("user"), String::from("4242")]);
    assert!(sealed_blobs::remove(&vault, &item_key(&space, &id("a"))).unwrap());
    assert_eq!(backing.ids(&space).unwrap(), vec![id("a")]);
    assert!(
        store
            .recall_by_tag(&me, &MemoryScope::User, &tag("t"), 2)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .recall(&me, &MemoryScope::User, &id("a"), 2)
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_item_swapped_for_another_or_opened_with_another_key_is_refused() {
    let (_scratch, key, root) = sealed("swap");
    let store = open(&root, &key).unwrap();
    let me = principal();
    store
        .write(
            &me,
            &MemoryScope::User,
            &id("a"),
            "a",
            &[],
            RetentionPolicy::Keep,
            1,
        )
        .unwrap();
    store
        .write(
            &me,
            &MemoryScope::User,
            &id("b"),
            "b",
            &[],
            RetentionPolicy::Keep,
            1,
        )
        .unwrap();
    let space = Space::new(vec![String::from("user"), String::from("4242")]);
    let sealed = sealed_root(&root);
    // b's head over a's: the envelope names the entry, so it doesn't open.
    std::fs::copy(
        sealed.join(format!("h{}", item_key(&space, &id("b")))),
        sealed.join(format!("h{}", item_key(&space, &id("a")))),
    )
    .unwrap();
    assert!(matches!(
        store.recall(&me, &MemoryScope::User, &id("a"), 2),
        Err(hatchling_memory::Error::Corrupt)
    ));
    // Another vault's key opens nothing of it.
    let other = MasterKey::generate().unwrap();
    let refused = open(&root, &other).map_or(true, |store| {
        store.recall(&me, &MemoryScope::User, &id("b"), 2).is_err()
    });
    assert!(refused, "another vault's key read an item");
}

#[test]
fn the_store_holds_its_spaces_and_their_ids() {
    let (_scratch, key, root) = sealed("index");
    let store = open(&root, &key).unwrap();
    let me = principal();
    let project = MemoryScope::Project(ProjectName::from_str("p").unwrap());
    store
        .write(
            &me,
            &MemoryScope::User,
            &id("a"),
            "a",
            &[],
            RetentionPolicy::Keep,
            1,
        )
        .unwrap();
    store
        .write(&me, &project, &id("b"), "b", &[], RetentionPolicy::Keep, 1)
        .unwrap();
    store
        .write(&me, &project, &id("b"), "b2", &[], RetentionPolicy::Keep, 2)
        .unwrap();
    let backing = SealedMemory::new(&root, &key).unwrap();
    let spaces = backing.spaces().unwrap();
    assert_eq!(
        spaces,
        vec![
            Space::new(vec![
                String::from("project"),
                String::from("4242"),
                String::from("p")
            ]),
            Space::new(vec![String::from("user"), String::from("4242")]),
        ]
    );
    let project_space = spaces.first().unwrap();
    assert_eq!(
        backing.ids(project_space).unwrap(),
        vec![id("b")],
        "written twice, listed once"
    );
}
