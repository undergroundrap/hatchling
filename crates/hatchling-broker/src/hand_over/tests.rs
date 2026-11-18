//! Handing a file over, with a stand-in Room, stand-in checks and a real
//! Library, Activity log and app stream in a scratch directory.

use std::cell::RefCell;
use std::sync::Arc;

use hatchling_core::room::{End, Found, JobId, Page, Stream};
use hatchling_core::ui::UiEvent;
use hatchling_vault::MasterKey;

use super::*;
use crate::api_stream::Bus;
use crate::room_job::{RoomJobAnswer, RoomRun};
use crate::spawner::{RoomChange, RoomChanged, RoomSearch};

/// The chat's word on a run: whether it is the chat's, in the open
/// conversation.
type Open = fn(&str) -> Option<bool>;

/// A Room that hands back one file, or why not, and says what it was
/// asked for.
struct Room {
    file: Result<Vec<u8>, String>,
    asked: RefCell<Vec<String>>,
}

impl Room {
    const fn with(file: Result<Vec<u8>, String>) -> Self {
        Self {
            file,
            asked: RefCell::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }
}

impl Rooms for Room {
    fn run(&self, _: &RoomRun) -> Result<RoomJobAnswer, String> {
        unreachable!("a file out runs nothing")
    }
    fn job(&self, _: &JobId, _: u32) -> Result<RoomJobAnswer, String> {
        unreachable!("a file out runs nothing")
    }
    fn stop(&self, _: &JobId) -> Result<RoomJobAnswer, String> {
        unreachable!("a file out runs nothing")
    }
    fn output(&self, _: &JobId, _: Stream, _: End) -> Result<Vec<u8>, String> {
        unreachable!("a file out runs nothing")
    }
    fn page(&self, _: &str, _: u64, _: u32) -> Result<Page, String> {
        unreachable!("a file out reads no page")
    }
    fn search(&self, _: &RoomSearch) -> Result<Found, String> {
        unreachable!("a file out searches nothing")
    }
    fn change(&self, _: &RoomChange) -> Result<RoomChanged, String> {
        unreachable!("a file out changes nothing")
    }
    fn take(&self, path: &str) -> Result<Vec<u8>, String> {
        self.asked.borrow_mut().push(path.to_owned());
        self.file.clone()
    }
}

/// Checks that answer for the name and for anything else as told.
struct Says {
    name: Vec<u8>,
    for_name: Result<bool, String>,
    for_file: Result<bool, String>,
}

impl Checks for Says {
    fn holds(&self, text: &[u8]) -> Result<bool, String> {
        if text == self.name.as_slice() {
            self.for_name.clone()
        } else {
            self.for_file.clone()
        }
    }
}

/// A scratch install: a Library, schedules for the Activity log, and a
/// vault of its own, so its app stream is its own.
struct Install {
    config: Config,
    key: MasterKey,
    bus: Arc<Bus>,
    cell: CellId,
    _scratch: crate::api_auth::tests::Scratch,
}

fn install(name: &str) -> Install {
    let scratch = crate::api_auth::tests::scratch(&format!("hand-over-{name}"));
    let mut config = crate::api_auth::tests::owner_config(None);
    config.scheduler.root = scratch.0.join("schedules");
    config.scheduler.library = scratch.0.join("library");
    config.vault.root = scratch.0.join("vault");
    let bus = Arc::new(Bus::starting_at(1));
    crate::app_stream::stream(&config).attach(Arc::clone(&bus));
    Install {
        config,
        key: MasterKey::generate().unwrap(),
        bus,
        cell: "run-0123456789abcdef01234567".parse().unwrap(),
        _scratch: scratch,
    }
}

impl Install {
    /// A way to hand a file over for the run of `cell`, a chat's run in
    /// its open conversation as `open` says, its file and its name
    /// checked as `checks` says.
    fn handing<'k>(
        &'k self,
        cell: Option<&'k CellId>,
        open: fn(&str) -> Option<bool>,
        checks: Says,
    ) -> Handing<'k> {
        Handing {
            config: &self.config,
            keys: &self.key,
            cell,
            run_id: String::from("run-1"),
            open,
            checks: Box::new(checks),
            recorded: Cell::new(0),
        }
    }

    fn library(&self) -> Library<'_> {
        Library::of(&self.config, &self.key).unwrap()
    }

    fn events(&self) -> Vec<UiEventBody> {
        self.bus
            .after(None)
            .into_iter()
            .map(|event: UiEvent| event.body)
            .collect()
    }

    /// What Activity kept of each file out, in order.
    fn crossings(&self) -> Vec<(Option<String>, u64, FileOutEnded)> {
        std::fs::read_to_string(self.config.scheduler.activity_log())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<ActivityEvent>(line).ok())
            .filter_map(|event| {
                if let SubagentEvent::FileOut { name, bytes, ended } = event.kind {
                    Some((name, bytes, ended))
                } else {
                    None
                }
            })
            .collect()
    }
}

