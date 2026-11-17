//! The Library, kept in a scratch directory under a fresh master key.

use std::path::PathBuf;

use hatchling_vault::MasterKey;

use super::*;

/// A config whose Library is kept in a scratch directory.
fn kept(name: &str) -> (Config, crate::api_auth::tests::Scratch, MasterKey) {
    let scratch = crate::api_auth::tests::scratch(&format!("library-{name}"));
    let mut config = crate::api_auth::tests::owner_config(None);
    config.scheduler.library = scratch.0.join("library");
    (config, scratch, MasterKey::generate().unwrap())
}

/// `len` bytes, each its place mod 251, then `seed`.
fn bytes_of(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|at| u8::try_from(at % 251).unwrap().wrapping_add(seed))
        .collect()
}

/// Keeps `bytes` as `name`, which must be kept.
fn keep(library: &Library<'_>, name: &str, bytes: &[u8]) -> KeptFile {
    library
        .keep(name, bytes, "2026-11-17T00:00:00Z")
        .unwrap()
        .unwrap()
}

/// Whether chunk `chunk` of `id` is in the store.
fn has_chunk(library: &Library<'_>, id: &str, chunk: u64) -> bool {
    library
        .vault()
        .unwrap()
        .exists(&Library::name(&chunk_name(id, chunk)).unwrap())
}

/// Writes `state` as the Library's, as a crash would have left it.
fn leave_state(library: &Library<'_>, state: &State) {
    Library::write(&library.vault().unwrap(), STATE, state).unwrap();
}

#[test]
fn the_bounds_are_as_they_are_written() {
    assert_eq!(LIBRARY_FILES, 64);
    assert_eq!(LIBRARY_BYTES, 268_435_456);
    assert_eq!(HANDED_PER_CONVERSATION, 16);
    assert_eq!(CHUNK_BYTES, 32_768);
    assert_eq!(FILE_BYTES, 8_388_608);
    assert_eq!(NAME_BYTES, 64);
    assert_eq!(chunks_of(0), 0);
    assert_eq!(chunks_of(1), 1);
    assert_eq!(chunks_of(32_768), 1);
    assert_eq!(chunks_of(32_769), 2);
    assert_eq!(chunks_of(FILE_BYTES), 256);
}

#[test]
fn a_library_is_there_only_when_the_operator_names_one() {
    let (mut config, _scratch, key) = kept("off");
    assert!(Library::of(&config, &key).is_some());
    // What it says of itself is where it is, and nothing of its keys.
    let said = format!("{:?}", Library::of(&config, &key).unwrap());
    assert!(said.starts_with("Library {"), "{said}");
    assert!(
        said.contains(&format!("{:?}", config.scheduler.library)),
        "{said}"
    );
    assert!(!said.contains("keys"), "{said}");
    config.scheduler.library = PathBuf::new();
    assert!(Library::of(&config, &key).is_none());
}

#[test]
fn a_name_is_plain_ascii_and_bounded() {
    let longest = "n".repeat(NAME_BYTES);
    for good in ["a", "report.pdf", "out_1-2.tar.gz", "A.b", longest.as_str()] {
        assert!(is_name(good), "{good:?}");
    }
    let over = format!("{longest}n");
    for bad in [
        "",
        ".hidden",
        "a/b",
        "..",
        "a b",
        "a\0b",
        "caf\u{e9}",
        "a\u{202e}b",
        over.as_str(),
    ] {
        assert!(!is_name(bad), "{bad:?}");
    }
    let (config, _scratch, key) = kept("names");
    let library = Library::of(&config, &key).unwrap();
    let said = library.keep("../escape", b"x", "now").unwrap_err();
    assert!(said.contains("doesn't keep a file by the name"), "{said}");
    assert!(library.files().unwrap().is_empty());
    assert_eq!(
        library.standing().unwrap(),
        (0, false),
        "a refused name counted"
    );
}

