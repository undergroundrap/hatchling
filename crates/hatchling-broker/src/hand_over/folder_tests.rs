//! Handing a folder over, with a stand-in Room holding a tree, stand-in
//! checks and a real Library, Activity log and app stream in a scratch
//! directory.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use hatchling_core::LeftOut;
use hatchling_core::room::Kind;

use super::tests::{Install, Says, chat, clear, install};
use super::*;
use crate::folder_walk::tests::Tree;
use crate::folder_walk::{CASE_TWINS, EMPTY};
use crate::library::{FILE_BYTES, ItemKind, ReadOut};

const ROOT: &str = "/workspace/site";

/// Checks that remember each text they were asked of, and find a secret
/// in the folder's own name when told to, and in any other text holding
/// `needle`.
struct Finds {
    name: Vec<u8>,
    name_holds: bool,
    needle: Option<Vec<u8>>,
    seen: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl Checks for Finds {
    fn holds(&self, text: &[u8]) -> Result<bool, String> {
        self.seen.borrow_mut().push(text.to_vec());
        if text == self.name.as_slice() {
            return Ok(self.name_holds);
        }
        Ok(self.needle.as_ref().is_some_and(|needle| {
            text.windows(needle.len())
                .any(|window| window == needle.as_slice())
        }))
    }
}

/// A folder `site` of three files in two folders, a link and a pipe.
fn site() -> Tree {
    let mut tree = Tree::of(
        ROOT,
        &[
            ("index.html", b"<p>hi</p>"),
            ("css/site.css", b"b {}"),
            ("css/img/a.bin", &[0, 1, 2]),
        ],
    );
    tree.entry(ROOT, b"latest", Kind::Link, 0, 0);
    tree.entry(&format!("{ROOT}/css"), b"fifo", Kind::Other, 0, 0);
    tree
}

/// The text the folder `site` is checked as.
fn site_text() -> Vec<u8> {
    b"index.html\ncss/site.css\ncss/img/a.bin\n\n<p>hi</p>b {}\x00\x01\x02".to_vec()
}

/// Checks over `install`'s run that find `needle`, the folder's name
/// holding a secret when `name_holds`, and what they were asked.
fn finds<'a>(
    install: &'a Install,
    needle: Option<&[u8]>,
    name_holds: bool,
) -> (Handing<'a>, Rc<RefCell<Vec<Vec<u8>>>>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut handing = install.handing(Some(&install.cell), chat, clear("site"));
    handing.checks = Box::new(Finds {
        name: b"site".to_vec(),
        name_holds,
        needle: needle.map(<[u8]>::to_vec),
        seen: Rc::clone(&seen),
    });
    (handing, seen)
}