fn clear(name: &str) -> Says {
    Says {
        name: name.as_bytes().to_vec(),
        for_name: Ok(false),
        for_file: Ok(false),
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "it stands in for the chat's word, which may be none"
)]
const fn chat(_: &str) -> Option<bool> {
    Some(true)
}

#[test]
fn a_file_is_kept_offered_and_kept_in_activity() {
    let install = install("offered");
    let room = Room::with(Ok(b"the report".to_vec()));
    let handing = install.handing(Some(&install.cell), chat, clear("report.pdf"));
    let handed = handing.hand(&room, "out/report.pdf");
    assert_eq!(
        handed,
        RoomResult::HandedOver {
            name: String::from("report.pdf"),
            bytes: 10
        }
    );
    // The Room was asked for the file under the workspace, once.
    assert_eq!(room.asked(), ["/workspace/out/report.pdf"]);
    let files = install.library().files().unwrap();
    let [file] = files.as_slice() else {
        unreachable!("{files:?}");
    };
    assert_eq!(
        install.library().fetch(&file.id).unwrap(),
        Some(b"the report".to_vec())
    );
    // Grouped by the broker: the open conversation, and the chat run's
    // cell, never what the Room said.
    assert_eq!(
        (file.conversation, file.reply.as_str()),
        (
            install.library().conversation().unwrap(),
            install.cell.as_str()
        )
    );
    assert_eq!(install.library().standing().unwrap(), (1, false));
    let events = install.events();
    let [UiEventBody::FileOffered(offered)] = events.as_slice() else {
        unreachable!("{events:?}");
    };
    assert_eq!(offered.chat_id, LOCAL_CHAT);
    assert!(
        offered.message_id.starts_with("file-"),
        "{}",
        offered.message_id
    );
    assert_eq!(
        offered.file,
        LibraryFile {
            id: file.id.clone(),
            name: String::from("report.pdf"),
            bytes: 10,
            sha256: file.sha256.clone(),
            kept_at: file.kept_at.clone(),
        }
    );
    assert_eq!(
        install.crossings(),
        [(Some(String::from("report.pdf")), 10, FileOutEnded::Offered)]
    );
    // An absolute path is asked for as it is.
    handing.hand(&room, "/workspace/b.txt");
    assert_eq!(room.asked().last().unwrap(), "/workspace/b.txt");
}

#[test]
fn a_file_holding_a_secret_is_withheld_and_turns_handing_over_off() {
    let install = install("withheld");
    let room = Room::with(Ok(b"has the key in it".to_vec()));
    let mut says = clear("notes.txt");
    says.for_file = Ok(true);
    let handing = install.handing(Some(&install.cell), chat, says);
    assert_eq!(
        handing.hand(&room, "notes.txt"),
        RoomResult::NotHandedOver {
            reason: String::from(WITHHELD)
        }
    );
    assert!(
        install.library().files().unwrap().is_empty(),
        "a withheld file was kept"
    );
    // Checked, it was spent; found holding a secret, giving is off.
    assert_eq!(install.library().standing().unwrap(), (1, true));
    let events = install.events();
    let [UiEventBody::FileWithheld(withheld)] = events.as_slice() else {
        unreachable!("{events:?}");
    };
    assert_eq!(withheld.name.as_deref(), Some("notes.txt"));
    assert_eq!(withheld.chat_id, LOCAL_CHAT);
    // The next is refused before the Room is asked for anything.
    let next = install.handing(Some(&install.cell), chat, clear("other.txt"));
    assert_eq!(
        next.hand(&room, "other.txt"),
        RoomResult::NotHandedOver {
            reason: String::from(Refused::Off.words())
        }
    );
    assert_eq!(room.asked().len(), 1, "the Room was asked again");
    assert_eq!(
        install.crossings(),
        [(Some(String::from("notes.txt")), 17, FileOutEnded::Withheld)]
    );
}