#[test]
fn a_file_is_kept_in_chunks_and_read_back_whole() {
    let (config, _scratch, key) = kept("round-trip");
    let library = Library::of(&config, &key).unwrap();
    let sizes = [
        0,
        1,
        CHUNK_BYTES - 1,
        CHUNK_BYTES,
        CHUNK_BYTES + 1,
        3 * CHUNK_BYTES + 7,
    ];
    let mut ids = Vec::new();
    for (seed, size) in (0_u8..).zip(sizes) {
        let bytes = bytes_of(size, seed);
        let file = keep(&library, &format!("file-{size}.bin"), &bytes);
        assert_eq!(file.bytes, u64::try_from(size).unwrap());
        assert_eq!(file.chunks, chunks_of(file.bytes));
        assert_eq!(file.sha256, hex(&Sha256::digest(&bytes)));
        assert_eq!(file.kept_at, "2026-11-17T00:00:00Z");
        assert!(is_id(&file.id), "{}", file.id);
        assert_eq!(library.fetch(&file.id).unwrap(), Some(bytes));
        ids.push(file.id);
    }
    let listed: Vec<String> = library
        .files()
        .unwrap()
        .into_iter()
        .map(|file| file.id)
        .collect();
    assert_eq!(listed, ids, "the files aren't listed in the order kept");
    assert_eq!(library.fetch(&"0".repeat(64)).unwrap(), None);
    // The same bytes kept twice are two files, by two ids.
    let again = keep(&library, "file-1.bin", &bytes_of(1, 1));
    assert!(!ids.contains(&again.id));
}

#[test]
fn the_longest_file_is_kept_and_one_byte_more_is_refused() {
    let (config, _scratch, key) = kept("longest");
    let library = Library::of(&config, &key).unwrap();
    let most = bytes_of(usize::try_from(FILE_BYTES).unwrap(), 3);
    let file = keep(&library, "most.bin", &most);
    assert_eq!(file.chunks, 256);
    assert_eq!(library.fetch(&file.id).unwrap(), Some(most));
    let over = vec![0; usize::try_from(FILE_BYTES).unwrap() + 1];
    assert_eq!(
        library.keep("over.bin", &over, "now").unwrap(),
        Err(Refused::TooLarge)
    );
    assert_eq!(library.files().unwrap().len(), 1);
    assert_eq!(
        library.standing().unwrap(),
        (1, false),
        "a refused file counted"
    );
}

#[test]
fn a_conversation_hands_over_so_many_and_is_turned_off_for_good() {
    let (config, _scratch, key) = kept("conversation");
    let library = Library::of(&config, &key).unwrap();
    for file in 0..HANDED_PER_CONVERSATION {
        keep(&library, &format!("f{file}"), b"x");
    }
    assert_eq!(library.refuses(1).unwrap(), Some(Refused::Spent));
    assert_eq!(
        library.keep("one-more", b"x", "now").unwrap(),
        Err(Refused::Spent)
    );
    assert_eq!(library.standing().unwrap(), (16, false));
    library.new_conversation().unwrap();
    assert_eq!(library.refuses(1).unwrap(), None);
    keep(&library, "next", b"x");
    // Turned off, it stays off, through another Library on the same
    // store as a restart would open, until the owner's new conversation.
    library.turn_off().unwrap();
    assert_eq!(
        library.keep("after", b"x", "now").unwrap(),
        Err(Refused::Off)
    );
    let reopened = Library::of(&config, &key).unwrap();
    assert_eq!(reopened.standing().unwrap(), (1, true));
    assert_eq!(reopened.refuses(1).unwrap(), Some(Refused::Off));
    reopened.new_conversation().unwrap();
    assert_eq!(library.standing().unwrap(), (0, false));
    assert_eq!(
        library.files().unwrap().len(),
        usize::try_from(HANDED_PER_CONVERSATION).unwrap() + 1
    );
}

#[test]
fn a_full_library_refuses_and_evicts_nothing() {
    let (config, _scratch, key) = kept("full");
    let library = Library::of(&config, &key).unwrap();
    for file in 0..LIBRARY_FILES {
        if file > 0 && file % 16 == 0 {
            library.new_conversation().unwrap();
        }
        // The longest names: a full index still fits one sealed entry.
        keep(&library, &format!("{file:0>64}"), b"x");
    }
    library.new_conversation().unwrap();
    assert_eq!(
        library.keep("one-more", b"x", "now").unwrap(),
        Err(Refused::Full)
    );
    assert_eq!(library.files().unwrap().len(), LIBRARY_FILES);
    let first = library.files().unwrap().first().unwrap().id.clone();
    assert!(library.remove(&first).unwrap());
    keep(&library, "room-again", b"x");
}

