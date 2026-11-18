//! A folder walked through a stand-in Room: a tree of listings and files
//! made by hand.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;

use hatchling_core::room::{End, Entry, Found, JobId, Page, Stream};

use super::*;
use crate::room_job::{RoomJobAnswer, RoomRun};
use crate::spawner::{Listing, RoomChange, RoomChanged, RoomSearch};

/// A stand-in Room holding a tree under one folder: each folder's listing
/// and each file's bytes, by absolute path, and what it was asked, in
/// order (`list <path>`, `take <path>`).
#[derive(Default)]
pub(crate) struct Tree {
    pub lists: BTreeMap<String, Result<Listing, String>>,
    pub files: BTreeMap<String, Result<Vec<u8>, String>>,
    pub asked: RefCell<Vec<String>>,
    /// How long each listing takes.
    pub slow: Duration,
    /// How long each take takes.
    pub slow_take: Duration,
    inos: u64,
}

impl Tree {
    /// A tree whose folder `root` holds `files`, each by its path under
    /// it, with the folders those paths name.
    pub(crate) fn of(root: &str, files: &[(&str, &[u8])]) -> Self {
        let mut tree = Self::default();
        tree.folder(root);
        for (path, bytes) in files {
            tree.file(&whole(root, path), bytes);
        }
        tree
    }

    /// Makes the folder at the absolute `path`, and those it is in under
    /// the first made, if they aren't.
    pub(crate) fn folder(&mut self, path: &str) -> u64 {
        if let Some(Ok(listing)) = self.lists.get(path) {
            return listing.ino;
        }
        self.inos += 1;
        let ino = self.inos;
        if let Some((parent, name)) = path.rsplit_once('/') {
            if self.lists.contains_key(parent) || !self.lists.is_empty() {
                self.folder(parent);
                self.entry(parent, name.as_bytes(), Kind::Folder, ino, 0);
            }
        }
        self.lists.insert(
            path.to_owned(),
            Ok(Listing {
                ino,
                changed: 1,
                entries: Vec::new(),
            }),
        );
        ino
    }

    /// Makes the file at the absolute `path` holding `bytes`.
    pub(crate) fn file(&mut self, path: &str, bytes: &[u8]) {
        let (parent, name) = path.rsplit_once('/').unwrap();
        self.folder(parent);
        self.inos += 1;
        let ino = self.inos;
        let size = u64::try_from(bytes.len()).unwrap();
        self.entry(parent, name.as_bytes(), Kind::File, ino, size);
        self.files.insert(path.to_owned(), Ok(bytes.to_vec()));
    }