#[test]
fn a_name_holding_a_secret_is_never_shown_or_kept() {
    let install = install("withheld-name");
    let room = Room::with(Ok(b"plain".to_vec()));
    let mut says = clear("sk-held.txt");
    says.for_name = Ok(true);
    let handing = install.handing(Some(&install.cell), chat, says);
    assert_eq!(
        handing.hand(&room, "sk-held.txt"),
        RoomResult::NotHandedOver {
            reason: String::from(WITHHELD)
        }
    );
    let events = install.events();
    let [UiEventBody::FileWithheld(withheld)] = events.as_slice() else {
        unreachable!("{events:?}");
    };
    assert_eq!(withheld.name, None);
    assert_eq!(install.crossings(), [(None, 5, FileOutEnded::Withheld)]);
    assert_eq!(install.library().standing().unwrap(), (1, true));
}

#[test]
fn a_file_that_couldnt_be_checked_is_never_told_as_a_secret() {
    for (for_name, for_file) in [
        (Err(String::from("no keeper")), Ok(false)),
        (Ok(false), Err(String::from("no keeper"))),
    ] {
        let install = install("unchecked");
        let room = Room::with(Ok(b"plain".to_vec()));
        let says = Says {
            name: b"a.txt".to_vec(),
            for_name,
            for_file,
        };
        let handing = install.handing(Some(&install.cell), chat, says);
        assert_eq!(
            handing.hand(&room, "a.txt"),
            RoomResult::NotHandedOver {
                reason: String::from(UNCHECKED)
            }
        );
        assert!(install.library().files().unwrap().is_empty());
        // Checked, or tried, it is spent; nothing is turned off.
        assert_eq!(
            install.library().standing().unwrap(),
            (1, false),
            "turned off, or not spent"
        );
        assert!(install.events().is_empty(), "the owner was told of it");
        assert_eq!(install.crossings(), [(None, 5, FileOutEnded::Unchecked)]);
    }
}

#[test]
fn either_check_finding_a_secret_is_a_secret_found() {
    let no = || Err(String::from("no"));
    assert_eq!(either(Ok(true), no()), Ok(true));
    assert_eq!(either(no(), Ok(true)), Ok(true));
    assert_eq!(either(Ok(true), Ok(false)), Ok(true));
    assert_eq!(either(Ok(false), Ok(true)), Ok(true));
    assert_eq!(either(Ok(false), Ok(false)), Ok(false));
    assert_eq!(either(no(), Ok(false)), no());
    assert_eq!(either(Ok(false), no()), no());
}

/// Whatever is refused before the file is read leaves the Room unasked,
/// keeps nothing, and writes no line to Activity: a session asking over
/// and over fills no log.
#[test]
fn what_is_refused_before_the_file_is_read_asks_the_room_nothing() {
    let not_a_chat: Open = |_| None;
    let stale: Open = |_| Some(false);
    let cases: [(&str, bool, Open, bool, &str, &str); 7] = [
        ("off", false, chat, true, "a.txt", OFF_HERE),
        ("no-cell", true, chat, false, "a.txt", NOT_THE_CHAT),
        ("not-a-chat", true, not_a_chat, true, "a.txt", NOT_THE_CHAT),
        ("stale", true, stale, true, "a.txt", STALE),
        ("reserved", true, chat, true, "out/con.txt", RESERVED),
        ("trailing", true, chat, true, "notes.", TRAILING_DOT),
        ("spent", true, chat, true, "a.txt", Refused::Spent.words()),
    ];
    for (case, library, open, cell, path, said) in cases {
        let mut install = install(&format!("before-{case}"));
        if case == "spent" {
            for file in 0..crate::library::HANDED_PER_CONVERSATION {
                install
                    .library()
                    .keep(&format!("f{file}"), b"x", "now")
                    .unwrap()
                    .unwrap();
            }
        }
        if !library {
            install.config.scheduler.library = std::path::PathBuf::new();
        }
        let room = Room::with(Ok(b"x".to_vec()));
        let cell = cell.then_some(&install.cell);
        let handing = install.handing(cell, open, clear("a.txt"));
        assert_eq!(
            handing.hand(&room, path),
            RoomResult::NotHandedOver {
                reason: String::from(said)
            },
            "{case}"
        );
        assert!(room.asked().is_empty(), "{case}: the Room was asked");
        assert!(install.crossings().is_empty(), "{case}");
        assert!(install.events().is_empty(), "{case}");
    }
}