#[test]
fn what_is_refused_is_said_in_order_and_at_each_bound() {
    let file = |bytes: u64| KeptFile {
        id: "a".repeat(64),
        name: String::from("f"),
        bytes,
        sha256: String::new(),
        chunks: chunks_of(bytes),
        kept_at: String::new(),
    };
    let open = State::default();
    let refusal =
        |index: &[KeptFile], state: &State, bytes: u64| Library::refusal(index, state, bytes);
    assert_eq!(refusal(&[], &open, FILE_BYTES), None);
    assert_eq!(refusal(&[], &open, FILE_BYTES + 1), Some(Refused::TooLarge));
    let spent = State {
        handed: HANDED_PER_CONVERSATION,
        ..State::default()
    };
    let almost = State {
        handed: HANDED_PER_CONVERSATION - 1,
        ..State::default()
    };
    assert_eq!(refusal(&[], &spent, 1), Some(Refused::Spent));
    assert_eq!(refusal(&[], &almost, 1), None);
    let off = State {
        off: true,
        handed: HANDED_PER_CONVERSATION,
        ..State::default()
    };
    assert_eq!(refusal(&[], &off, FILE_BYTES + 1), Some(Refused::TooLarge));
    assert_eq!(
        refusal(&[], &off, 1),
        Some(Refused::Off),
        "off is said before spent"
    );
    // By bytes: 32 files of the longest fill it, to the byte.
    let longest: Vec<KeptFile> = (0..32).map(|_| file(FILE_BYTES)).collect();
    assert_eq!(refusal(&longest, &open, 0), None);
    assert_eq!(refusal(&longest, &open, 1), Some(Refused::Full));
    let short_of: Vec<KeptFile> = (0..32)
        .map(|at| file(FILE_BYTES - u64::from(at == 0)))
        .collect();
    assert_eq!(refusal(&short_of, &open, 1), None);
    assert_eq!(refusal(&short_of, &open, 2), Some(Refused::Full));
    // By files.
    let many: Vec<KeptFile> = (0..LIBRARY_FILES).map(|_| file(1)).collect();
    assert_eq!(refusal(&many, &open, 1), Some(Refused::Full));
    assert_eq!(refusal(many.get(1..).unwrap(), &open, 1), None);
    for refused in [
        Refused::Off,
        Refused::Spent,
        Refused::Full,
        Refused::TooLarge,
    ] {
        assert!(!refused.words().is_empty());
    }
    assert!(Refused::Off.words().contains("secret"));
    assert!(Refused::Spent.words().contains("16 files"));
    assert!(Refused::Full.words().contains("64 files, or 256 MiB"));
    assert!(Refused::Full.words().contains("removes files"));
    assert!(Refused::TooLarge.words().contains("8 MiB"));
}

#[test]
fn a_file_whose_chunks_dont_add_up_reads_as_damaged() {
    let (config, _scratch, key) = kept("damaged");
    let library = Library::of(&config, &key).unwrap();
    let three = 2 * CHUNK_BYTES + 5;
    let damaged = |library: &Library<'_>, id: &str| {
        let said = library.fetch(id).unwrap_err();
        assert!(said.contains("is damaged"), "{said}");
    };
    let vault = || library.vault().unwrap();
    // A chunk of another file, of the same length, in its place.
    let a = keep(&library, "a", &bytes_of(three, 1));
    let b = keep(&library, "b", &bytes_of(three, 2));
    let theirs = vault()
        .fetch(&Library::name(&chunk_name(&a.id, 0)).unwrap())
        .unwrap();
    Library::put(&vault(), &chunk_name(&b.id, 0), theirs.as_bytes().to_vec()).unwrap();
    damaged(&library, &b.id);
    // The last chunk cut short, a middle one cut short, the first gone.
    for (chunk, bytes) in [
        (2, vec![0; 4]),
        (1, vec![0; CHUNK_BYTES - 1]),
        (0, Vec::new()),
    ] {
        let file = keep(&library, "c", &bytes_of(three, 3));
        if bytes.is_empty() {
            vault()
                .remove(&Library::name(&chunk_name(&file.id, chunk)).unwrap())
                .unwrap();
        } else {
            Library::put(&vault(), &chunk_name(&file.id, chunk), bytes).unwrap();
        }
        damaged(&library, &file.id);
    }
    // The bytes changed, at the same length.
    let file = keep(&library, "d", &bytes_of(5, 4));
    Library::put(&vault(), &chunk_name(&file.id, 0), bytes_of(5, 5)).unwrap();
    damaged(&library, &file.id);
    // The other files are as they were kept.
    assert_eq!(library.fetch(&a.id).unwrap(), Some(bytes_of(three, 1)));
}