/// A folder is walked, spent as found, each file read once, its name
/// checked alone and the rest as one text, and kept as one item grouped
/// by the broker, with what was left out; Activity keeps it by name and
/// size, and the app is told nothing yet.
#[test]
fn a_folder_is_walked_read_checked_once_and_kept_as_one_item() {
    let install = install("folder");
    let tree = site();
    let (handing, seen) = finds(&install, None, false);
    let handed = handing.hand_folder(&tree, "site");
    let left_out = LeftOut {
        links: 1,
        mounts: 0,
        others: 1,
    };
    assert_eq!(
        handed,
        RoomResult::FolderHandedOver {
            name: String::from("site"),
            files: 3,
            bytes: 16,
            left_out,
        }
    );
    assert_eq!(
        tree.asked(),
        [
            "list /workspace/site",
            "list /workspace/site/css",
            "list /workspace/site/css/img",
            "take /workspace/site/index.html",
            "take /workspace/site/css/site.css",
            "take /workspace/site/css/img/a.bin",
        ]
    );
    assert_eq!(*seen.borrow(), [b"site".to_vec(), site_text()]);
    let library = install.library();
    let items = library.items().unwrap();
    let [item] = items.as_slice() else {
        unreachable!("{items:?}")
    };
    assert_eq!(
        (item.kind, item.name.as_str(), item.files, item.bytes),
        (ItemKind::Folder, "site", 3, 16)
    );
    assert_eq!(item.left_out, left_out);
    assert_eq!(
        (item.conversation, item.reply.as_str()),
        (library.conversation().unwrap(), install.cell.as_str())
    );
    for (n, bytes) in [b"<p>hi</p>".as_slice(), b"b {}", &[0, 1, 2]]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            library.read_out_in(&item.id, n).unwrap(),
            ReadOut::Whole(zeroize::Zeroizing::new(bytes.to_vec()))
        );
    }
    assert_eq!(library.standing().unwrap(), (1, false));
    assert_eq!(library.read_so_far().unwrap(), 16);
    // The list of files has none; every app is told to read the Library
    // again, and Activity counts its files.
    assert!(library.files().unwrap().is_empty());
    assert_eq!(install.events(), [UiEventBody::LibraryChanged {}]);
    assert_eq!(
        install.crossings(),
        [(Some(String::from("site")), 16, FileOutEnded::Offered)]
    );
    let activity = std::fs::read_to_string(install.config.scheduler.activity_log()).unwrap();
    assert!(activity.contains(r#""files":3"#), "{activity}");
}

/// What is refused before the walk reads nothing, lists nothing, spends
/// nothing and leaves no line in Activity.
#[test]
fn a_folder_refused_before_its_walk_costs_nothing() {
    let install = install("folder-unread");
    let library = install.library();
    let conversation = library.conversation().unwrap();
    for _ in 0..crate::library::HANDED_PER_CONVERSATION {
        library
            .spend(&Claim::file(0), conversation)
            .unwrap()
            .unwrap();
    }
    let (handing, _) = finds(&install, None, false);
    let tree = site();
    assert_eq!(
        handing.hand_folder(&tree, "site"),
        unread(Refused::Spent.words())
    );
    library.new_conversation().unwrap();
    library
        .turn_off_in(library.conversation().unwrap())
        .unwrap();
    assert_eq!(
        handing.hand_folder(&tree, "site"),
        unread(Refused::Off.words())
    );
    library.new_conversation().unwrap();
    for (path, why) in [
        ("site/", "a folder's path has a step that is empty, . or .."),
        (
            "/etc",
            "a folder is handed over from the workspace: its path starts with /workspace/",
        ),
        ("CON", RESERVED),
        ("site.", TRAILING_DOT),
        ("inbox", THE_INBOX),
        ("/workspace/inbox/0001-a", THE_INBOX),
    ] {
        assert_eq!(handing.hand_folder(&tree, path), unread(why), "{path}");
    }
    // A folder named like the inbox, but not it, is walked.
    assert_eq!(
        handing.hand_folder(&tree, "inboxes"),
        unread("no such folder")
    );
    tree.asked.borrow_mut().clear();
    let stale = install.handing(Some(&install.cell), |_| Some(false), clear("site"));
    assert_eq!(stale.hand_folder(&tree, "site"), unread(STALE));
    let other = install.handing(None, chat, clear("site"));
    assert_eq!(other.hand_folder(&tree, "site"), unread(NOT_THE_CHAT));
    assert!(tree.asked().is_empty(), "{:?}", tree.asked());
    assert_eq!(library.standing().unwrap(), (0, false));
    assert!(install.crossings().is_empty());
}

/// What the walk refuses is refused before anything is spent or read:
/// the Room's names are the agent's own, and no check is made.
#[test]
fn a_folder_its_walk_refuses_is_never_spent_read_or_checked() {
    let install = install("folder-walk");
    let (handing, seen) = finds(&install, None, false);
    let twins = Tree::of(ROOT, &[("A.txt", b"a"), ("a.txt", b"b")]);
    assert_eq!(handing.hand_folder(&twins, "site"), unread(CASE_TWINS));
    let empty = Tree::of(ROOT, &[]);
    assert_eq!(handing.hand_folder(&empty, "site"), unread(EMPTY));
    assert!(
        twins.asked().iter().all(|asked| asked.starts_with("list ")),
        "{:?}",
        twins.asked()
    );
    assert!(seen.borrow().is_empty());
    assert_eq!(install.library().standing().unwrap(), (0, false));
    assert_eq!(install.library().read_so_far().unwrap(), 0);
    assert!(install.crossings().is_empty());
}

/// A folder the conversation's bytes have no room for is refused when it
/// is spent: nothing read, and Activity keeps no name.
#[test]
fn a_folder_past_the_conversations_bytes_is_refused_unread() {
    let install = install("folder-bytes");
    let library = install.library();
    let conversation = library.conversation().unwrap();
    for _ in 0..13 {
        library
            .spend(&Claim::file(FILE_BYTES), conversation)
            .unwrap()
            .unwrap();
    }
    let mut tree = Tree::of(ROOT, &[]);
    for name in ["a", "b", "c", "d"] {
        tree.entry(ROOT, name.as_bytes(), Kind::File, 9, FILE_BYTES);
    }
    let (handing, seen) = finds(&install, None, false);
    assert_eq!(
        handing.hand_folder(&tree, "site"),
        RoomResult::NotHandedOver {
            reason: String::from(Refused::ReadOut.words())
        }
    );
    assert!(
        tree.asked().iter().all(|asked| asked.starts_with("list ")),
        "{:?}",
        tree.asked()
    );
    assert!(seen.borrow().is_empty());
    assert_eq!(install.crossings(), [(None, 0, FileOutEnded::Refused)]);
}

/// Each file is held to its listed length as soon as it is read: one that
/// isn't stops the reads there, and the folder, spent, is refused whole,
/// unchecked; the conversation has read what was spent.
#[test]
fn a_file_unlike_its_listing_stops_the_reads_and_refuses_the_folder() {
    for bytes in [b"<p>hi</p>!".as_slice(), b"<p>"] {
        let install = install("folder-length");
        let mut tree = site();
        tree.files
            .insert(format!("{ROOT}/index.html"), Ok(bytes.to_vec()));
        let (handing, seen) = finds(&install, None, false);
        assert_eq!(handing.hand_folder(&tree, "site"), unread(CHANGED));
        assert_eq!(
            tree.asked().last().unwrap(),
            "take /workspace/site/index.html",
            "a take after the change"
        );
        assert!(seen.borrow().is_empty());
        let library = install.library();
        assert!(library.items().unwrap().is_empty());
        assert_eq!(library.standing().unwrap(), (1, false));
        // What was spent, and what the file read past its listed 9 bytes.
        let past = u64::try_from(bytes.len().saturating_sub(9)).unwrap();
        assert_eq!(library.read_so_far().unwrap(), 16 + past);
        let read = u64::try_from(bytes.len()).unwrap();
        assert_eq!(install.crossings(), [(None, read, FileOutEnded::Refused)]);
    }
}

/// A file the Room won't hand back, once the folder is spent, refuses it
/// whole in the Room's words, and Activity keeps no name.
#[test]
fn a_file_the_room_wont_hand_back_refuses_the_folder() {
    let install = install("folder-take");
    let mut tree = site();
    tree.files.insert(
        format!("{ROOT}/css/site.css"),
        Err(String::from("not a file")),
    );
    let (handing, _) = finds(&install, None, true);
    assert_eq!(handing.hand_folder(&tree, "site"), unread("not a file"));
    assert!(install.library().items().unwrap().is_empty());
    assert_eq!(install.library().standing().unwrap(), (1, false));
    assert_eq!(install.crossings(), [(None, 9, FileOutEnded::Refused)]);
}

/// A secret anywhere withholds the whole folder: in its last file, in a
/// path deep in it, or split across two files' bytes. Nothing is kept,
/// giving is off for the conversation, the owner is told by the folder's
/// name, and Activity keeps it withheld.
#[test]
fn a_secret_anywhere_in_a_folder_withholds_all_of_it() {
    for needle in [b"\x01\x02".as_slice(), b"css/img/a.bin", b"</p>b {"] {
        let install = install("folder-held");
        let (handing, _) = finds(&install, Some(needle), false);
        assert_eq!(handing.hand_folder(&site(), "site"), unread(WITHHELD));
        let library = install.library();
        assert!(library.items().unwrap().is_empty());
        assert_eq!(library.standing().unwrap(), (1, true));
        let events = install.events();
        let [UiEventBody::FileWithheld(withheld)] = events.as_slice() else {
            unreachable!("{events:?}");
        };
        assert_eq!(withheld.name.as_deref(), Some("site"));
        assert_eq!(
            install.crossings(),
            [(Some(String::from("site")), 16, FileOutEnded::Withheld)]
        );
        // Off: a file isn't read either, nor another folder.
        let room = tests::Room::with(Ok(b"x".to_vec()));
        handing.hand(&room, "a.txt");
        assert!(room.asked().is_empty());
        let tree = site();
        assert_eq!(
            handing.hand_folder(&tree, "site"),
            unread(Refused::Off.words())
        );
        assert!(tree.asked().is_empty());
    }
}

/// A folder whose own name holds a secret is withheld, and never named:
/// not to the owner, not in Activity.
#[test]
fn a_folder_whose_name_holds_a_secret_is_never_named() {
    let install = install("folder-name-held");
    let (handing, _) = finds(&install, None, true);
    assert_eq!(handing.hand_folder(&site(), "site"), unread(WITHHELD));
    let events = install.events();
    let [UiEventBody::FileWithheld(withheld)] = events.as_slice() else {
        unreachable!("{events:?}");
    };
    assert_eq!(withheld.name, None);
    assert_eq!(install.crossings(), [(None, 16, FileOutEnded::Withheld)]);
}

/// A folder that couldn't be checked keeps nothing, turns nothing off,
/// and is never told as a secret found.
#[test]
fn a_folder_that_couldnt_be_checked_is_neither_kept_nor_held() {
    let install = install("folder-unchecked");
    let handing = install.handing(
        Some(&install.cell),
        chat,
        Says {
            name: b"site".to_vec(),
            for_name: Ok(false),
            for_file: Err(String::from("no keeper")),
        },
    );
    assert_eq!(handing.hand_folder(&site(), "site"), unread(UNCHECKED));
    let library = install.library();
    assert!(library.items().unwrap().is_empty());
    assert_eq!(library.standing().unwrap(), (1, false));
    assert!(install.events().is_empty());
    assert_eq!(install.crossings(), [(None, 16, FileOutEnded::Unchecked)]);
}

/// A file given before turns giving off for a folder, and a folder held
/// for a file: either way, nothing more is read.
#[test]
fn a_file_withheld_turns_off_giving_a_folder() {
    let install = install("file-then-folder");
    let handing = install.handing(
        Some(&install.cell),
        chat,
        Says {
            name: b"a.txt".to_vec(),
            for_name: Ok(false),
            for_file: Ok(true),
        },
    );
    let room = tests::Room::with(Ok(b"secret".to_vec()));
    assert_eq!(handing.hand(&room, "a.txt"), unread(WITHHELD));
    let tree = site();
    assert_eq!(
        handing.hand_folder(&tree, "site"),
        unread(Refused::Off.words())
    );
    assert!(tree.asked().is_empty());
}

/// The deadline holds the walk, the time before the spend, and each read.
#[test]
fn a_folder_that_takes_too_long_is_refused_where_it_is() {
    use crate::folder_walk::TOO_SLOW;
    // Out of time before the walk lists anything.
    let install = install("folder-slow");
    let (mut handing, _) = finds(&install, None, false);
    handing.folder_time = Duration::ZERO;
    let tree = site();
    assert_eq!(handing.hand_folder(&tree, "site"), unread(TOO_SLOW));
    assert!(tree.asked().is_empty());
    // Out of time once the walk is done: nothing spent.
    let mut tree = Tree::of(ROOT, &[("a.txt", b"a")]);
    tree.slow = Duration::from_millis(60);
    handing.folder_time = Duration::from_millis(30);
    assert_eq!(handing.hand_folder(&tree, "site"), unread(TOO_SLOW));
    assert_eq!(install.library().standing().unwrap(), (0, false));
    // Out of time between reads: spent, and refused there. Two seconds
    // are ample for a walk and a spend; the first read takes longer.
    let mut tree = Tree::of(ROOT, &[("a.txt", b"a"), ("b.txt", b"bb")]);
    tree.slow_take = Duration::from_secs(3);
    handing.folder_time = Duration::from_secs(2);
    assert_eq!(handing.hand_folder(&tree, "site"), unread(TOO_SLOW));
    assert_eq!(
        tree.asked(),
        ["list /workspace/site", "take /workspace/site/a.txt"]
    );
    assert_eq!(install.library().standing().unwrap(), (1, false));
    assert_eq!(install.crossings(), [(None, 1, FileOutEnded::Refused)]);
}

/// A Room whose first file is read while the owner starts a new
/// conversation.
struct Bumping<'a> {
    tree: Tree,
    library: Library<'a>,
}