/// The Room's refusal is said as the Room said it; a file the Library
/// won't keep is said in the Library's words before the file is checked,
/// so by no name: nothing was spent.
#[test]
fn a_file_the_room_or_the_library_refuses_is_said_so() {
    let install = install("refused");
    let room = Room::with(Err(String::from("that path has a link in it")));
    let handing = install.handing(Some(&install.cell), chat, clear("a.txt"));
    assert_eq!(
        handing.hand(&room, "a.txt"),
        RoomResult::NotHandedOver {
            reason: String::from("that path has a link in it")
        }
    );
    let longest = usize::try_from(crate::library::FILE_BYTES).unwrap();
    let over = Room::with(Ok(vec![0; longest + 1]));
    assert_eq!(
        handing.hand(&over, "a.txt"),
        RoomResult::NotHandedOver {
            reason: String::from(Refused::TooLarge.words())
        }
    );
    let size = u64::try_from(longest + 1).unwrap();
    assert_eq!(
        install.crossings(),
        [
            (None, 0, FileOutEnded::Refused),
            (None, size, FileOutEnded::Refused),
        ]
    );
    assert!(install.library().files().unwrap().is_empty());
    assert_eq!(install.library().standing().unwrap(), (0, false));
}

#[test]
fn a_name_is_the_files_own_made_plain_and_never_a_device() {
    let longest = format!("{}.txt", "n".repeat(300));
    for (path, name) in [
        ("report.pdf", "report.pdf"),
        ("/workspace/out/report.pdf", "report.pdf"),
        ("../../etc/passwd", "passwd"),
        ("a b\u{202e}c.txt", "a_b_c.txt"),
        (".hidden", "hidden"),
        ("", "file"),
        ("COM10", "COM10"),
        ("console.log", "console.log"),
        ("com", "com"),
        ("lpt", "lpt"),
        ("nul_file", "nul_file"),
    ] {
        assert_eq!(name_of(path), Ok(String::from(name)), "{path:?}");
    }
    let cut = name_of(&longest).unwrap();
    assert!(
        cut.len() <= crate::library::NAME_BYTES && cut.rsplit('.').next() == Some("txt"),
        "{cut}"
    );
    for path in [
        "CON",
        "con",
        "con.txt",
        "Prn.doc",
        "aux",
        "nul.tar.gz",
        "COM0",
        "COM1",
        "com9.log",
        "LPT0",
        "LPT1",
        "lpt9.x",
    ] {
        assert_eq!(name_of(path), Err(RESERVED), "{path:?}");
    }
    assert_eq!(name_of("notes."), Err(TRAILING_DOT));
    // A file Windows acts on when its folder is only shown: never given.
    for path in [
        "desktop.ini",
        "out/Desktop.INI",
        "share.url",
        "a.SCF",
        "x.lnk",
        "y.library-ms",
        "z.search-ms",
        "q.searchConnector-ms",
        "t.theme",
        "t.themepack",
        "a.website",
        "a.PIF",
        "a.deskthemepack",
    ] {
        assert_eq!(name_of(path), Err(REACHES_OUT), "{path:?}");
    }
    for path in [
        "desktop.ini.txt",
        "url",
        "notes.url.txt",
        "lnk.pdf",
        "my-desktop.ini",
    ] {
        assert!(name_of(path).is_ok(), "{path:?}");
    }
    assert_eq!(name_of("notes. "), Ok(String::from("notes._")));
    for path in ["report.pdf", "a b c", "", "x\u{0}y"] {
        assert!(crate::library::is_name(&name_of(path).unwrap()), "{path:?}");
    }
}

