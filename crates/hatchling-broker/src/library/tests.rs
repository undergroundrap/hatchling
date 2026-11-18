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
fn keep(library: &Library<'_>, name: &str, bytes: &[u8]) -> KeptItem {
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
    assert_eq!(LIBRARY_ITEMS, 64);
    assert_eq!(LIBRARY_BYTES, 268_435_456);
    assert_eq!(LIBRARY_FILES, 4096);
    assert_eq!(HANDED_PER_CONVERSATION, 16);
    assert_eq!(READ_PER_CONVERSATION, 134_217_728);
    assert_eq!(FOLDER_BYTES, 33_554_432);
    assert_eq!(
        (FOLDER_FILES, FOLDER_DEPTH, PATH_BYTES, STEP_BYTES),
        (512, 16, 1024, 255)
    );
    assert_eq!((LIST_BYTES, LIST_CHUNKS), (589_826, 19));
    // The literals the list's bounds are written with are these.
    assert_eq!(
        LIST_BYTES,
        FOLDER_FILES * (u64::try_from(PATH_BYTES).unwrap() + 128) + 2
    );
    assert_eq!(
        LIST_CHUNKS,
        LIST_BYTES.div_ceil(u64::try_from(CHUNK_BYTES).unwrap())
    );
    assert_eq!(chunks_of(FOLDER_BYTES), 1024);
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
    assert!(said.contains("doesn't keep an item by that name"), "{said}");
    assert!(
        !said.contains("escape"),
        "a name the agent chose was said: {said}"
    );
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
    assert_eq!(library.read_out(&"0".repeat(64)), Ok(ReadOut::Gone));
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
    assert_eq!(
        library.refuses(&Claim::file(1)).unwrap(),
        Some(Refused::Spent)
    );
    assert_eq!(
        library.keep("one-more", b"x", "now").unwrap(),
        Err(Refused::Spent)
    );
    assert_eq!(library.standing().unwrap(), (16, false));
    library.new_conversation().unwrap();
    assert_eq!(library.refuses(&Claim::file(1)).unwrap(), None);
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
    assert_eq!(
        reopened.refuses(&Claim::file(1)).unwrap(),
        Some(Refused::Off)
    );
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
    for file in 0..LIBRARY_ITEMS {
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
    assert_eq!(library.files().unwrap().len(), LIBRARY_ITEMS);
    let first = library.files().unwrap().first().unwrap().id.clone();
    assert!(library.remove(&first).unwrap());
    keep(&library, "room-again", b"x");
}

/// An item of `kind`, `files` and `bytes`, as an index names one.
fn item(kind: ItemKind, files: u64, bytes: u64) -> KeptItem {
    KeptItem {
        id: "a".repeat(64),
        kind,
        name: String::from("f"),
        bytes,
        files,
        sha256: "b".repeat(64),
        chunks: chunks_of(bytes),
        list_chunks: u64::from(kind == ItemKind::Folder),
        kept_at: String::new(),
        conversation: 0,
        reply: String::new(),
        left_out: LeftOut::default(),
    }
}

#[test]
fn what_is_refused_is_said_in_order_and_at_each_bound() {
    let open = State::default();
    let refusal = |index: &[KeptItem], state: &State, bytes: u64| {
        Library::refusal(index, state, &Claim::file(bytes), false)
    };
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
    // By the conversation's bytes: all read, or more than is left.
    let read = |read: u64| State {
        read,
        ..State::default()
    };
    assert_eq!(
        refusal(&[], &read(READ_PER_CONVERSATION), 0),
        Some(Refused::ReadOut)
    );
    assert_eq!(refusal(&[], &read(READ_PER_CONVERSATION - 1), 1), None);
    assert_eq!(
        refusal(&[], &read(READ_PER_CONVERSATION - 1), 2),
        Some(Refused::ReadOut)
    );
    assert_eq!(
        refusal(&[], &spent, 1),
        Some(Refused::Spent),
        "spent is said before read out"
    );
    // Once spent, a hand-over is held to every bound but its count and
    // its bytes.
    for state in [&spent, &read(READ_PER_CONVERSATION)] {
        assert_eq!(Library::refusal(&[], state, &Claim::file(1), true), None);
    }
    assert_eq!(
        Library::refusal(&[], &off, &Claim::file(1), true),
        Some(Refused::Off)
    );
}

/// The Library's room, by its bytes, its items and its files in all, and
/// a folder's own bounds.
#[test]
fn the_librarys_room_and_a_folders_bounds_are_held() {
    let file = |bytes: u64| item(ItemKind::File, 1, bytes);
    let open = State::default();
    let refusal = |index: &[KeptItem], state: &State, bytes: u64| {
        Library::refusal(index, state, &Claim::file(bytes), false)
    };
    // By bytes: 32 files of the longest fill it, to the byte, and then it
    // is full before anything is read, whatever its length.
    let longest: Vec<KeptItem> = (0..32).map(|_| file(FILE_BYTES)).collect();
    assert_eq!(refusal(&longest, &open, 0), Some(Refused::Full));
    assert_eq!(refusal(&longest, &open, 1), Some(Refused::Full));
    let short_of: Vec<KeptItem> = (0..32)
        .map(|at| file(FILE_BYTES - u64::from(at == 0)))
        .collect();
    assert_eq!(refusal(&short_of, &open, 1), None);
    assert_eq!(refusal(&short_of, &open, 2), Some(Refused::Full));
    // By items.
    let many: Vec<KeptItem> = (0..LIBRARY_ITEMS).map(|_| file(1)).collect();
    assert_eq!(refusal(&many, &open, 1), Some(Refused::Full));
    assert_eq!(refusal(many.get(1..).unwrap(), &open, 1), None);
    // By files in all: eight folders of the most files.
    let folders: Vec<KeptItem> = (0..8)
        .map(|_| item(ItemKind::Folder, FOLDER_FILES, 1))
        .collect();
    assert_eq!(refusal(&folders, &open, 1), Some(Refused::Full));
    assert_eq!(refusal(folders.get(1..).unwrap(), &open, 1), None);
    let one_short: Vec<KeptItem> = (0..8)
        .map(|at| item(ItemKind::Folder, FOLDER_FILES - u64::from(at == 0), 1))
        .collect();
    assert_eq!(refusal(&one_short, &open, 1), None);
    assert_eq!(
        Library::refusal(&one_short, &open, &Claim::folder(2, 1), false),
        Some(Refused::Full)
    );
    // A folder: its bytes and files, at their bounds and one past, and
    // none.
    let folder =
        |files: u64, bytes: u64| Library::refusal(&[], &open, &Claim::folder(files, bytes), false);
    assert_eq!(folder(FOLDER_FILES, FOLDER_BYTES), None);
    assert_eq!(folder(FOLDER_FILES + 1, 1), Some(Refused::FolderTooLarge));
    assert_eq!(folder(1, FOLDER_BYTES + 1), Some(Refused::FolderTooLarge));
    assert_eq!(folder(0, 0), Some(Refused::FolderTooLarge));
    assert_eq!(folder(1, 0), None);
    let words = [
        Refused::Off,
        Refused::Spent,
        Refused::ReadOut,
        Refused::Full,
        Refused::TooLarge,
        Refused::FolderTooLarge,
        Refused::Stale,
    ]
    .map(Refused::words);
    let mut distinct = words.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), words.len());
    assert!(Refused::Off.words().contains("secret"));
    assert!(Refused::Spent.words().contains("16 times"));
    assert!(Refused::ReadOut.words().contains("128 MiB"));
    assert!(
        Refused::Full
            .words()
            .contains("64 items, 256 MiB or 4096 files")
    );
    assert!(Refused::Full.words().contains("removes items"));
    assert!(Refused::TooLarge.words().contains("8 MiB"));
    assert!(Refused::FolderTooLarge.words().contains("32 MiB"));
    assert!(Refused::FolderTooLarge.words().contains("512 files"));
}