impl Rooms for Bumping<'_> {
    fn run(
        &self,
        run: &crate::room_job::RoomRun,
    ) -> Result<crate::room_job::RoomJobAnswer, String> {
        self.tree.run(run)
    }
    fn job(
        &self,
        job: &hatchling_core::room::JobId,
        wait: u32,
    ) -> Result<crate::room_job::RoomJobAnswer, String> {
        self.tree.job(job, wait)
    }
    fn stop(
        &self,
        job: &hatchling_core::room::JobId,
    ) -> Result<crate::room_job::RoomJobAnswer, String> {
        self.tree.stop(job)
    }
    fn output(
        &self,
        job: &hatchling_core::room::JobId,
        stream: hatchling_core::room::Stream,
        end: hatchling_core::room::End,
    ) -> Result<Vec<u8>, String> {
        self.tree.output(job, stream, end)
    }
    fn page(
        &self,
        path: &str,
        from: u64,
        lines: u32,
    ) -> Result<hatchling_core::room::Page, String> {
        self.tree.page(path, from, lines)
    }
    fn search(
        &self,
        search: &crate::spawner::RoomSearch,
    ) -> Result<hatchling_core::room::Found, String> {
        self.tree.search(search)
    }
    fn change(
        &self,
        change: &crate::spawner::RoomChange,
    ) -> Result<crate::spawner::RoomChanged, String> {
        self.tree.change(change)
    }
    fn take(&self, path: &str) -> Result<Vec<u8>, String> {
        if self
            .tree
            .asked()
            .iter()
            .all(|asked| !asked.starts_with("take "))
        {
            self.library.new_conversation().unwrap();
        }
        self.tree.take(path)
    }
    fn list(&self, path: &str) -> Result<crate::spawner::Listing, String> {
        self.tree.list(path)
    }
}