/// The broker's own checks: a file, or a name, holding a secret the
/// vault keeps is found, and one holding none isn't.
#[test]
fn the_brokers_checks_find_what_the_vault_keeps() {
    let mut install = install("checks");
    // The egress grant names the secret, as a gate scans for it.
    install.config.egress.grants = vec![String::from("api.example.com:443")];
    hatchling_vault::SealedVault::open(&install.config.vault.root, &install.key)
        .unwrap()
        .store(
            &"egress-api-example-com-443".parse().unwrap(),
            &hatchling_vault::Secret::new(b"held-credential-value".to_vec()).unwrap(),
        )
        .unwrap();
    let held = Held {
        config: &install.config,
        keys: &install.key,
        cell: Some(&install.cell),
    };
    assert_eq!(held.holds(b"a file with nothing held in it"), Ok(false));
    assert_eq!(
        held.holds(b"a file with held-credential-value in it"),
        Ok(true)
    );
    // Each check finds it by itself: the gate's scan, and the keeper's,
    // which is the broker's own where no keeper runs.
    for check in [Held::scanned, Held::kept] {
        assert_eq!(check(&held, b"nothing held"), Ok(false));
        assert_eq!(check(&held, b"has held-credential-value"), Ok(true));
    }
    // A secret kept that no setting names is the keeper's check's to find:
    // it reads every one the vault keeps.
    hatchling_vault::SealedVault::open(&install.config.vault.root, &install.key)
        .unwrap()
        .store(
            &"named-by-nothing".parse().unwrap(),
            &hatchling_vault::Secret::new(b"unnamed-credential-value".to_vec()).unwrap(),
        )
        .unwrap();
    let unnamed = b"has unnamed-credential-value";
    assert_eq!(held.scanned(unnamed), Ok(false));
    assert_eq!(held.holds(unnamed), Ok(true));
    // A run the chat didn't start hands nothing over.
    let handing = Handing::new(&install.config, &install.key, true, &install.cell, "run-1");
    assert_eq!(
        handing.hand(&Room::with(Ok(b"x".to_vec())), "a.txt"),
        RoomResult::NotHandedOver {
            reason: String::from(NOT_THE_CHAT)
        }
    );
}

/// A session the broker didn't start as a run hands nothing over, though
/// the cell its hello names is the chat's run, in the open conversation:
/// it names any cell it likes. The broker's own run of that cell asks the
/// Room.
#[test]
fn only_the_brokers_own_run_of_a_cell_hands_a_file_over() {
    let install = install("known");
    let cell: CellId = "cell-handing-known".parse().unwrap();
    let hub = hatchling_channel::LocalHub::new(hatchling_channel::OutputGate::disabled());
    crate::follow_ups::began(cell.as_str(), &hub, hub.conversation());
    for known in [false, true] {
        let room = Room::with(Ok(b"x".to_vec()));
        let handing = Handing::new(&install.config, &install.key, known, &cell, "run-1");
        let handed = handing.hand(&room, "a.txt");
        assert_eq!(
            room.asked().len(),
            usize::from(known),
            "{known}: {handed:?}"
        );
        if !known {
            assert_eq!(
                handed,
                RoomResult::NotHandedOver {
                    reason: String::from(NOT_THE_CHAT)
                }
            );
        }
    }
    let _ = crate::follow_ups::ended(cell.as_str());
}

/// A name whose check failed is never shown, though the file is found to
/// hold a secret: only a name checked and found clear is.
#[test]
fn a_name_whose_check_failed_is_never_shown() {
    let install = install("unchecked-name");
    let room = Room::with(Ok(b"has the key in it".to_vec()));
    let says = Says {
        name: b"maybe-held.txt".to_vec(),
        for_name: Err(String::from("no keeper")),
        for_file: Ok(true),
    };
    let handing = install.handing(Some(&install.cell), chat, says);
    assert_eq!(
        handing.hand(&room, "maybe-held.txt"),
        RoomResult::NotHandedOver {
            reason: String::from(WITHHELD)
        }
    );
    let events = install.events();
    let [UiEventBody::FileWithheld(withheld)] = events.as_slice() else {
        unreachable!("{events:?}");
    };
    assert_eq!(withheld.name, None);
    assert_eq!(install.crossings(), [(None, 17, FileOutEnded::Withheld)]);
}

thread_local! {
    /// How many times the chat has been asked whether a run is in its
    /// open conversation.
    static ASKED: Cell<u32> = const { Cell::new(0) };
}

/// Open when first asked, and stale after: the owner started a new
/// conversation while the file was read.
#[allow(
    clippy::unnecessary_wraps,
    reason = "it stands in for the chat's word, which may be none"
)]
fn then_stale(_: &str) -> Option<bool> {
    let asked = ASKED.with(|asked| {
        asked.set(asked.get() + 1);
        asked.get()
    });
    Some(asked == 1)
}