#[test]
fn a_file_whose_chunks_dont_add_up_reads_as_damaged() {
    let (config, _scratch, key) = kept("damaged");
    let library = Library::of(&config, &key).unwrap();
    let three = 2 * CHUNK_BYTES + 5;
    let damaged = |library: &Library<'_>, id: &str| {
        let said = library.fetch(id).unwrap_err();
        assert!(said.contains("is damaged"), "{said}");
        // Read out for the owner, it is damaged, not gone.
        assert_eq!(library.read_out(id), Ok(ReadOut::Damaged));
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
fn an_index_that_cant_be_one_is_refused() {
    let (config, _scratch, key) = kept("unsound");
    let library = Library::of(&config, &key).unwrap();
    let file = keep(&library, "a", b"abc");
    let vault = || library.vault().unwrap();
    let unsound = |index: Vec<KeptItem>| {
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
        unsound(vec![KeptItem { id, ..file.clone() }]);
    }
    unsound(vec![KeptItem {
        name: String::from("a/b"),
        ..file.clone()
    }]);
    unsound(vec![KeptItem {
        bytes: FILE_BYTES + 1,
        chunks: chunks_of(FILE_BYTES + 1),
        ..file.clone()
    }]);
    unsound(vec![KeptItem {
        chunks: 2,
        ..file.clone()
    }]);
    unsound(vec![file; LIBRARY_ITEMS + 1]);
}

/// An item the index can't hold: not its kind's, its hash, time or reply
/// not of their shapes, or the Library's bounds passed in all.
#[test]
fn an_item_the_index_cant_hold_is_refused() {
    let (config, _scratch, key) = kept("unsound-items");
    let library = Library::of(&config, &key).unwrap();
    let file = keep(&library, "a", b"abc");
    let vault = || library.vault().unwrap();
    let unsound = |index: Vec<KeptItem>| {
        Library::write(&vault(), INDEX, &index).unwrap();
        let said = library.items().unwrap_err();
        assert!(said.contains("index holds what an index can't"), "{said}");
    };
    // An item that isn't its kind's, or whose hash, time or reply aren't
    // of their shapes.
    unsound(vec![KeptItem {
        files: 2,
        ..file.clone()
    }]);
    unsound(vec![KeptItem {
        list_chunks: 1,
        ..file.clone()
    }]);
    unsound(vec![KeptItem {
        sha256: String::from("abc"),
        ..file.clone()
    }]);
    unsound(vec![KeptItem {
        kept_at: "t".repeat(41),
        ..file.clone()
    }]);
    unsound(vec![KeptItem {
        kept_at: String::from("2026-11-18 caf\u{e9}"),
        ..file.clone()
    }]);
    for reply in [
        "cell-1",
        "run-0123456789abcdef0123456",
        "run-0123456789ABCDEF01234567",
    ] {
        unsound(vec![KeptItem {
            reply: String::from(reply),
            ..file.clone()
        }]);
    }
    // A file has nothing left out: only a folder's walk leaves anything.
    unsound(vec![KeptItem {
        left_out: LeftOut {
            others: 1,
            ..LeftOut::default()
        },
        ..file.clone()
    }]);
    let folder = item(ItemKind::Folder, 2, 10);
    for bad in [
        KeptItem {
            files: 0,
            ..folder.clone()
        },
        KeptItem {
            files: FOLDER_FILES + 1,
            ..folder.clone()
        },
        KeptItem {
            bytes: FOLDER_BYTES + 1,
            chunks: chunks_of(FOLDER_BYTES + 1),
            ..folder.clone()
        },
        KeptItem {
            list_chunks: 0,
            ..folder.clone()
        },
        KeptItem {
            list_chunks: LIST_CHUNKS + 1,
            ..folder
        },
    ] {
        unsound(vec![bad]);
    }
    // More bytes, or files, in all than the Library keeps.
    // Too many bytes, with few files; then too many files, with few bytes.
    unsound(vec![item(ItemKind::Folder, 1, FOLDER_BYTES); 9]);
    let wide = KeptItem {
        id: "c".repeat(64),
        ..item(ItemKind::Folder, FOLDER_FILES, 1)
    };
    unsound(vec![wide; 9]);
    Library::write(&vault(), INDEX, &vec![file; LIBRARY_ITEMS]).unwrap();
    assert_eq!(library.files().unwrap().len(), LIBRARY_ITEMS);
}

/// A state the Library can't have is refused; one at its bounds is read,
/// and what it leaves pending recovered.
#[test]
fn a_state_that_cant_be_one_is_refused() {
    let (config, _scratch, key) = kept("unsound-state");
    let most = chunks_of(FILE_BYTES);
    let library = Library::of(&config, &key).unwrap();
    keep(&library, "a", b"abc");
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
                list_chunks: 0,
            }),
            ..State::default()
        },
        State {
            pending: Some(Pending {
                op: Op::Remove,
                id: "d".repeat(64),
                chunks: chunks_of(FOLDER_BYTES) + 1,
                list_chunks: 0,
            }),
            ..State::default()
        },
        State {
            pending: Some(Pending {
                op: Op::Remove,
                id: "d".repeat(64),
                chunks: 1,
                list_chunks: LIST_CHUNKS + 1,
            }),
            ..State::default()
        },
        State {
            read: READ_PER_CONVERSATION + 1,
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
    // A pending change of the most chunks a folder and its list are kept
    // in is one, and recovered.
    leave_state(
        &library,
        &State {
            pending: Some(Pending {
                op: Op::Remove,
                id: "f".repeat(64),
                chunks: chunks_of(FOLDER_BYTES),
                list_chunks: LIST_CHUNKS,
            }),
            ..State::default()
        },
    );
    assert_eq!(library.standing().unwrap(), (0, false));
    // A pending change of the longest file's chunks is one: it is
    // recovered, and the state then holds nothing pending.
    leave_state(
        &library,
        &State {
            pending: Some(Pending {
                op: Op::Remove,
                id: "e".repeat(64),
                chunks: most,
                list_chunks: 0,
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
                list_chunks: 0,
            }),
            handed: 2,
            off: false,
            conversation: 0,
            read: 0,
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
                list_chunks: 0,
            }),
            handed: 1,
            off: false,
            conversation: 0,
            read: 0,
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
                    list_chunks: 0,
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
    let outcomes: Vec<Result<KeptItem, Refused>> = std::thread::scope(|scope| {
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
) -> (Result<KeptItem, String>, Vec<KeptItem>, (u32, bool), bool) {
    let (config, _scratch, key) = kept(name);
    let library = Library::of(&config, &key).unwrap();
    // The store is made, and its key kept, before any failure is asked.
    assert_eq!(library.standing().unwrap(), (0, false));
    let bytes = bytes_of(CHUNK_BYTES + 1, 6);
    let conversation = library.conversation().unwrap();
    let claim = Claim::file(u64::try_from(bytes.len()).unwrap());
    let spent = library.spend(&claim, conversation).unwrap().unwrap();
    fail_nth(step, skipped);
    let said = library
        .keep_spent("two.bin", &bytes, "now", spent, REPLY)
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
/// up to its index the file isn't kept, and said not to be, and its
/// hand-over stays spent; once the index names it, it is kept, and said to
/// be, whatever fails after. The stores, in order: the pending state, the two chunks,
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
        // Spent before it was kept, it counts, whatever failed.
        assert_eq!(standing, (1, false), "{case}");
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

/// A hand-over is spent, sealed, before its file is checked: the count goes
/// up, and nothing is kept until the file is.
#[test]
fn a_hand_over_is_spent_before_its_file_is_kept() {
    let (config, _scratch, key) = kept("spend");
    let library = Library::of(&config, &key).unwrap();
    let conversation = library.conversation().unwrap();
    let first = library
        .spend(&Claim::file(3), conversation)
        .unwrap()
        .unwrap();
    assert_eq!(library.standing().unwrap(), (1, false));
    assert!(library.files().unwrap().is_empty());
    // Only what was spent is kept: another length, or a folder, isn't.
    let other = library
        .spend(&Claim::file(3), conversation)
        .unwrap()
        .unwrap();
    let said = library
        .keep_spent("c.txt", b"abcd", "now", other, REPLY)
        .unwrap_err();
    assert!(said.contains("isn't what was spent"), "{said}");
    let other = library
        .spend(&Claim::file(3), conversation)
        .unwrap()
        .unwrap();
    let said = library
        .keep_folder_spent(
            "f",
            &[("a", b"abc")],
            LeftOut::default(),
            "now",
            other,
            REPLY,
        )
        .unwrap_err();
    assert!(said.contains("isn't what was spent"), "{said}");
    assert!(library.files().unwrap().is_empty());
    // A new keep names its reply.
    let other = library
        .spend(&Claim::file(3), conversation)
        .unwrap()
        .unwrap();
    let said = library
        .keep_spent("c.txt", b"abc", "now", other, "")
        .unwrap_err();
    assert!(said.contains("reply"), "{said}");
    // Kept as spent, it isn't counted again.
    let kept = library
        .keep_spent("a.txt", b"abc", "now", first, REPLY)
        .unwrap()
        .unwrap();
    assert_eq!(kept.reply, REPLY);
    assert_eq!(library.fetch(&kept.id).unwrap(), Some(b"abc".to_vec()));
    assert_eq!(library.standing().unwrap(), (4, false));
    // Spent to the last one, the next is refused, and nothing more spent.
    let mut last = None;
    for _ in 4..HANDED_PER_CONVERSATION {
        last = Some(
            library
                .spend(&Claim::file(1), conversation)
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(
        library.spend(&Claim::file(1), conversation),
        Ok(Err(Refused::Spent))
    );
    assert_eq!(
        library.standing().unwrap(),
        (HANDED_PER_CONVERSATION, false)
    );
    // A hand-over spent before the last is still kept: its count is spent.
    assert!(
        library
            .keep_spent("b.txt", b"b", "now", last.unwrap(), REPLY)
            .unwrap()
            .is_ok()
    );
}

/// What the Library couldn't keep is refused before anything is spent:
/// off, spent, too long, or no room for its bytes.
#[test]
fn a_hand_over_the_library_couldnt_keep_spends_nothing() {
    let (config, _scratch, key) = kept("spend-refused");
    let library = Library::of(&config, &key).unwrap();
    let conversation = library.conversation().unwrap();
    assert_eq!(
        library.spend(&Claim::file(FILE_BYTES + 1), conversation),
        Ok(Err(Refused::TooLarge))
    );
    // Near its byte bound, a file of more than the room left is refused.
    let mut held = vec![FILE_BYTES; 31];
    held.push(FILE_BYTES - 5);
    library.pretend_held(&held);
    let left = LIBRARY_BYTES - held.iter().sum::<u64>();
    assert_eq!(left, 5);
    assert_eq!(
        library.spend(&Claim::file(left + 1), conversation),
        Ok(Err(Refused::Full))
    );
    assert_eq!(library.standing().unwrap(), (0, false));
    assert!(matches!(
        library.spend(&Claim::file(left), conversation),
        Ok(Ok(_))
    ));
    library.turn_off().unwrap();
    assert_eq!(
        library.spend(&Claim::file(1), conversation),
        Ok(Err(Refused::Off))
    );
    assert_eq!(library.standing().unwrap(), (1, true));
}

/// A hand-over belongs to the conversation it began in: after the owner
/// starts a new one, it is neither spent nor kept, and turns nothing off.
#[test]
fn a_hand_over_from_a_conversation_since_ended_is_neither_spent_nor_kept() {
    let (config, _scratch, key) = kept("spend-stale");
    let library = Library::of(&config, &key).unwrap();
    let old = library.conversation().unwrap();
    let spent_old = library.spend(&Claim::file(1), old).unwrap().unwrap();
    library.new_conversation().unwrap();
    let new = library.conversation().unwrap();
    assert_ne!(new, old);
    assert_eq!(library.spend(&Claim::file(1), old), Ok(Err(Refused::Stale)));
    assert_eq!(
        library.keep_spent("a.txt", b"a", "now", spent_old, REPLY),
        Ok(Err(Refused::Stale))
    );
    assert_eq!(library.standing().unwrap(), (0, false));
    assert!(library.files().unwrap().is_empty());
    let spent_a = library.spend(&Claim::file(1), new).unwrap().unwrap();
    let spent_b = library.spend(&Claim::file(1), new).unwrap().unwrap();
    assert!(
        library
            .keep_spent("a.txt", b"a", "now", spent_a, REPLY)
            .unwrap()
            .is_ok()
    );
    // Turned off since it was spent, a hand-over isn't kept.
    library.turn_off().unwrap();
    assert_eq!(
        library.keep_spent("b.txt", b"b", "now", spent_b, REPLY),
        Ok(Err(Refused::Off))
    );
    assert_eq!(Refused::Stale.words(), crate::hand_over::STALE);
}

/// A state kept before the Library numbered its conversations reads as the
/// first conversation, its count and off as they were.
#[test]
fn a_state_from_before_conversations_were_numbered_reads_as_the_first() {
    let (config, _scratch, key) = kept("spend-old-state");
    let library = Library::of(&config, &key).unwrap();
    Library::write(
        &library.vault().unwrap(),
        STATE,
        &serde_json::json!({"pending": null, "handed": 3, "off": true}),
    )
    .unwrap();
    assert_eq!(library.standing().unwrap(), (3, true));
    assert_eq!(library.conversation().unwrap(), 0);
}

/// A secret found by a run begun before the owner's new conversation turns
/// nothing off in it.
#[test]
fn an_old_conversations_secret_turns_the_new_one_nothing_off() {
    let (config, _scratch, key) = kept("off-stale");
    let library = Library::of(&config, &key).unwrap();
    let old = library.conversation().unwrap();
    library.new_conversation().unwrap();
    library.turn_off_in(old).unwrap();
    assert_eq!(library.standing().unwrap(), (0, false));
    library
        .turn_off_in(library.conversation().unwrap())
        .unwrap();
    assert_eq!(library.standing().unwrap(), (0, true));
}

/// The chat run a test's items came from.
#[allow(
    clippy::redundant_pub_crate,
    reason = "the API's tests keep items from this run too"
)]
pub(crate) const REPLY: &str = "run-0123456789abcdef01234567";

/// A folder's files: a path and its bytes each.
fn folder_of(sizes: &[(&str, usize)]) -> Vec<(String, Vec<u8>)> {
    (0_u8..)
        .zip(sizes)
        .map(|(seed, (path, size))| ((*path).to_owned(), bytes_of(*size, seed)))
        .collect()
}

/// Spends a hand-over on `files` and keeps them as the folder `name`,
/// which must be kept.
fn keep_folder(library: &Library<'_>, name: &str, files: &[(String, Vec<u8>)]) -> KeptItem {
    let conversation = library.conversation().unwrap();
    let bytes = files
        .iter()
        .map(|(_, bytes)| u64::try_from(bytes.len()).unwrap())
        .sum();
    let claim = Claim::folder(u64::try_from(files.len()).unwrap(), bytes);
    let spent = library.spend(&claim, conversation).unwrap().unwrap();
    let parts: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    library
        .keep_folder_spent(
            name,
            &parts,
            LeftOut::default(),
            "2026-11-18T00:00:00Z",
            spent,
            REPLY,
        )
        .unwrap()
        .unwrap()
}

/// A folder is kept as one item, its files back to back, and each read
/// back whole, to its own hash, whatever chunks it crosses.
#[test]
fn a_folder_is_kept_and_each_file_read_back_to_its_own_hash() {
    let (config, _scratch, key) = kept("folder");
    let library = Library::of(&config, &key).unwrap();
    let file = keep(&library, "alone.txt", b"alone");
    let files = folder_of(&[
        ("index.html", 10),
        ("css/site.css", CHUNK_BYTES + 5),
        ("img/a.bin", 2 * CHUNK_BYTES),
        ("empty", 0),
        (".github/x y.yml", 7),
    ]);
    let total: usize = files.iter().map(|(_, bytes)| bytes.len()).sum();
    let folder = keep_folder(&library, "site", &files);
    assert_eq!(folder.kind, ItemKind::Folder);
    assert_eq!(
        (folder.files, folder.bytes),
        (5, u64::try_from(total).unwrap())
    );
    assert_eq!(folder.chunks, chunks_of(folder.bytes));
    assert_eq!(folder.list_chunks, 1);
    assert_eq!((folder.conversation, folder.reply.as_str()), (0, REPLY));
    // Its list, to its hash, in the order kept.
    let ListOut::Whole(list) = library.list_of(&folder.id).unwrap() else {
        unreachable!("the folder's list didn't read back");
    };
    let said: Vec<(&str, u64)> = list
        .iter()
        .map(|file| (file.path.as_str(), file.bytes))
        .collect();
    let kept: Vec<(&str, u64)> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), u64::try_from(bytes.len()).unwrap()))
        .collect();
    assert_eq!(said, kept);
    for (n, (_, bytes)) in files.iter().enumerate() {
        assert_eq!(
            library.read_out_in(&folder.id, n),
            Ok(ReadOut::Whole(Zeroizing::new(bytes.clone()))),
            "file {n}"
        );
        assert_eq!(list.get(n).unwrap().sha256, hex(&Sha256::digest(bytes)));
    }
    // A folder is no file, and a file no folder; past its last file there
    // is none.
    assert_eq!(library.read_out(&folder.id), Ok(ReadOut::Gone));
    assert_eq!(library.list_of(&file.id), Ok(ListOut::Gone));
    assert_eq!(library.read_out_in(&file.id, 0), Ok(ReadOut::Gone));
    assert_eq!(library.read_out_in(&folder.id, 5), Ok(ReadOut::Gone));
    assert_eq!(library.list_of(&"0".repeat(64)), Ok(ListOut::Gone));
    // Files lists only files; items, both.
    assert_eq!(library.files().unwrap(), std::slice::from_ref(&file));
    let ids: Vec<String> = library
        .items()
        .unwrap()
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(ids, [file.id.clone(), folder.id.clone()]);
    // Removed, nothing of it is left: its bytes nor its list.
    assert!(library.remove(&folder.id).unwrap());
    for chunk in 0..folder.chunks {
        assert!(!has_chunk(&library, &folder.id, chunk), "chunk {chunk}");
    }
    let list = Library::name(&list_chunk_name(&folder.id, 0)).unwrap();
    assert!(!library.vault().unwrap().exists(&list), "its list was left");
    assert_eq!(library.fetch(&file.id).unwrap(), Some(b"alone".to_vec()));
}