/// A folder read while the owner starts a new conversation is kept in
/// neither, and turns neither off.
#[test]
fn a_folder_read_as_a_new_conversation_starts_is_not_kept() {
    let install = install("folder-bump");
    let room = Bumping {
        tree: site(),
        library: install.library(),
    };
    let (handing, _) = finds(&install, None, false);
    assert_eq!(
        handing.hand_folder(&room, "site"),
        unread(Refused::Stale.words())
    );
    let library = install.library();
    assert!(library.items().unwrap().is_empty());
    assert_eq!(library.standing().unwrap(), (0, false));
    // A secret found in it turns off the conversation it was spent in,
    // never the new one.
    let room = Bumping {
        tree: site(),
        library: install.library(),
    };
    let (handing, _) = finds(&install, Some(b"b {}"), false);
    assert_eq!(handing.hand_folder(&room, "site"), unread(WITHHELD));
    assert!(library.items().unwrap().is_empty());
    assert_eq!(library.standing().unwrap(), (0, false));
}

/// The workspace itself may be given: its inbox, the owner's, is left out
/// and counted, and the folder is named for it.
#[test]
fn the_workspace_is_given_without_its_inbox() {
    let install = install("folder-workspace");
    let mut tree = Tree::of(WORKSPACE_ROOT, &[("a.txt", b"a"), ("inbox/b.txt", b"b")]);
    tree.entry(WORKSPACE_ROOT, b"inbox", Kind::Folder, 77, 0);
    let handing = install.handing(Some(&install.cell), chat, clear("workspace"));
    assert_eq!(
        handing.hand_folder(&tree, "/workspace"),
        RoomResult::FolderHandedOver {
            name: String::from("workspace"),
            files: 1,
            bytes: 1,
            left_out: LeftOut {
                links: 0,
                mounts: 1,
                others: 0,
            },
        }
    );
    assert!(
        !tree.asked().iter().any(|asked| asked.contains("inbox")),
        "{:?}",
        tree.asked()
    );
}