/// A file read for a conversation that ended while it was read is never
/// kept or told, and a secret in it never turns the new conversation off.
#[test]
fn a_file_read_for_a_conversation_since_ended_is_never_kept() {
    for (case, for_file) in [("clear", false), ("held", true)] {
        ASKED.with(|asked| asked.set(0));
        let install = install(&format!("stale-{case}"));
        let room = Room::with(Ok(b"the report".to_vec()));
        let mut says = clear("report.pdf");
        says.for_file = Ok(for_file);
        let handing = install.handing(Some(&install.cell), then_stale, says);
        assert_eq!(
            handing.hand(&room, "report.pdf"),
            RoomResult::NotHandedOver {
                reason: String::from(STALE)
            },
            "{case}"
        );
        assert_eq!(room.asked().len(), 1, "{case}");
        assert_eq!(ASKED.with(Cell::get), 2, "{case}");
        assert!(install.library().files().unwrap().is_empty(), "{case}");
        // It was checked, so it was spent, in the conversation it began in.
        assert_eq!(install.library().standing().unwrap(), (1, false), "{case}");
        assert!(install.events().is_empty(), "{case}");
        assert_eq!(
            install.crossings(),
            [(Some(String::from("report.pdf")), 10, FileOutEnded::Refused)],
            "{case}"
        );
    }
}

/// One session writes at most [`RECORDED_MAX`] lines to Activity, however
/// often its Room refuses.
#[test]
fn a_session_writes_only_so_many_lines_to_activity() {
    let install = install("recorded");
    let room = Room::with(Err(String::from("no such file")));
    let handing = install.handing(Some(&install.cell), chat, clear("a.txt"));
    let lines = usize::try_from(RECORDED_MAX).unwrap();
    for _ in 0..=lines {
        handing.hand(&room, "a.txt");
    }
    assert_eq!(room.asked().len(), lines + 1);
    assert_eq!(install.crossings().len(), lines);
    // Another session writes its own.
    let other = install.handing(Some(&install.cell), chat, clear("a.txt"));
    other.hand(&room, "a.txt");
    assert_eq!(install.crossings().len(), lines + 1);
}

/// Checks that answer as told, and count how often they were asked.
struct Counted {
    says: Says,
    asked: std::rc::Rc<Cell<u32>>,
}

impl Checks for Counted {
    fn holds(&self, text: &[u8]) -> Result<bool, String> {
        self.asked.set(self.asked.get() + 1);
        self.says.holds(text)
    }
}

/// A file the Library couldn't keep is never checked: the check is a guess
/// at a secret, and a guess is only made when one is spent (#504).
#[test]
fn a_file_the_library_couldnt_keep_is_never_checked() {
    let install = install("never-checked");
    let library = install.library();
    let mut held = vec![crate::library::FILE_BYTES; 31];
    held.push(crate::library::FILE_BYTES - 5);
    library.pretend_held(&held);
    let left = crate::library::LIBRARY_BYTES - held.iter().sum::<u64>();
    let asked = std::rc::Rc::new(Cell::new(0));
    let mut handing = install.handing(Some(&install.cell), chat, clear("a.txt"));
    handing.checks = Box::new(Counted {
        says: clear("a.txt"),
        asked: std::rc::Rc::clone(&asked),
    });
    let room = Room::with(Ok(vec![b'x'; usize::try_from(left).unwrap() + 1]));
    for _ in 0..20 {
        assert_eq!(
            handing.hand(&room, "a.txt"),
            RoomResult::NotHandedOver {
                reason: String::from(Refused::Full.words())
            }
        );
    }
    assert_eq!(
        asked.get(),
        0,
        "a file the Library couldn't keep was checked"
    );
    assert_eq!(library.standing().unwrap(), (0, false));
}

/// Every file checked spends one of the conversation's hand-overs, kept or
/// not: one that couldn't be checked too. Past the last, nothing is read.
#[test]
fn every_file_checked_spends_a_hand_over() {
    let install = install("every-checked");
    let room = Room::with(Ok(b"x".to_vec()));
    let unchecked = || Says {
        name: b"a.txt".to_vec(),
        for_name: Ok(false),
        for_file: Err(String::from("no keeper")),
    };
    let handing = install.handing(Some(&install.cell), chat, unchecked());
    for _ in 0..crate::library::HANDED_PER_CONVERSATION {
        assert_eq!(
            handing.hand(&room, "a.txt"),
            RoomResult::NotHandedOver {
                reason: String::from(UNCHECKED)
            }
        );
    }
    assert_eq!(
        handing.hand(&room, "a.txt"),
        RoomResult::NotHandedOver {
            reason: String::from(Refused::Spent.words())
        }
    );
    assert_eq!(
        room.asked().len(),
        usize::try_from(crate::library::HANDED_PER_CONVERSATION).unwrap(),
        "a file was read past the last hand-over"
    );
}