/// A damaged chunk damages only the files in it; a damaged list, the
/// whole folder.
#[test]
fn a_damaged_file_in_a_folder_damages_only_itself() {
    let (config, _scratch, key) = kept("folder-damaged");
    let library = Library::of(&config, &key).unwrap();
    // Each file in chunks of its own.
    let files = folder_of(&[("a", CHUNK_BYTES), ("b", CHUNK_BYTES), ("c", CHUNK_BYTES)]);
    let folder = keep_folder(&library, "three", &files);
    library.damage_chunk(&folder.id, 1, false);
    let read = |n| library.read_out_in(&folder.id, n).unwrap();
    assert_eq!(read(1), ReadOut::Damaged);
    for n in [0, 2] {
        assert!(matches!(read(n), ReadOut::Whole(_)), "file {n}");
    }
    // A chunk gone is damage too.
    library
        .vault()
        .unwrap()
        .remove(&Library::name(&chunk_name(&folder.id, 2)).unwrap())
        .unwrap();
    assert_eq!(read(2), ReadOut::Damaged);
    library.damage_chunk(&folder.id, 0, true);
    assert_eq!(library.list_of(&folder.id), Ok(ListOut::Damaged));
    assert_eq!(read(0), ReadOut::Damaged);
}

/// A list that comes to the index's hash but can't be the folder's (its
/// lengths, its count, a path, a hash) reads as damaged: the index is the
/// Library's word, and the list must agree with it.
#[test]
fn a_list_that_disagrees_with_its_item_reads_as_damaged() {
    let (config, _scratch, key) = kept("folder-lies");
    let library = Library::of(&config, &key).unwrap();
    let vault = || library.vault().unwrap();
    let entry = |path: &str, bytes: u64| FolderFile {
        path: path.to_owned(),
        bytes,
        sha256: "c".repeat(64),
    };
    let id = "d".repeat(64);
    for (case, list) in [
        ("lengths", vec![entry("a", 1), entry("b", 2)]),
        ("count", vec![entry("a", 4)]),
        ("path", vec![entry("a", 2), entry("../b", 2)]),
        (
            "hash",
            vec![
                entry("a", 2),
                FolderFile {
                    sha256: String::from("x"),
                    ..entry("b", 2)
                },
            ],
        ),
        ("case", vec![entry("a", 2), entry("A", 2)]),
    ] {
        let bytes = serde_json::to_vec(&list).unwrap();
        Library::put(&vault(), &list_chunk_name(&id, 0), bytes.clone()).unwrap();
        library.pretend_index(&vec![KeptItem {
            id: id.clone(),
            sha256: hex(&Sha256::digest(&bytes)),
            ..item(ItemKind::Folder, 2, 4)
        }]);
        assert_eq!(library.list_of(&id), Ok(ListOut::Damaged), "{case}");
        assert_eq!(library.read_out_in(&id, 0), Ok(ReadOut::Damaged), "{case}");
    }
    // A list that would be the folder's, padded past the most a list may
    // be (JSON takes the spaces), to its hash: never read whole.
    let mut padded = serde_json::to_vec(&vec![entry("a", 2), entry("b", 2)]).unwrap();
    let most = usize::try_from(LIST_CHUNKS).unwrap() * CHUNK_BYTES;
    assert!(u64::try_from(most).unwrap() > LIST_BYTES);
    padded.resize(most, b' ');
    for (chunk, piece) in (0_u64..).zip(padded.chunks(CHUNK_BYTES)) {
        Library::put(&vault(), &list_chunk_name(&id, chunk), piece.to_vec()).unwrap();
    }
    library.pretend_index(&vec![KeptItem {
        id: id.clone(),
        sha256: hex(&Sha256::digest(&padded)),
        list_chunks: LIST_CHUNKS,
        ..item(ItemKind::Folder, 2, 4)
    }]);
    assert_eq!(library.list_of(&id), Ok(ListOut::Damaged));
    // At the most a list may be, it reads.
    let mut fits = serde_json::to_vec(&vec![entry("a", 2), entry("b", 2)]).unwrap();
    fits.resize(usize::try_from(LIST_BYTES).unwrap(), b' ');
    for (chunk, piece) in (0_u64..).zip(fits.chunks(CHUNK_BYTES)) {
        Library::put(&vault(), &list_chunk_name(&id, chunk), piece.to_vec()).unwrap();
    }
    library.pretend_index(&vec![KeptItem {
        id: id.clone(),
        sha256: hex(&Sha256::digest(&fits)),
        list_chunks: LIST_CHUNKS,
        ..item(ItemKind::Folder, 2, 4)
    }]);
    assert!(matches!(library.list_of(&id), Ok(ListOut::Whole(_))));
}