const WORKSPACE_ROOT: &str = hatchling_core::room::WORKSPACE;

/// A value too short to look for inside a text is found only as a whole
/// text: a file that is exactly one is withheld by `give_file`, but in a
/// folder it is part of a longer text, and not found. Said, not hidden.
#[test]
fn a_short_secret_is_found_in_a_file_alone_but_not_in_a_folder() {
    let short = zeroize::Zeroizing::new(b"pass123".to_vec());
    assert!(short.len() < crate::held_check::MIN_SCAN_BYTES);
    let values = [short.clone()];
    assert!(crate::held_check::holds(&values, &short));
    let install = install("folder-short");
    let tree = Tree::of(ROOT, &[("a.txt", b"pass123")]);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut handing = install.handing(Some(&install.cell), chat, clear("site"));
    handing.checks = Box::new(Finds {
        name: b"site".to_vec(),
        name_holds: false,
        needle: None,
        seen: Rc::clone(&seen),
    });
    assert!(matches!(
        handing.hand_folder(&tree, "site"),
        RoomResult::FolderHandedOver { .. }
    ));
    let text = seen.borrow().last().unwrap().clone();
    assert_eq!(text, b"a.txt\n\npass123");
    assert!(!crate::held_check::holds(&values, &text));
}

/// A folder's whole text fits one check, at its largest.
#[test]
fn a_folders_largest_text_fits_one_check() {
    let most = crate::library::FOLDER_FILES
        * (u64::try_from(crate::library::PATH_BYTES).unwrap() + 1)
        + 1
        + crate::library::FOLDER_BYTES;
    assert_eq!(FOLDER_CHECK_BYTES, most);
    assert!(most <= crate::held_check::MAX_CHECK_BYTES);
}