#[test]
fn an_index_or_a_state_that_cant_be_one_is_refused() {
    let (config, _scratch, key) = kept("unsound");
    let most = chunks_of(FILE_BYTES);
    let library = Library::of(&config, &key).unwrap();
    let file = keep(&library, "a", b"abc");
    let vault = || library.vault().unwrap();
    let unsound = |index: Vec<KeptFile>| {
        Library::write(&vault(), INDEX, &index).unwrap();
        let said = library.files().unwrap_err();
        assert!(said.contains("index holds what an index can't"), "{said}");
    };
    // An id that isn't 64 lowercase hex digits: too short, all hex; the
    // right length, not hex.
    for id in [
        String::from("short"),
        "a".repeat(63),
        "g".repeat(64),
        "A".repeat(64),
    ] {
        unsound(vec![KeptFile { id, ..file.clone() }]);
    }
    unsound(vec![KeptFile {
        name: String::from("a/b"),
        ..file.clone()
    }]);
    unsound(vec![KeptFile {
        bytes: FILE_BYTES + 1,
        chunks: chunks_of(FILE_BYTES + 1),
        ..file.clone()
    }]);
    unsound(vec![KeptFile {
        chunks: 2,
        ..file.clone()
    }]);
    unsound(vec![file.clone(); LIBRARY_FILES + 1]);
    Library::write(&vault(), INDEX, &vec![file.clone(); LIBRARY_FILES]).unwrap();
    assert_eq!(library.files().unwrap().len(), LIBRARY_FILES);
    Library::write(&vault(), INDEX, &vec![file]).unwrap();
    for state in [
        State {
            handed: HANDED_PER_CONVERSATION + 1,
            ..State::default()
        },
        State {
            pending: Some(Pending {
                op: Op::Keep,
                id: String::from("../x"),
                chunks: 1,
            }),
            ..State::default()
        },
        State {
            pending: Some(Pending {
                op: Op::Remove,
                id: "d".repeat(64),
                chunks: most + 1,
            }),
            ..State::default()
        },
    ] {
        leave_state(&library, &state);
        let said = library.standing().unwrap_err();
        assert!(said.contains("state holds what a state can't"), "{said}");
    }
    leave_state(
        &library,
        &State {
            handed: HANDED_PER_CONVERSATION,
            ..State::default()
        },
    );
    assert_eq!(library.standing().unwrap(), (16, false));
    // A pending change of the longest file's chunks is one: it is
    // recovered, and the state then holds nothing pending.
    leave_state(
        &library,
        &State {
            pending: Some(Pending {
                op: Op::Remove,
                id: "e".repeat(64),
                chunks: most,
            }),
            ..State::default()
        },
    );
    assert_eq!(library.standing().unwrap(), (0, false));
}

#[test]
fn a_keep_cut_short_is_undone_and_still_counts() {
    let (config, _scratch, key) = kept("keep-cut");
    let library = Library::of(&config, &key).unwrap();
    let kept_before = keep(&library, "before", b"before");
    // As a crash, or a failed write, leaves a keep: the pending keep and
    // its count written, some of its chunks, and no index entry.
    let id = "b".repeat(64);
    leave_state(
        &library,
        &State {
            pending: Some(Pending {
                op: Op::Keep,
                id: id.clone(),
                chunks: 3,
            }),
            handed: 2,
            off: false,
        },
    );
    let vault = library.vault().unwrap();
    for chunk in 0..2 {
        Library::put(&vault, &chunk_name(&id, chunk), vec![1; CHUNK_BYTES]).unwrap();
    }
    assert!(has_chunk(&library, &id, 1));
    // The next keep finds it undone first, and is kept itself.
    let after = keep(&library, "after", b"after");
    for chunk in 0..3 {
        assert!(!has_chunk(&library, &id, chunk), "chunk {chunk} was left");
    }
    let listed: Vec<String> = library
        .files()
        .unwrap()
        .into_iter()
        .map(|file| file.id)
        .collect();
    assert_eq!(listed, [kept_before.id, after.id]);
    assert_eq!(
        library.standing().unwrap(),
        (3, false),
        "a keep cut short gave its count back"
    );
}