    /// Lists `name` in the folder at `parent` as `kind`, of `ino` and
    /// `size`, in the order of the names' bytes.
    pub(crate) fn entry(&mut self, parent: &str, name: &[u8], kind: Kind, ino: u64, size: u64) {
        if let Some(Ok(listing)) = self.lists.get_mut(parent) {
            listing.entries.retain(|entry| entry.name != name);
            listing.entries.push(Entry {
                name: name.to_vec(),
                kind,
                ino,
                size,
            });
            listing.entries.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    /// What it was asked, in order.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }
}

impl Rooms for Tree {
    fn run(&self, _: &RoomRun) -> Result<RoomJobAnswer, String> {
        unreachable!("a folder out runs nothing")
    }
    fn job(&self, _: &JobId, _: u32) -> Result<RoomJobAnswer, String> {
        unreachable!("a folder out runs nothing")
    }
    fn stop(&self, _: &JobId) -> Result<RoomJobAnswer, String> {
        unreachable!("a folder out runs nothing")
    }
    fn output(&self, _: &JobId, _: Stream, _: End) -> Result<Vec<u8>, String> {
        unreachable!("a folder out runs nothing")
    }
    fn page(&self, _: &str, _: u64, _: u32) -> Result<Page, String> {
        unreachable!("a folder out reads no page")
    }
    fn search(&self, _: &RoomSearch) -> Result<Found, String> {
        unreachable!("a folder out searches nothing")
    }
    fn change(&self, _: &RoomChange) -> Result<RoomChanged, String> {
        unreachable!("a folder out changes nothing")
    }
    fn take(&self, path: &str) -> Result<Vec<u8>, String> {
        self.asked.borrow_mut().push(format!("take {path}"));
        std::thread::sleep(self.slow_take);
        self.files
            .get(path)
            .cloned()
            .unwrap_or_else(|| Err(String::from("no such file")))
    }
    fn list(&self, path: &str) -> Result<Listing, String> {
        self.asked.borrow_mut().push(format!("list {path}"));
        std::thread::sleep(self.slow);
        self.lists
            .get(path)
            .cloned()
            .unwrap_or_else(|| Err(String::from("no such folder")))
    }
}

/// Long enough for any walk here.
fn later() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

const ROOT: &str = "/workspace/site";

/// A folder is walked breadth first, each folder listed once, its files
/// in the walk's order with their listed lengths; nothing is read.
#[test]
fn a_folder_is_walked_whole_and_nothing_in_it_is_read() {
    let tree = Tree::of(
        ROOT,
        &[
            ("index.html", b"<p>"),
            ("css/site.css", b"b {}"),
            ("css/deep/x y.txt", b""),
            (".github/ci.yml", b"on: push"),
        ],
    );
    let walked = walk(&tree, ROOT, later()).unwrap();
    assert_eq!(
        walked,
        Walked {
            files: vec![
                (String::from("index.html"), 3),
                (String::from(".github/ci.yml"), 8),
                (String::from("css/site.css"), 4),
                (String::from("css/deep/x y.txt"), 0),
            ],
            bytes: 15,
            left_out: LeftOut::default(),
        }
    );
    assert_eq!(
        tree.asked(),
        [
            "list /workspace/site",
            "list /workspace/site/.github",
            "list /workspace/site/css",
            "list /workspace/site/css/deep",
        ]
    );
}

/// Links, other mounts and anything else are left out and counted, at
/// any depth, whatever their names: none is listed or read.
#[test]
fn what_isnt_a_file_or_folder_is_left_out_and_counted() {
    let mut tree = Tree::of(ROOT, &[("a.txt", b"a"), ("d/b.txt", b"b")]);
    let d = format!("{ROOT}/d");
    tree.entry(ROOT, b"link", Kind::Link, 0, 0);
    tree.entry(&d, b"CON", Kind::Link, 0, 0);
    tree.entry(&d, b"\xff", Kind::Other, 0, 0);
    tree.entry(&d, b"pipe", Kind::Other, 0, 0);
    tree.entry(ROOT, b"mnt", Kind::Mount, 0, 0);
    let walked = walk(&tree, ROOT, later()).unwrap();
    assert_eq!(
        walked.left_out,
        LeftOut {
            links: 2,
            mounts: 1,
            others: 2,
        }
    );
    assert_eq!(walked.files.len(), 2);
    assert_eq!(tree.asked(), [format!("list {ROOT}"), format!("list {d}")]);
}

/// The workspace's inbox is the owner's: given from the workspace, it is
/// left out by its name whatever it is said to be, and counted as a
/// mount. A folder named inbox anywhere else is walked.
#[test]
fn the_workspaces_inbox_is_never_walked() {
    for kind in [Kind::Mount, Kind::Folder, Kind::File] {
        let mut tree = Tree::of(WORKSPACE, &[("a.txt", b"a"), ("d/inbox/b.txt", b"b")]);
        tree.entry(WORKSPACE, b"inbox", kind, 99, 1);
        let walked = walk(&tree, WORKSPACE, later()).unwrap();
        assert_eq!(walked.left_out.mounts, 1, "{kind:?}");
        assert_eq!(
            walked.files,
            [
                (String::from("a.txt"), 1),
                (String::from("d/inbox/b.txt"), 1)
            ],
            "{kind:?}"
        );
        assert!(
            !tree
                .asked()
                .contains(&String::from("list /workspace/inbox")),
            "{kind:?}"
        );
    }
    // Not at the workspace, a file named inbox is a file.
    let tree = Tree::of(ROOT, &[("inbox", b"x")]);
    assert_eq!(walk(&tree, ROOT, later()).unwrap().files.len(), 1);
}

/// A name that isn't UTF-8, or breaks one of the Library's rules, at any
/// depth, a file's or a folder's, refuses the folder in words that never
/// name it.
#[test]
fn a_name_the_owners_computer_cant_hold_refuses_the_folder() {
    let mut tree = Tree::of(ROOT, &[("a.txt", b"a")]);
    tree.entry(ROOT, b"b\xff.txt", Kind::File, 50, 1);
    assert_eq!(walk(&tree, ROOT, later()), Err(String::from(NOT_PLAIN)));
    for path in [
        "CON.txt",
        "d/aux",
        "d/e/desktop.ini",
        "d/x.lnk",
        "a./b.txt",
        " a.txt",
        "a<b.txt",
        "caf\u{e9}.txt",
    ] {
        let tree = Tree::of(ROOT, &[("ok.txt", b"a"), (path, b"b")]);
        let refused = walk(&tree, ROOT, later()).unwrap_err();
        assert_eq!(
            Err(refused.as_str()),
            check_paths([path]),
            "{path}: {refused}"
        );
        assert!(!refused.contains(path), "{path}");
    }
}

/// Two names in one folder that are one but for case refuse it, files or
/// folders: whatever folders they are in.
#[test]
fn names_that_are_one_but_for_case_refuse_the_folder() {
    let cases: [[(&str, &[u8]); 2]; 3] = [
        [("A.txt", b"a"), ("a.txt", b"b")],
        [("Src/a.txt", b"a"), ("src/b.txt", b"b")],
        [("d/A", b"a"), ("d/a/b.txt", b"b")],
    ];
    for files in cases {
        let tree = Tree::of(ROOT, &files);
        assert_eq!(
            walk(&tree, ROOT, later()),
            Err(String::from(CASE_TWINS)),
            "{files:?}"
        );
    }
    // The same name in two folders is two files.
    let tree = Tree::of(ROOT, &[("a/x.txt", b"a"), ("b/x.txt", b"b")]);
    assert!(walk(&tree, ROOT, later()).is_ok());
}

/// A folder is held to the inode its parent listed it with.
#[test]
fn a_folder_swapped_for_another_while_walked_refuses_it() {
    let mut tree = Tree::of(ROOT, &[("d/a.txt", b"a")]);
    let Some(Ok(listing)) = tree.lists.get_mut(&format!("{ROOT}/d")) else {
        unreachable!()
    };
    listing.ino += 1000;
    assert_eq!(walk(&tree, ROOT, later()), Err(String::from(CHANGED)));
}

/// A folder with no file in it, at any depth, gives nothing; a listing
/// refused says why.
#[test]
fn an_empty_folder_or_one_that_cant_be_listed_gives_nothing() {
    let mut tree = Tree::of(ROOT, &[]);
    tree.folder(&format!("{ROOT}/a/b"));
    tree.entry(ROOT, b"link", Kind::Link, 0, 0);
    assert_eq!(walk(&tree, ROOT, later()), Err(String::from(EMPTY)));
    let tree = Tree::default();
    assert_eq!(
        walk(&tree, ROOT, later()),
        Err(String::from("no such folder"))
    );
}

/// The deadline is checked before each listing.
#[test]
fn a_walk_ends_at_its_deadline() {
    let tree = Tree::of(ROOT, &[("d/a.txt", b"a")]);
    assert_eq!(
        walk(&tree, ROOT, Instant::now()),
        Err(String::from(TOO_SLOW))
    );
    assert!(tree.asked().is_empty());
    let mut tree = Tree::of(ROOT, &[("d/a.txt", b"a")]);
    tree.slow = Duration::from_millis(50);
    let until = Instant::now() + Duration::from_millis(20);
    assert_eq!(walk(&tree, ROOT, until), Err(String::from(TOO_SLOW)));
    assert_eq!(tree.asked(), [format!("list {ROOT}")]);
}

/// A tree of `folders` folders under the root, each listed, and `files`
/// files of `size` in the root.
fn wide(folders: usize, files: usize, size: u64) -> Tree {
    let mut tree = Tree::of(ROOT, &[]);
    for n in 0..folders {
        tree.folder(&format!("{ROOT}/d{n:03}"));
    }
    for n in 0..files {
        tree.entry(ROOT, format!("f{n:03}").as_bytes(), Kind::File, 9, size);
    }
    tree
}

/// Folders and files are counted as they are found, each bound held at
/// its edge: past one, nothing more is listed.
#[test]
fn the_walk_is_held_to_its_bounds_as_it_finds_things() {
    // 256 folders, the root among them, are listed; one more isn't.
    let tree = wide(FOLDERS - 1, 1, 1);
    assert!(walk(&tree, ROOT, later()).is_ok());
    assert_eq!(tree.asked().len(), FOLDERS);
    let tree = wide(FOLDERS, 1, 1);
    assert_eq!(
        walk(&tree, ROOT, later()),
        Err(String::from(TOO_MANY_FOLDERS))
    );
    assert_eq!(tree.asked(), [format!("list {ROOT}")]);
    // 512 files; one more refuses as it is found, before a folder found
    // before it is listed; so do bytes past a folder's.
    let most = usize::try_from(FOLDER_FILES).unwrap();
    let tree = wide(1, most + 1, 1);
    assert_eq!(
        walk(&tree, ROOT, later()),
        Err(String::from(TOO_MANY_FILES))
    );
    assert_eq!(tree.asked(), [format!("list {ROOT}")]);
    let tree = wide(1, 5, FILE_BYTES);
    assert_eq!(walk(&tree, ROOT, later()), Err(String::from(TOO_LARGE)));
    assert_eq!(tree.asked(), [format!("list {ROOT}")]);
    assert_eq!(
        walk(&wide(0, most, 1), ROOT, later()).unwrap().files.len(),
        most
    );
    assert_eq!(
        walk(&wide(0, most + 1, 1), ROOT, later()),
        Err(String::from(TOO_MANY_FILES))
    );
    // A file of at most 8 MiB, and 32 MiB in all.
    assert!(walk(&wide(0, 1, FILE_BYTES), ROOT, later()).is_ok());
    assert_eq!(
        walk(&wide(0, 1, FILE_BYTES + 1), ROOT, later()),
        Err(String::from(FILE_TOO_LARGE))
    );
    let mut tree = wide(0, 4, FILE_BYTES);
    assert_eq!(walk(&tree, ROOT, later()).unwrap().bytes, FOLDER_BYTES);
    tree.entry(ROOT, b"z", Kind::File, 9, 1);
    assert_eq!(walk(&tree, ROOT, later()), Err(String::from(TOO_LARGE)));
}

/// A path is held to the Library's depth, and with the folder's own path
/// before it, to what the Room can be asked for.
#[test]
fn a_path_too_deep_or_too_long_refuses_the_folder() {
    let deepest = vec!["d"; crate::library::FOLDER_DEPTH].join("/");
    let tree = Tree::of(ROOT, &[(deepest.as_str(), b"a")]);
    assert!(walk(&tree, ROOT, later()).is_ok());
    let deeper = format!("{deepest}/e");
    let tree = Tree::of(ROOT, &[(deeper.as_str(), b"a")]);
    assert_eq!(
        walk(&tree, ROOT, later()),
        Err(String::from(check_paths([deeper.as_str()]).unwrap_err()))
    );
    // A folder 814 bytes deep: a path in it of 209 bytes is 1024 whole,
    // and of 210 one more.
    let step = "s".repeat(200);
    let base = format!("{WORKSPACE}/{step}/{step}/{step}/{step}");
    assert_eq!(base.len(), 814);
    for (more, refused) in [(8, false), (9, true)] {
        let path = format!("{}/{}", "n".repeat(200), "m".repeat(more));
        assert!(check_paths([path.as_str()]).is_ok(), "{path}");
        let tree = Tree::of(&base, &[(path.as_str(), b"a")]);
        assert_eq!(
            walk(&tree, &base, later()) == Err(String::from(PATH_TOO_LONG)),
            refused,
            "{}",
            whole(&base, &path).len()
        );
    }
}