/// A folder's paths are the broker's rules': each step plain, none a
/// device's or one Windows acts on, no two the same but for case, none a
/// file where another is a folder, and bounded.
#[test]
fn a_folders_paths_are_held_to_the_brokers_rules() {
    let deepest = vec!["d"; FOLDER_DEPTH].join("/");
    let longest = format!(
        "{}/{}",
        "a".repeat(STEP_BYTES),
        "b".repeat(PATH_BYTES - STEP_BYTES - 1)
    );
    assert_eq!(longest.len(), PATH_BYTES);
    let good = [
        "index.html",
        "a b/c-d_e.f~!@#$%^&()+=,;'[]{}`",
        ".gitignore",
        ".github/workflows/ci.yml",
        "a/b",
        "a/c",
        "CONSOLE.txt",
        deepest.as_str(),
    ];
    assert_eq!(check_paths(good), Ok(()));
    assert_eq!(
        check_paths([longest.as_str()]),
        Err("a name in a folder is longer than 255 bytes")
    );
    let step = "s".repeat(STEP_BYTES);
    assert_eq!(check_paths([step.as_str()]), Ok(()));
    let too_deep = format!("{deepest}/d");
    let too_long = format!(
        "{}/{}",
        "a".repeat(STEP_BYTES),
        "b".repeat(PATH_BYTES - STEP_BYTES)
    );
    let long_step = "s".repeat(STEP_BYTES + 1);
    for (paths, why) in [
        (vec![""], "empty, . or .."),
        (vec!["a//b"], "empty, . or .."),
        (vec!["/a"], "empty, . or .."),
        (vec!["a/"], "empty, . or .."),
        (vec!["a/./b"], "empty, . or .."),
        (vec!["../a"], "empty, . or .."),
        (vec![too_deep.as_str()], "16 steps"),
        (vec![too_long.as_str()], "1024 bytes"),
        (vec![long_step.as_str()], "255 bytes"),
        (vec!["caf\u{e9}"], "plain ASCII"),
        (vec!["a\tb"], "plain ASCII"),
        (vec!["a\\b"], "Windows forbids"),
        (vec!["a:b"], "Windows forbids"),
        (vec!["a*"], "Windows forbids"),
        (vec!["a?"], "Windows forbids"),
        (vec!["a\"b"], "Windows forbids"),
        (vec!["a<b"], "Windows forbids"),
        (vec!["a|b"], "Windows forbids"),
        (vec![" a"], "space"),
        (vec!["a "], "space"),
        (vec!["dir./a"], "dot"),
        (vec!["CON"], "device"),
        (vec!["CON .txt"], "device"),
        (vec!["x/nul.txt"], "device"),
        (vec!["com1/a"], "device"),
        (vec!["x/Desktop.ini"], "acts on"),
        (vec!["a/b.LNK"], "acts on"),
        (vec!["a.url"], "acts on"),
        (vec!["Read.me", "read.ME"], "same but for case"),
        (vec!["a", "a/b"], "a file where another has a folder"),
        (vec!["x/A/b", "x/a"], "a file where another has a folder"),
    ] {
        let said = check_paths(paths.iter().copied()).unwrap_err();
        assert!(said.contains(why), "{paths:?}: {said}");
    }
    // A folder whose paths break a rule isn't kept, and the agent's path
    // isn't said.
    let (config, _scratch, key) = kept("folder-paths");
    let library = Library::of(&config, &key).unwrap();
    let conversation = library.conversation().unwrap();
    let spent = library
        .spend(&Claim::folder(1, 1), conversation)
        .unwrap()
        .unwrap();
    let said = library
        .keep_folder_spent(
            "bad",
            &[("secret-ish/../x", b"x")],
            LeftOut::default(),
            "now",
            spent,
            REPLY,
        )
        .unwrap_err();
    assert!(said.contains("empty, . or .."), "{said}");
    assert!(!said.contains("secret-ish"), "a path was said: {said}");
    assert!(library.items().unwrap().is_empty());
}