/// How many times [`open_once`] was asked.
static OPEN_ASKED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The chat's word: the run is its own when first asked, and not after,
/// as when the owner starts a new conversation while a folder is read.
#[allow(
    clippy::unnecessary_wraps,
    reason = "it stands in for the chat's word, which may be none"
)]
fn open_once(_: &str) -> Option<bool> {
    let asked = OPEN_ASKED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Some(asked == 0)
}

/// A run the chat no longer calls its own once the folder is read keeps
/// nothing, and a secret found then turns nothing off.
#[test]
fn a_folder_whose_run_ended_while_it_was_read_keeps_nothing() {
    let install = install("folder-stale");
    let mut handing = install.handing(Some(&install.cell), open_once, clear("site"));
    let seen = Rc::new(RefCell::new(Vec::new()));
    handing.checks = Box::new(Finds {
        name: b"site".to_vec(),
        name_holds: false,
        needle: Some(b"b {}".to_vec()),
        seen,
    });
    assert_eq!(handing.hand_folder(&site(), "site"), unread(STALE));
    let library = install.library();
    assert!(library.items().unwrap().is_empty());
    assert_eq!(library.standing().unwrap(), (1, false));
    assert!(install.events().is_empty());
    assert_eq!(
        install.crossings(),
        [(Some(String::from("site")), 16, FileOutEnded::Refused)]
    );
}