#[test]
fn a_keep_cut_short_after_its_index_is_finished() {
    let (config, _scratch, key) = kept("keep-indexed");
    let library = Library::of(&config, &key).unwrap();
    let file = keep(&library, "kept", &bytes_of(CHUNK_BYTES + 1, 7));
    // The keep's index entry written, and the crash before its pending
    // was cleared.
    leave_state(
        &library,
        &State {
            pending: Some(Pending {
                op: Op::Keep,
                id: file.id.clone(),
                chunks: file.chunks,
            }),
            handed: 1,
            off: false,
        },
    );
    assert_eq!(
        library.fetch(&file.id).unwrap(),
        Some(bytes_of(CHUNK_BYTES + 1, 7))
    );
    assert_eq!(library.standing().unwrap(), (1, false));
    assert_eq!(
        Library::state(&library.vault().unwrap()).unwrap().pending,
        None
    );
}

#[test]
fn a_removal_cut_short_is_always_finished() {
    let (config, _scratch, key) = kept("remove-cut");
    let library = Library::of(&config, &key).unwrap();
    let staying = keep(&library, "staying", b"staying");
    let size = 2 * CHUNK_BYTES + 1;
    for indexed in [true, false] {
        let going = keep(&library, "going", &bytes_of(size, 9));
        // The removal's pending written; then the crash, before or after
        // its index entry was taken out.
        let state = Library::state(&library.vault().unwrap()).unwrap();
        leave_state(
            &library,
            &State {
                pending: Some(Pending {
                    op: Op::Remove,
                    id: going.id.clone(),
                    chunks: going.chunks,
                }),
                ..state
            },
        );
        if !indexed {
            let mut index = Library::index(&library.vault().unwrap()).unwrap();
            index.retain(|file| file.id != going.id);
            Library::write(&library.vault().unwrap(), INDEX, &index).unwrap();
            // One of its chunks already removed.
            library
                .vault()
                .unwrap()
                .remove(&Library::name(&chunk_name(&going.id, 0)).unwrap())
                .unwrap();
        }
        let listed: Vec<String> = library
            .files()
            .unwrap()
            .into_iter()
            .map(|file| file.id)
            .collect();
        assert_eq!(
            listed,
            std::slice::from_ref(&staying.id),
            "indexed: {indexed}"
        );
        for chunk in 0..going.chunks {
            assert!(
                !has_chunk(&library, &going.id, chunk),
                "indexed: {indexed}, chunk {chunk}"
            );
        }
        assert_eq!(library.fetch(&going.id).unwrap(), None);
    }
    assert_eq!(
        library.fetch(&staying.id).unwrap(),
        Some(b"staying".to_vec())
    );
}

#[test]
fn a_file_is_removed_once_with_its_chunks() {
    let (config, _scratch, key) = kept("remove");
    let library = Library::of(&config, &key).unwrap();
    let a = keep(&library, "a", &bytes_of(CHUNK_BYTES + 2, 1));
    let b = keep(&library, "b", &bytes_of(3, 2));
    assert!(library.remove(&a.id).unwrap());
    assert!(!library.remove(&a.id).unwrap());
    assert!(!library.remove(&"c".repeat(64)).unwrap());
    for chunk in 0..a.chunks {
        assert!(!has_chunk(&library, &a.id, chunk));
    }
    assert_eq!(library.files().unwrap(), std::slice::from_ref(&b));
    assert_eq!(library.fetch(&b.id).unwrap(), Some(bytes_of(3, 2)));
    assert_eq!(
        library.standing().unwrap(),
        (2, false),
        "a removal gave a count back"
    );
}