/// A Room whose file is read out while the owner starts a new conversation.
struct Bumping<'a> {
    room: Room,
    library: Library<'a>,
}

impl Rooms for Bumping<'_> {
    fn run(&self, run: &RoomRun) -> Result<RoomJobAnswer, String> {
        self.room.run(run)
    }
    fn job(&self, job: &JobId, wait: u32) -> Result<RoomJobAnswer, String> {
        self.room.job(job, wait)
    }
    fn stop(&self, job: &JobId) -> Result<RoomJobAnswer, String> {
        self.room.stop(job)
    }
    fn output(&self, job: &JobId, stream: Stream, end: End) -> Result<Vec<u8>, String> {
        self.room.output(job, stream, end)
    }
    fn page(&self, path: &str, from: u64, lines: u32) -> Result<Page, String> {
        self.room.page(path, from, lines)
    }
    fn search(&self, search: &RoomSearch) -> Result<Found, String> {
        self.room.search(search)
    }
    fn change(&self, change: &RoomChange) -> Result<RoomChanged, String> {
        self.room.change(change)
    }
    fn take(&self, path: &str) -> Result<Vec<u8>, String> {
        self.library.new_conversation().unwrap();
        self.room.take(path)
    }
}

/// A file read while the owner started a new conversation is neither
/// checked, counted nor kept in it: the hand-over belongs to the one it
/// began in.
#[test]
fn a_file_read_as_a_new_conversation_starts_is_never_checked_or_counted() {
    let install = install("bump-read");
    let asked = std::rc::Rc::new(Cell::new(0));
    let mut handing = install.handing(Some(&install.cell), chat, clear("a.txt"));
    handing.checks = Box::new(Counted {
        says: clear("a.txt"),
        asked: std::rc::Rc::clone(&asked),
    });
    let room = Bumping {
        room: Room::with(Ok(b"x".to_vec())),
        library: install.library(),
    };
    assert_eq!(
        handing.hand(&room, "a.txt"),
        RoomResult::NotHandedOver {
            reason: String::from(STALE)
        }
    );
    assert_eq!(asked.get(), 0, "a stale file was checked");
    assert_eq!(install.library().standing().unwrap(), (0, false));
    assert!(install.library().files().unwrap().is_empty());
}

/// Checks during which the owner starts a new conversation.
struct BumpingChecks<'a> {
    says: Says,
    library: Library<'a>,
}

impl Checks for BumpingChecks<'_> {
    fn holds(&self, text: &[u8]) -> Result<bool, String> {
        if text == self.says.name.as_slice() {
            self.library.new_conversation().unwrap();
        }
        self.says.holds(text)
    }
}

/// A file checked while the owner started a new conversation is neither
/// kept in it nor, holding a secret, turns it off: what it spent stays in
/// the conversation it began in.
#[test]
fn a_file_checked_as_a_new_conversation_starts_is_neither_kept_nor_turns_it_off() {
    for held in [false, true] {
        let install = install(&format!("bump-check-{held}"));
        let mut says = clear("a.txt");
        says.for_file = Ok(held);
        let mut handing = install.handing(Some(&install.cell), chat, clear("a.txt"));
        handing.checks = Box::new(BumpingChecks {
            says,
            library: install.library(),
        });
        let room = Room::with(Ok(b"x".to_vec()));
        let said = handing.hand(&room, "a.txt");
        if held {
            assert_eq!(
                said,
                RoomResult::NotHandedOver {
                    reason: String::from(WITHHELD)
                }
            );
        } else {
            assert_eq!(
                said,
                RoomResult::NotHandedOver {
                    reason: String::from(STALE)
                }
            );
        }
        assert_eq!(install.library().standing().unwrap(), (0, false), "{held}");
        assert!(install.library().files().unwrap().is_empty(), "{held}");
    }
}