/// A folder is spent before it is read, all its files and bytes at once:
/// one past any bound is refused and counts nothing; one spent counts one
/// hand-over and its bytes.
#[test]
fn a_folder_is_spent_whole_or_not_at_all() {
    let (config, _scratch, key) = kept("folder-spend");
    let library = Library::of(&config, &key).unwrap();
    let conversation = library.conversation().unwrap();
    for refused in [
        Claim::folder(FOLDER_FILES + 1, 1),
        Claim::folder(1, FOLDER_BYTES + 1),
        Claim::folder(0, 0),
    ] {
        assert_eq!(
            library.spend(&refused, conversation),
            Ok(Err(Refused::FolderTooLarge)),
            "{refused:?}"
        );
    }
    assert_eq!(library.standing().unwrap(), (0, false));
    assert_eq!(library.read_so_far().unwrap(), 0);
    let most = Claim::folder(FOLDER_FILES, FOLDER_BYTES);
    for _ in 0..4 {
        assert!(matches!(library.spend(&most, conversation), Ok(Ok(_))));
    }
    assert_eq!(library.read_so_far().unwrap(), READ_PER_CONVERSATION);
    // The conversation's bytes are spent: a folder is refused before it is
    // read, and so is a file.
    assert_eq!(
        library.spend(&Claim::folder(1, 1), conversation),
        Ok(Err(Refused::ReadOut))
    );
    assert_eq!(library.refuses(&Claim::file(0)), Ok(Some(Refused::ReadOut)));
    assert_eq!(library.standing().unwrap(), (4, false));
    // A new conversation reads again.
    library.new_conversation().unwrap();
    assert_eq!(library.read_so_far().unwrap(), 0);
    assert_eq!(library.refuses(&Claim::file(0)), Ok(None));
}