/// How many times [`open_then_not`] was asked.
static THEN_NOT_ASKED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// As [`open_once`], with a count of its own.
#[allow(
    clippy::unnecessary_wraps,
    reason = "it stands in for the chat's word, which may be none"
)]
fn open_then_not(_: &str) -> Option<bool> {
    let asked = THEN_NOT_ASKED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Some(asked == 0)
}

/// A folder's name whose check failed is never shown, though nothing in
/// it was found: here, its run ended while it was read.
#[test]
fn a_folder_name_whose_check_failed_is_never_shown() {
    let install = install("folder-name-unchecked");
    let handing = install.handing(
        Some(&install.cell),
        open_then_not,
        Says {
            name: b"site".to_vec(),
            for_name: Err(String::from("no keeper")),
            for_file: Ok(false),
        },
    );
    assert_eq!(handing.hand_folder(&site(), "site"), unread(STALE));
    assert_eq!(install.crossings(), [(None, 16, FileOutEnded::Refused)]);
}

/// The broker's own checks find a secret the vault keeps in a folder,
/// split across two of its files; a folder without it is kept.
#[test]
fn the_brokers_checks_find_a_secret_split_across_a_folders_files() {
    let mut install = install("folder-checks");
    install.config.egress.grants = vec![String::from("api.example.com:443")];
    hatchling_vault::SealedVault::open(&install.config.vault.root, &install.key)
        .unwrap()
        .store(
            &"egress-api-example-com-443".parse().unwrap(),
            &hatchling_vault::Secret::new(b"held-credential-value".to_vec()).unwrap(),
        )
        .unwrap();
    let cell: CellId = "run-00000000000000000000f01d".parse().unwrap();
    let hub = hatchling_channel::LocalHub::new(hatchling_channel::OutputGate::disabled());
    crate::follow_ups::began(cell.as_str(), &hub, hub.conversation());
    let handing = Handing::new(&install.config, &install.key, true, &cell, "run-1");
    let split = Tree::of(
        ROOT,
        &[("a.txt", b"x held-credential"), ("b.txt", b"-value y")],
    );
    assert_eq!(handing.hand_folder(&split, "site"), unread(WITHHELD));
    install.library().new_conversation().unwrap();
    let clean = Tree::of(ROOT, &[("a.txt", b"x held-credential"), ("b.txt", b" y")]);
    assert!(matches!(
        handing.hand_folder(&clean, "site"),
        RoomResult::FolderHandedOver { files: 2, .. }
    ));
    let _ = crate::follow_ups::ended(cell.as_str());
}

/// A folder is read into one text, sized once: never copied as it grows.
#[test]
fn a_folder_is_read_into_one_text_sized_once() {
    let tree = site();
    let until = Instant::now() + Duration::from_secs(60);
    let walked = crate::folder_walk::walk(&tree, ROOT, until).unwrap();
    let Ok((text, spans)) = read_folder(&tree, ROOT, &walked, until) else {
        unreachable!("the folder wasn't read");
    };
    assert_eq!(text, site_text());
    assert_eq!(
        text.capacity(),
        text.len(),
        "the text was copied as it grew"
    );
    assert_eq!(spans, [39..48, 48..52, 52..55]);
}

/// A folder has five minutes to give, as its words say.
#[test]
fn a_folder_has_five_minutes() {
    assert_eq!(FOLDER_TIME, Duration::from_secs(300));
    assert!(TOO_SLOW.contains("5 minutes"));
}