#[test]
fn keeps_at_once_stay_within_the_conversations_count() {
    let (config, _scratch, key) = kept("at-once");
    let library = Library::of(&config, &key).unwrap();
    let outcomes: Vec<Result<KeptFile, Refused>> = std::thread::scope(|scope| {
        // Every thread is started before any is joined.
        #[allow(
            clippy::needless_collect,
            reason = "every thread starts before any is joined"
        )]
        let threads: Vec<_> = (0..4)
            .map(|thread| {
                let library = &library;
                scope.spawn(move || {
                    (0..6)
                        .map(|file| {
                            library
                                .keep(&format!("t{thread}-{file}"), b"x", "now")
                                .unwrap()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect()
    });
    let kept = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    assert_eq!(kept, usize::try_from(HANDED_PER_CONVERSATION).unwrap());
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, Ok(_) | Err(Refused::Spent)))
    );
    assert_eq!(library.files().unwrap().len(), kept);
}

/// A file's hash is SHA-256, in lowercase hex: as the app will check it.
#[test]
fn a_files_hash_is_its_sha256_in_lowercase_hex() {
    let (config, _scratch, key) = kept("hash");
    let library = Library::of(&config, &key).unwrap();
    let file = keep(&library, "abc.txt", b"abc");
    assert_eq!(
        file.sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

use hatchling_vault::fault::{Step, fail_nth};

/// A file of two chunks, kept with the `skipped`th store's `step` made
/// to fail: what `keep` said, and the Library for the next operation.
fn keep_failing(
    name: &str,
    step: Step,
    skipped: usize,
) -> (Result<KeptFile, String>, Vec<KeptFile>, (u32, bool), bool) {
    let (config, _scratch, key) = kept(name);
    let library = Library::of(&config, &key).unwrap();
    // The store is made, and its key kept, before any failure is asked.
    assert_eq!(library.standing().unwrap(), (0, false));
    let bytes = bytes_of(CHUNK_BYTES + 1, 6);
    fail_nth(step, skipped);
    let said = library
        .keep("two.bin", &bytes, "now")
        .map(|kept| kept.unwrap());
    let files = library.files().unwrap();
    let standing = library.standing().unwrap();
    // A kept file reads back whole; with none kept, no chunk is left in
    // the store's directory, whatever its id was.
    let chunks_left = std::fs::read_dir(&config.scheduler.library)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("library-") && name != INDEX && name != STATE)
        .count();
    let whole = files.first().map_or(chunks_left == 0, |file| {
        library.fetch(&file.id).unwrap() == Some(bytes) && chunks_left == 2
    });
    assert_eq!(
        Library::state(&library.vault().unwrap()).unwrap().pending,
        None
    );
    (said, files, standing, whole)
}

/// Each store a keep makes, made to fail before its rename and after it:
/// up to its index the file isn't kept, and said not to be, and still
/// counts; once the index names it, it is kept, and said to be, whatever
/// fails after. The stores, in order: the pending state, the two chunks,
/// the index, the state cleared.
#[test]
fn a_keep_that_fails_at_any_store_is_said_as_it_is() {
    for (store, step, kept) in [
        (0, Step::Rename, false),
        (0, Step::DirSync, false),
        (1, Step::Rename, false),
        (2, Step::DirSync, false),
        (3, Step::Rename, false),
        (3, Step::DirSync, true),
        (4, Step::Rename, true),
        (4, Step::DirSync, true),
    ] {
        let (said, files, standing, whole) =
            keep_failing(&format!("keep-fail-{store}-{step:?}"), step, store);
        let case = format!("store {store} at {step:?}");
        assert_eq!(said.is_ok(), kept, "{case}: {said:?}");
        assert_eq!(files.len(), usize::from(kept), "{case}");
        assert!(whole, "{case}: a chunk was left, or the file isn't whole");
        // A keep counts once its pending is visible: only a failure before
        // the first rename gives it back, as nothing of it was written.
        let counted = !(store == 0 && step == Step::Rename);
        assert_eq!(standing, (u32::from(counted), false), "{case}");
    }
}

/// Each store a removal makes, made to fail: before its pending is
/// visible the file stays and the removal is said to have failed; from
/// then on it is removed, and said to be. The stores: the pending state,
/// the index, the state cleared.
#[test]
fn a_removal_that_fails_at_any_store_is_said_as_it_is() {
    for (store, step, removed) in [
        (0, Step::Rename, false),
        (0, Step::DirSync, true),
        (1, Step::Rename, true),
        (1, Step::DirSync, true),
        (2, Step::Rename, true),
    ] {
        let case = format!("store {store} at {step:?}");
        let (config, _scratch, key) = kept(&format!("remove-fail-{store}-{step:?}"));
        let library = Library::of(&config, &key).unwrap();
        let bytes = bytes_of(CHUNK_BYTES + 1, 8);
        let file = keep(&library, "two.bin", &bytes);
        fail_nth(step, store);
        let said = library.remove(&file.id);
        assert_eq!(said.is_ok(), removed, "{case}: {said:?}");
        let listed = library.files().unwrap();
        if removed {
            assert!(listed.is_empty(), "{case}");
            assert!(
                (0..2).all(|chunk| !has_chunk(&library, &file.id, chunk)),
                "{case}"
            );
        } else {
            assert_eq!(listed.len(), 1, "{case}");
            assert_eq!(library.fetch(&file.id).unwrap(), Some(bytes), "{case}");
        }
        assert_eq!(
            Library::state(&library.vault().unwrap()).unwrap().pending,
            None,
            "{case}"
        );
    }
}