/// A file is spent once read, and its bytes count whatever comes of it:
/// a loop of files the Library refuses ends at the conversation's bytes,
/// refused before reading.
#[test]
fn every_byte_a_file_reads_counts_and_ends_a_loop_of_refusals() {
    let (config, _scratch, key) = kept("file-read");
    let library = Library::of(&config, &key).unwrap();
    library.pretend_held(&[1; LIBRARY_ITEMS]);
    let conversation = library.conversation().unwrap();
    let mut tries = 0;
    while library.refuses(&Claim::file(0)).unwrap() == Some(Refused::Full) {
        assert_eq!(
            library.spend(&Claim::file(FILE_BYTES), conversation),
            Ok(Err(Refused::Full))
        );
        tries += 1;
        assert!(
            tries <= 16,
            "the loop went on past the conversation's bytes"
        );
    }
    assert_eq!(tries, 16);
    assert_eq!(library.refuses(&Claim::file(0)), Ok(Some(Refused::ReadOut)));
    assert_eq!(library.read_so_far().unwrap(), READ_PER_CONVERSATION);
    assert_eq!(
        library.standing().unwrap(),
        (0, false),
        "a refusal spent a hand-over"
    );
    // Past the most, the count stays at it.
    assert_eq!(
        library.spend(&Claim::file(5), conversation),
        Ok(Err(Refused::ReadOut))
    );
    assert_eq!(library.read_so_far().unwrap(), READ_PER_CONVERSATION);
    // A stale file counts nothing in the new conversation.
    library.new_conversation().unwrap();
    assert_eq!(
        library.spend(&Claim::file(5), conversation),
        Ok(Err(Refused::Stale))
    );
    assert_eq!(library.read_so_far().unwrap(), 0);
}

/// A folder keep cut short after its bytes and before its list is undone,
/// both runs of chunks removed, by what the pending keep says alone.
#[test]
fn a_folder_keep_cut_short_is_undone_both_runs() {
    let (config, _scratch, key) = kept("folder-cut");
    let library = Library::of(&config, &key).unwrap();
    let id = "e".repeat(64);
    leave_state(
        &library,
        &State {
            pending: Some(Pending {
                op: Op::Keep,
                id: id.clone(),
                chunks: 2,
                list_chunks: 2,
            }),
            handed: 1,
            ..State::default()
        },
    );
    let vault = library.vault().unwrap();
    for chunk in 0..2 {
        Library::put(&vault, &chunk_name(&id, chunk), vec![1; CHUNK_BYTES]).unwrap();
    }
    Library::put(&vault, &list_chunk_name(&id, 0), vec![2; CHUNK_BYTES]).unwrap();
    let logged = crate::test_log::captured(|| {
        assert!(library.items().unwrap().is_empty());
    });
    for chunk in 0..2 {
        assert!(!has_chunk(&library, &id, chunk), "chunk {chunk}");
        let list = Library::name(&list_chunk_name(&id, chunk)).unwrap();
        assert!(!vault.exists(&list), "list chunk {chunk}");
    }
    assert_eq!(library.standing().unwrap(), (1, false));
    // Undone quietly: once, not a line a chunk.
    assert_eq!(logged.matches("undid an item").count(), 1, "{logged}");
    assert!(!logged.contains("removed secret"), "{logged}");
}

/// The Library's chunks are written and removed without an info line
/// each; each item kept or removed is logged once, by its id, never its
/// name or a path.
#[test]
fn the_library_logs_an_item_once_and_never_its_names() {
    let (config, _scratch, key) = kept("quiet");
    let library = Library::of(&config, &key).unwrap();
    let files = folder_of(&[("named-by-the-agent/a.bin", 3 * CHUNK_BYTES), ("b", 5)]);
    let mut kept = None;
    let logged = crate::test_log::captured(|| {
        kept = Some(keep_folder(&library, "agent-folder", &files));
    });
    let info: Vec<&str> = logged
        .lines()
        .filter(|line| line.contains(" INFO"))
        .collect();
    assert_eq!(
        info.iter()
            .filter(|line| line.contains("the Library kept an item"))
            .count(),
        1,
        "{logged}"
    );
    assert!(
        !info.iter().any(|line| line.contains("library-")
            && !line.contains("library-index")
            && !line.contains("library-state")),
        "a chunk was logged at info: {logged}"
    );
    for name in ["agent-folder", "named-by-the-agent", "a.bin"] {
        assert!(!logged.contains(name), "{name} was logged: {logged}");
    }
    let id = kept.unwrap().id;
    let logged = crate::test_log::captured(|| {
        assert!(library.remove(&id).unwrap());
    });
    let info: Vec<&str> = logged
        .lines()
        .filter(|line| line.contains(" INFO"))
        .collect();
    assert_eq!(
        info.iter()
            .filter(|line| line.contains("removed an item"))
            .count(),
        1,
        "{logged}"
    );
    assert!(
        !info.iter().any(|line| line.contains("removed secret")),
        "a chunk's removal was logged at info: {logged}"
    );
}

/// An index entry kept before items had kinds reads as a file of the
/// first conversation, from no reply; a pending change from before lists
/// no chunks.
#[test]
fn an_index_and_a_state_from_before_items_read_as_files() {
    let (config, _scratch, key) = kept("old-index");
    let library = Library::of(&config, &key).unwrap();
    let bytes = b"from before".to_vec();
    let file = keep(&library, "old.txt", &bytes);
    library.pretend_index(&serde_json::json!([{
        "id": file.id,
        "name": "old.txt",
        "bytes": bytes.len(),
        "sha256": file.sha256,
        "chunks": 1,
        "kept_at": "2026-11-16T00:00:00Z",
    }]));
    let items = library.items().unwrap();
    let old = items.first().unwrap();
    assert_eq!(
        (
            old.kind,
            old.files,
            old.list_chunks,
            old.conversation,
            old.reply.as_str()
        ),
        (ItemKind::File, 1, 0, 0, "")
    );
    assert_eq!(library.fetch(&file.id).unwrap(), Some(bytes));
    Library::write(
        &library.vault().unwrap(),
        STATE,
        &serde_json::json!({"pending": {"op": "remove", "id": file.id, "chunks": 1}, "handed": 0, "off": false}),
    )
    .unwrap();
    assert!(library.items().unwrap().is_empty());
}

/// The index of the most items, each of the longest fields, fits one
/// sealed entry; the longest list fits its chunks; the last chunk's names
/// are entry names.
#[test]
fn the_largest_index_and_list_fit_where_they_are_kept() {
    let longest = KeptItem {
        id: "f".repeat(64),
        kind: ItemKind::Folder,
        name: "n".repeat(NAME_BYTES),
        bytes: FOLDER_BYTES,
        files: FOLDER_FILES,
        sha256: "f".repeat(64),
        chunks: chunks_of(FOLDER_BYTES),
        list_chunks: LIST_CHUNKS,
        kept_at: "t".repeat(KEPT_AT_BYTES),
        conversation: u64::MAX,
        reply: String::from(REPLY),
        left_out: LeftOut {
            links: u32::MAX,
            mounts: u32::MAX,
            others: u32::MAX,
        },
    };
    let index = serde_json::to_vec(&vec![longest; LIBRARY_ITEMS]).unwrap();
    assert!(index.len() <= CHUNK_BYTES, "{} bytes", index.len());
    let path = format!(
        "{}/{}",
        "p".repeat(STEP_BYTES),
        "q".repeat(PATH_BYTES - STEP_BYTES - 1)
    );
    let list: Vec<FolderFile> = (0..FOLDER_FILES)
        .map(|_| FolderFile {
            path: path.clone(),
            bytes: u64::MAX,
            sha256: "f".repeat(64),
        })
        .collect();
    let list = serde_json::to_vec(&list).unwrap();
    assert!(
        u64::try_from(list.len()).unwrap() <= LIST_BYTES,
        "{} bytes",
        list.len()
    );
    let id = "f".repeat(64);
    assert!(Library::name(&chunk_name(&id, chunks_of(FOLDER_BYTES) - 1)).is_ok());
    assert!(Library::name(&list_chunk_name(&id, LIST_CHUNKS - 1)).is_ok());
}

/// A conversation the chat doesn't resume at the broker's start is grouped
/// apart in the Library; one it resumes isn't. Either way, a restart gives
/// nothing back: the count, the bytes read and off are carried.
#[test]
fn a_conversation_not_resumed_at_start_is_grouped_apart_and_gets_nothing_back() {
    use crate::conversation_store::{Resumed, goes_on};
    use hatchling_core::conversation::{ChatExchange, Conversation};
    let (config, _scratch, key) = kept("at-start");
    let library = Library::of(&config, &key).unwrap();
    keep(&library, "before", b"x");
    library.turn_off().unwrap();
    let before = library.conversation().unwrap();
    let read = library.read_so_far().unwrap();
    at_start(&Once::new(), &config, &key, true);
    assert_eq!(library.conversation().unwrap(), before);
    // Once for the broker's life, however often it's asked.
    let once = Once::new();
    at_start(&once, &config, &key, false);
    at_start(&once, &config, &key, false);
    assert_eq!(library.conversation().unwrap(), before + 1);
    assert_eq!(
        library.standing().unwrap(),
        (1, true),
        "a restart gave something back"
    );
    assert_eq!(library.read_so_far().unwrap(), read);
    // Only the owner's new conversation clears them.
    library.new_conversation().unwrap();
    assert_eq!(library.standing().unwrap(), (0, false));
    // Which conversations go on: one kept with something said in it.
    let exchange = ChatExchange {
        user: String::from("hi"),
        assistant: String::from("hello"),
    };
    let said = |exchanges: Vec<ChatExchange>, earlier: Vec<String>| {
        Resumed::Kept(Conversation { earlier, exchanges })
    };
    assert!(goes_on(Some(&said(vec![exchange], Vec::new()))));
    assert!(goes_on(Some(&said(
        Vec::new(),
        vec![String::from("before")]
    ))));
    assert!(!goes_on(Some(&said(Vec::new(), Vec::new()))));
    assert!(!goes_on(Some(&Resumed::Unreadable)));
    assert!(!goes_on(None));
}

/// What is kept names a reply that is one, a time that is one and, for a
/// folder, a list no longer than one may be: else nothing is kept.
#[test]
fn a_keep_with_a_reply_a_time_or_a_list_that_cant_be_one_keeps_nothing() {
    let (config, _scratch, key) = kept("keeping");
    let library = Library::of(&config, &key).unwrap();
    let conversation = library.conversation().unwrap();
    let now = "2026-11-18T00:00:00Z";
    let claim = Claim::folder(2, 4);
    let parts: [&[u8]; 2] = [b"ab", b"cd"];
    let keeping = |list: &[u8], now: &str, reply: &str| {
        let spent = library.spend(&claim, conversation).unwrap().unwrap();
        let sha256 = hex(&Sha256::digest(list));
        library.keeping(
            "f",
            &claim,
            &parts,
            Some(list),
            LeftOut::default(),
            sha256,
            now,
            &spent,
            reply,
        )
    };
    let list = |length: u64| {
        let mut list = b"[]".to_vec();
        list.resize(usize::try_from(length).unwrap(), b' ');
        list
    };
    let long = "t".repeat(KEPT_AT_BYTES + 1);
    for (now, reply) in [
        (now, ""),
        (now, "session-0123456789abcdef0123"),
        (now, "run-0123456789ABCDEF01234567"),
        (long.as_str(), REPLY),
        ("2026-11-18 00:00:00", REPLY),
    ] {
        assert!(keeping(&list(2), now, reply).is_err(), "{now:?} {reply:?}");
    }
    assert!(keeping(&list(LIST_BYTES + 1), now, REPLY).is_err());
    assert_eq!(library.items().unwrap(), Vec::new(), "something was kept");
    let kept = keeping(&list(LIST_BYTES), now, REPLY).unwrap().unwrap();
    assert_eq!(kept.list_chunks, LIST_CHUNKS);
    assert_eq!(library.items().unwrap(), vec![kept]);
}

/// A Library full by its bytes refuses before anything is read, so a loop
/// of tiny files the Library has no room for ends before it starts.
#[test]
fn a_library_full_by_its_bytes_refuses_before_reading() {
    let (config, _scratch, key) = kept("byte-full");
    let library = Library::of(&config, &key).unwrap();
    library.pretend_held(&[FILE_BYTES; 32]);
    assert_eq!(library.refuses(&Claim::file(0)), Ok(Some(Refused::Full)));
    library.pretend_held(&[FILE_BYTES; 31]);
    assert_eq!(library.refuses(&Claim::file(0)), Ok(None));
}

/// A store of secrets still logs every write and removal: only what the
/// Library asks to be quiet is.
#[test]
fn a_secrets_store_still_logs_every_write_and_removal() {
    let (config, _scratch, key) = kept("loud");
    let library = Library::of(&config, &key).unwrap();
    let vault = library.vault().unwrap();
    let name = Library::name("loud-entry").unwrap();
    let logged = crate::test_log::captured(|| {
        vault
            .store(&name, &Secret::new(b"x".to_vec()).unwrap())
            .unwrap();
        vault.remove(&name).unwrap();
    });
    let info: Vec<&str> = logged
        .lines()
        .filter(|line| line.contains(" INFO"))
        .collect();
    assert!(
        info.iter()
            .any(|line| line.contains("stored sealed secret") && line.contains("loud-entry")),
        "{logged}"
    );
    assert!(
        info.iter()
            .any(|line| line.contains("removed secret") && line.contains("loud-entry")),
        "{logged}"
    );
}
