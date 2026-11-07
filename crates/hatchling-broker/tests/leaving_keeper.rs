//! What the agent sends off the machine, against the real vault keeper: the
//! broker's binary as `vault-keeper`, on a socket pair, its secrets sealed
//! by `migrate-vault`, as on an install.
//!
//! A stand-in is how these gates once passed while never seeing a real key:
//! with a keeper, they looked only for what a cell had already had, and no
//! cell can have a vault secret. So here nothing stands in for the keeper,
//! and [`Install::new`] proves no cell has had any of these keys. The
//! fetcher, the searcher and the browser do stand in, and each says whether
//! anything got past the gate.
//!
//! A hit ends the agent's run ([`HeldHit`]), so that's proved here too: each
//! way out in turn, the session on a socket pair, and a chat run in a real
//! cell whose program goes on as if nothing happened.
//!
//! The keeper a broker asks is one for the whole process, so this is a test
//! binary of its own, with one install that its tests take in turn.
#![allow(
    clippy::significant_drop_tightening,
    reason = "the install's guard is held for the whole test: that's what keeps them in turn"
)]

#[allow(
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use common::{
    TestDir, broker_config, guard_cell_available, harness_fixture_binary, mapped_agent_uid,
    run_fixture_binary, test_dir,
};
use hatchling_broker::browser_credentials::{SignIn, SignInAnswer, SignInRequest};
use hatchling_broker::browser_ops::{Browser, BrowserSession, Browsers, handle_browser_op};
use hatchling_broker::browser_proxy::Sites;
use hatchling_broker::chat_run::{Chat, Step, stopped_told};
use hatchling_broker::harness::HARNESS_CAPABILITY;
use hatchling_broker::held_check::{CHECK_PURPOSE, HeldHit, MAX_CHECK_BYTES};
use hatchling_broker::keeper::{self, Keeper};
use hatchling_broker::schedule_ops::HubDelivery;
use hatchling_broker::vault_access::{Access, CHECK_ACTION, Caller, Outcome, read_log};
use hatchling_broker::web_ops::{Fetcher, Searcher, WebSession, handle_web_op};
use hatchling_broker::{
    AgentCell, CellLauncher, Config, Harnesses, RunEnv, SkillRuntime, Skills, VaultConfig,
    serve_cell_connection, serve_connection_with_ingress,
};
use hatchling_channel::{ChannelId, Cleared, LocalHub, Message, OutputGate, SenderId};
use hatchling_core::browser::{BrowserFailure, PageAction};
use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_core::ui::Concealed;
use hatchling_core::{
    AgentMessage, BrokerMessage, BrowserOp, BrowserResult, CellId, PROTOCOL_VERSION, PeerUid,
    ScheduleGrant, SessionRequest, SessionResponse, SkillOp, SkillResult, WebOp, WebResult,
};
use hatchling_egress::web::FetchError;
use hatchling_scheduler::{SentTo, Timestamp};
use hatchling_skill::CodeHash;
use hatchling_vault::StoreKeys;
use serde_json::{Value, json};

/// An API key, in the vault under the name a grant reads it by.
const API_KEY: &str = "sk-egress-no-cell-has-had-0123456";
/// A channel account's credential.
const ACCOUNT_KEY: &str = "mail-account-no-cell-has-had-1234";
/// A sign-in's password, with a space, as a query would carry it.
const PASSWORD: &str = "correct horse sign-in password";
/// A key shorter than anything looked for inside a text.
const SHORT_KEY: &str = "pw-1234";

/// A sign-in's username, which isn't a secret.
const USERNAME: &str = "zoe-the-username";

/// What the agent's told when an address holds one.
const ADDRESS_HOLDS: &str = "the page's address holds a secret the broker keeps: it isn't sent";
/// What the agent's told when typed text holds one.
const TYPED_HOLDS: &str = "the text holds a secret the broker keeps: it isn't typed";

/// An install: a sealed vault and its keeper, which this process asks.
struct Install {
    dir: TestDir,
    config: Config,
    keys: StoreKeys,
    /// The keeper's other channels, open as on an install.
    _channels: (UnixStream, UnixStream),
}

/// The keeper's environment under `dir`.
fn environment(dir: &TestDir) -> Vec<(&'static str, String)> {
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    vec![
        ("TMPDIR", path("tmp")),
        ("HATCHLING_BROKER_SOCKET", path("sock/broker.sock")),
        (
            "HATCHLING_AGENT_UID",
            rustix::process::getuid().as_raw().to_string(),
        ),
        ("HATCHLING_LOG_LEVEL", String::from("error")),
        ("HATCHLING_VAULT_DIR", path("vault")),
        ("HATCHLING_EGRESS_GRANTS", String::from("localhost:18089")),
        ("HATCHLING_ARTIFACT_DIR", path("artifacts")),
        ("HATCHLING_BROWSER_PROFILES_DIR", path("profiles")),
        ("HATCHLING_BROWSER_CREDENTIALS_DIR", path("sign-ins")),
    ]
}

/// Seals the keys in the vault under `dir`, as an operator does.
fn seal(dir: &TestDir) -> Result<(), String> {
    let vault = dir.join("vault");
    std::fs::create_dir(&vault).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let stored = [
        ("egress-localhost-18089", API_KEY),
        ("mail-work", ACCOUNT_KEY),
        ("short", SHORT_KEY),
    ];
    for (name, value) in stored {
        std::fs::write(vault.join(name), value).map_err(|e| e.to_string())?;
    }
    let migrated = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("migrate-vault")
        .envs(environment(dir))
        .env_remove("HATCHLING_BROWSER_PROFILES_DIR")
        .env_remove("HATCHLING_BROWSER_CREDENTIALS_DIR")
        .output()
        .map_err(|e| e.to_string())?;
    if migrated.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&migrated.stderr).into_owned())
    }
}

impl Install {
    /// The install: its keeper made this process's, holding a sign-in as
    /// well as the keys. None of it is anything a cell has had, or every
    /// refusal below could come from the old look at what a cell has had.
    fn new() -> Result<Self, String> {
        let dir = test_dir("leaving");
        let _ = std::fs::remove_dir_all(&*dir);
        for sub in ["sock", "tmp"] {
            std::fs::create_dir_all(dir.join(sub)).map_err(|e| e.to_string())?;
        }
        seal(&dir)?;
        let mut program = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"));
        program
            .envs(environment(&dir))
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        let (keeper, keys, authority, spawner) =
            Keeper::start_with(program).map_err(|e| e.to_string())?;
        let kept = keeper
            .sign_ins(SignInRequest::Keep {
                profile_id: String::from("profile-1"),
                origin: String::from("https://a.example"),
                label: String::from("Work"),
                sign_in: SignIn {
                    username: Concealed::new(String::from(USERNAME)),
                    password: Concealed::new(String::from(PASSWORD)),
                },
                now: 1,
            })
            .map_err(|e| e.to_string())?;
        if !matches!(kept, SignInAnswer::Kept { .. }) {
            return Err(String::from("the sign-in wasn't kept"));
        }
        let scanned = keeper.scan("output-gate").map_err(|e| e.to_string())?;
        if !scanned.is_empty() {
            return Err(String::from("a cell has had one of these"));
        }
        keeper::install(keeper).map_err(|e| e.to_string())?;
        let uid = rustix::process::getuid().as_raw();
        let mut config = broker_config(&dir.join("sock/broker.sock"), uid.wrapping_add(1))?;
        config.vault.root = dir.join("vault");
        config.vault.key_file = VaultConfig::default_key_file(&config.vault.root);
        config.web.enabled = true;
        config.web.search = true;
        Ok(Self {
            dir,
            config,
            keys,
            _channels: (authority, spawner),
        })
    }

    /// The checks in the access log.
    fn checks(&self) -> Result<Vec<Access>, String> {
        let (mut accesses, unreadable) =
            read_log(&self.dir.join("vault")).map_err(|e| e.to_string())?;
        assert_eq!(unreadable, 0);
        accesses.retain(|access| access.action == CHECK_ACTION);
        Ok(accesses)
    }

    /// That the last check is the broker's, and found what's said: the
    /// keeper's record of what it was asked.
    fn last_check_found(&self, found: bool) -> Result<(), String> {
        let checks = self.checks()?;
        let last = checks.last().ok_or("no check on record")?;
        assert_eq!(
            last.caller,
            Caller::Broker {
                purpose: CHECK_PURPOSE.to_owned()
            }
        );
        assert_eq!(last.found, Some(found));
        assert_eq!(last.outcome, Outcome::Allowed);
        assert!(last.names.is_empty(), "a check names no secret");
        Ok(())
    }
}

/// The one install, for one test at a time. A test that fails doesn't stop
/// the others.
fn install() -> Result<MutexGuard<'static, Install>, String> {
    static INSTALL: OnceLock<Result<Mutex<Install>, String>> = OnceLock::new();
    let install = INSTALL
        .get_or_init(|| Install::new().map(Mutex::new))
        .as_ref()
        .map_err(String::clone)?;
    Ok(install.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Every byte of `text` as a percent escape.
fn escaped(text: &str) -> String {
    text.bytes().fold(String::new(), |mut all, byte| {
        let _ = write!(all, "%{byte:02x}");
        all
    })
}

fn hex(text: &str) -> String {
    text.bytes().fold(String::new(), |mut all, byte| {
        let _ = write!(all, "{byte:02x}");
        all
    })
}

/// Every way an address can carry the keys: in its query, in its path,
/// escaped byte by byte, and encoded.
fn addresses_holding() -> Vec<String> {
    vec![
        format!("https://a.example/?k={API_KEY}"),
        format!("https://a.example/{API_KEY}/page"),
        format!("https://a.example/?k={}", escaped(API_KEY)),
        format!("https://a.example/{}", escaped(ACCOUNT_KEY)),
        format!("https://a.example/?k={}", hex(API_KEY)),
        format!(
            "https://a.example/?k={}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(API_KEY)
        ),
        format!("https://a.example/?p={}", PASSWORD.replace(' ', "+")),
        format!("https://a.example/?p={}", PASSWORD.replace(' ', "%20")),
    ]
}

/// A session reading with a fetcher that counts what got past the gate.
fn counting(task: &str) -> (WebSession, Arc<AtomicUsize>) {
    let reads = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&reads);
    let fetcher: Fetcher = Arc::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Err(FetchError::Failed)
    });
    (
        WebSession::with(task, Duration::ZERO, Duration::from_secs(5), fetcher),
        reads,
    )
}

/// A web read of `url`.
fn reading(url: &str) -> WebOp {
    WebOp::Fetch {
        url: url.to_owned(),
        reason: String::from("to read it"),
    }
}

/// A hub whose client keeps what the owner is shown.
fn owners_chat() -> (LocalHub, Arc<Mutex<Vec<String>>>) {
    let hub = LocalHub::new(OutputGate::disabled());
    let shown: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&shown);
    let _ = hub.attach(Box::new(move |cleared: &Cleared| {
        if let Ok(mut seen) = seen.lock() {
            seen.push(cleared.message().text.clone());
        }
        Ok(())
    }));
    (hub, shown)
}

/// That `told`, what the owner was shown, holds no secret and no name.
fn says_nothing_held(told: &[String]) {
    let names = ["egress-localhost-18089", "mail-work", "short", USERNAME];
    for line in told {
        for held in [API_KEY, ACCOUNT_KEY, PASSWORD, SHORT_KEY]
            .iter()
            .chain(&names)
        {
            assert!(!line.contains(held), "{line}");
        }
    }
}

/// The agent's end of a session on a socket pair, past its hello.
fn say_hello(agent: &UnixStream, cell: &str) -> Result<(), String> {
    agent
        .set_read_timeout(Some(Duration::from_secs(20)))
        .map_err(|e| e.to_string())?;
    let hello = AgentMessage::Hello {
        v: PROTOCOL_VERSION,
        cell_id: cell.to_owned(),
    };
    let bytes = serde_json::to_vec(&hello).map_err(|e| e.to_string())?;
    write_frame(agent, &bytes).map_err(|e| e.to_string())?;
    let welcome = read_frame(agent).map_err(|e| e.to_string())?;
    match serde_json::from_slice(&welcome).map_err(|e| e.to_string())? {
        BrokerMessage::Welcome { .. } => Ok(()),
        other @ BrokerMessage::Reject { .. } => Err(format!("{other:?}")),
    }
}

/// Closes the agent's end when a test ends, however it ends: the scope a
/// session runs in can't finish until the session does, and a test that
/// fails has to fail, not hang.
struct Hangs<'a>(&'a UnixStream);

impl Drop for Hangs<'_> {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// Sends `request` and gives the broker's answer to it.
fn answer(agent: &UnixStream, request: &SessionRequest) -> Result<SessionResponse, String> {
    send(agent, request);
    let reply = read_frame(agent).map_err(|e| e.to_string())?;
    serde_json::from_slice(&reply).map_err(|e| e.to_string())
}

/// Sends `op` and gives the broker's answer to it.
fn ask(agent: &UnixStream, op: &WebOp) -> Result<SessionResponse, String> {
    answer(agent, &SessionRequest::Web(op.clone()))
}

/// Sends `request`, if the connection still takes it: a session that's over
/// may have shut it already.
fn send(agent: &UnixStream, request: &SessionRequest) {
    if let Ok(bytes) = serde_json::to_vec(request) {
        let _ = write_frame(agent, &bytes);
    }
}

/// That the broker closed the session on `agent`, with nothing more asked
/// of it.
fn session_closed(agent: &UnixStream) -> Result<(), String> {
    match read_frame_or_eof(agent) {
        Ok(None) => Ok(()),
        other => Err(format!("the session wasn't closed: {other:?}")),
    }
}

/// That the session on `agent` answers nothing more: it's closed to it.
fn session_is_over(agent: &UnixStream) -> Result<(), String> {
    let next = SessionRequest::Web(reading("https://a.example/?k=nothing-held"));
    send(agent, &next);
    match read_frame_or_eof(agent) {
        Ok(None) => Ok(()),
        Err(error) if error.to_string().contains("reset") => Ok(()),
        other => Err(format!("the session answered: {other:?}")),
    }
}

/// A browser that never starts: the gate is what's asked of it.
struct NoBrowsers;

impl Browsers for NoBrowsers {
    fn start(&self, _: &str, _: Sites, _: Option<&str>) -> Result<Box<dyn Browser>, String> {
        Err(String::from("no browser here"))
    }
}

/// A browser session with none open.
fn browsing() -> BrowserSession {
    BrowserSession::new(Some(Arc::new(NoBrowsers)))
}

fn typing(text: &str) -> BrowserOp {
    BrowserOp::Act {
        element: 3,
        action: PageAction::Type {
            text: text.to_owned(),
        },
    }
}

/// An API key no cell has had is refused in a web fetch's address, however
/// it's carried, with nothing fetched and each check on the keeper's
/// record. A clean address goes on, to the owner: so what stopped them was
/// this gate.
#[test]
fn a_key_no_cell_has_had_never_leaves_in_a_web_fetch() -> Result<(), String> {
    let install = install()?;
    let (mut web, reads) = counting("fetch");
    let fetch = |web: &mut WebSession, url: &str| {
        let op = WebOp::Fetch {
            url: url.to_owned(),
            reason: String::from("to read it"),
        };
        handle_web_op(&install.config, &install.keys, web, &op)
    };
    for url in addresses_holding() {
        assert_eq!(
            fetch(&mut web, &url),
            WebResult::Refused {
                reason: String::from(ADDRESS_HOLDS)
            },
            "{url}"
        );
        install.last_check_found(true)?;
    }
    let clean = fetch(&mut web, "https://a.example/?k=nothing-held");
    assert!(
        matches!(clean, WebResult::Waiting { .. }),
        "a clean address goes on to the owner"
    );
    install.last_check_found(false)?;
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "nothing is fetched before the owner says"
    );
    Ok(())
}

/// A key in a search never reaches the search provider, a clean one does,
/// and the same words short of the key are clean. A short key counts only
/// as the whole search.
#[test]
fn a_key_no_cell_has_had_never_leaves_in_a_search() -> Result<(), String> {
    let install = install()?;
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&asked);
    let searcher: Searcher = Arc::new(move |query| {
        if let Ok(mut asked) = log.lock() {
            asked.push(query.to_owned());
        }
        Ok(Vec::new())
    });
    let (web, _) = counting("search");
    let mut web = web.searching(searcher);
    let search = |web: &mut WebSession, query: &str| {
        let op = WebOp::Search {
            query: query.to_owned(),
        };
        handle_web_op(&install.config, &install.keys, web, &op)
    };
    let holds = WebResult::Refused {
        reason: String::from("the search holds a secret the broker keeps: it isn't sent"),
    };
    assert_eq!(search(&mut web, &format!("what is {API_KEY}")), holds);
    assert_eq!(search(&mut web, SHORT_KEY), holds);
    assert_eq!(
        search(&mut web, &format!("what is {SHORT_KEY}")),
        WebResult::Results {
            results: Vec::new()
        }
    );
    assert_eq!(
        *asked.lock().map_err(|e| e.to_string())?,
        [format!("what is {SHORT_KEY}")]
    );
    Ok(())
}

/// With no answer from the keeper, nothing leaves: a text it won't read is
/// refused as one it can't check, not passed as clean.
#[test]
fn a_check_the_keeper_refuses_sends_nothing() -> Result<(), String> {
    let install = install()?;
    let asked = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    let searcher: Searcher = Arc::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Vec::new())
    });
    let (web, _) = counting("too-long");
    let mut web = web.searching(searcher);
    let length = usize::try_from(MAX_CHECK_BYTES + 1).map_err(|e| e.to_string())?;
    let op = WebOp::Search {
        query: "x".repeat(length),
    };
    assert_eq!(
        handle_web_op(&install.config, &install.keys, &mut web, &op),
        WebResult::Refused {
            reason: String::from("the broker couldn't check the search for secrets: it isn't sent")
        }
    );
    assert_eq!(asked.load(Ordering::Relaxed), 0);
    let checks = install.checks()?;
    let last = checks.last().ok_or("no check on record")?;
    assert_eq!((last.found, last.outcome), (None, Outcome::Denied));
    Ok(())
}

/// A key no cell has had, or a sign-in's password, is never typed into a
/// page, and each try is on the keeper's record. Other text goes on, to a
/// browser that has no page.
#[test]
fn a_key_no_cell_has_had_is_never_typed() -> Result<(), String> {
    let install = install()?;
    let (mut web, _) = counting("typing");
    let mut browser = browsing();
    let mut act = |op: &BrowserOp| {
        handle_browser_op(&install.config, &install.keys, &mut web, &mut browser, op)
    };
    let refused = BrowserResult::Refused {
        reason: String::from(TYPED_HOLDS),
    };
    for text in [
        API_KEY.to_owned(),
        format!("my key is {API_KEY}, thanks"),
        hex(ACCOUNT_KEY),
        base64::engine::general_purpose::STANDARD.encode(API_KEY),
        PASSWORD.to_owned(),
        SHORT_KEY.to_owned(),
    ] {
        assert_eq!(act(&typing(&text)), refused, "{text}");
        install.last_check_found(true)?;
    }
    let before = install.checks()?.len();
    for clean in ["hello", USERNAME, "pw-123", "sk-egress-no-cell-has-had"] {
        assert_eq!(
            act(&typing(clean)),
            BrowserResult::Failed {
                failure: BrowserFailure::NoPage
            },
            "{clean}"
        );
        install.last_check_found(false)?;
    }
    assert_eq!(
        install.checks()?.len(),
        before + 4,
        "one check for each act of typing"
    );
    Ok(())
}

/// A page's address is held to the same as a web read's, however it's
/// carried, and the browser isn't started for one. A clean one goes on.
#[test]
fn a_key_no_cell_has_had_is_never_gone_to() -> Result<(), String> {
    let install = install()?;
    let (mut web, _) = counting("going");
    let mut browser = browsing();
    let mut open = |url: &str| {
        let op = BrowserOp::Open {
            url: url.to_owned(),
            reason: String::from("to read it"),
        };
        handle_browser_op(&install.config, &install.keys, &mut web, &mut browser, &op)
    };
    for url in addresses_holding() {
        assert_eq!(
            open(&url),
            BrowserResult::Refused {
                reason: String::from(ADDRESS_HOLDS)
            },
            "{url}"
        );
        install.last_check_found(true)?;
    }
    let clean = open("https://a.example/?k=nothing-held");
    assert!(
        !matches!(&clean, BrowserResult::Refused { reason } if reason.contains("secret")),
        "a clean text is not refused for a secret"
    );
    install.last_check_found(false)?;
    Ok(())
}

/// Every way the agent's own text leaves ends its run on a hit, and says
/// where it was going. A clean text ends nothing, and nor does a check the
/// keeper can't answer: nothing says that one held a secret.
#[test]
fn a_hit_ends_the_run_each_way_out_and_nothing_else_does() -> Result<(), String> {
    let install = install()?;
    let searcher: Searcher = Arc::new(|_| Ok(Vec::new()));
    let ways: [(WaySent, SentTo); 4] = [
        (
            WaySent::Web(WebOp::Fetch {
                url: format!("https://a.example/?k={API_KEY}"),
                reason: String::from("to read it"),
            }),
            SentTo::Website,
        ),
        (
            WaySent::Web(WebOp::Search {
                query: format!("what is {ACCOUNT_KEY}"),
            }),
            SentTo::Search,
        ),
        (
            WaySent::Browser(BrowserOp::Open {
                url: format!("https://a.example/{}", escaped(PASSWORD)),
                reason: String::from("to read it"),
            }),
            SentTo::Website,
        ),
        (WaySent::Browser(typing(SHORT_KEY)), SentTo::Page),
    ];
    for (way, to) in ways {
        let hit = HeldHit::default();
        let (web, _) = counting("each-way");
        let mut web = web.searching(Arc::clone(&searcher)).in_run(hit.clone());
        let mut browser = browsing();
        way.go(&install, &mut web, &mut browser);
        assert_eq!(hit.seen(), Some(to), "{way:?}");
        install.last_check_found(true)?;
    }

    let hit = HeldHit::default();
    let (web, _) = counting("clean");
    let mut web = web.searching(Arc::clone(&searcher)).in_run(hit.clone());
    let mut browser = browsing();
    let length = usize::try_from(MAX_CHECK_BYTES + 1).map_err(|e| e.to_string())?;
    for way in [
        WaySent::Web(reading("https://a.example/?k=nothing-held")),
        WaySent::Web(WebOp::Search {
            query: String::from("nothing held"),
        }),
        WaySent::Web(WebOp::Search {
            query: "x".repeat(length),
        }),
        WaySent::Browser(typing("nothing held")),
    ] {
        way.go(&install, &mut web, &mut browser);
        assert_eq!(hit.seen(), None, "{way:?}");
    }
    Ok(())
}

/// One op on the way out.
#[derive(Debug)]
enum WaySent {
    Web(WebOp),
    Browser(BrowserOp),
}

impl WaySent {
    fn go(&self, install: &Install, web: &mut WebSession, browser: &mut BrowserSession) {
        match self {
            Self::Web(op) => {
                let _ = handle_web_op(&install.config, &install.keys, web, op);
            }
            Self::Browser(op) => {
                let _ = handle_browser_op(&install.config, &install.keys, web, browser, op);
            }
        }
    }
}

/// The attack: a tricked agent tries a guess in an address. A hit is on the
/// keeper's record, is answered as ever, and is the last thing the session
/// does: it's closed to the agent, though the broker's end is still open,
/// and the owner is told once, in words that hold no secret and no name. A
/// clean read before it went on as usual.
#[test]
fn a_hit_ends_the_session_and_the_owner_is_told_once() -> Result<(), String> {
    let install = install()?;
    let mut config = install.config.clone();
    config.agent_uid = PeerUid::from_kernel(rustix::process::getuid().as_raw());
    let (hub, shown) = owners_chat();
    let (agent, broker) = UnixStream::pair().map_err(|e| e.to_string())?;
    std::thread::scope(|scope| {
        let _hangs = Hangs(&agent);
        let (config, keys, hub, broker) = (&config, &install.keys, &hub, &broker);
        let served = scope.spawn(move || {
            serve_connection_with_ingress(broker, config, keys, &Skills::default(), Some(hub))
                .map_err(|e| e.to_string())
        });
        say_hello(&agent, "cell-guessing")?;
        let clean = ask(&agent, &reading("https://a.example/?k=nothing-held"))?;
        assert!(
            matches!(clean, SessionResponse::Web(WebResult::Waiting { .. })),
            "a clean text is not refused for a secret"
        );
        install.last_check_found(false)?;
        let guess = format!("https://a.example/?k={API_KEY}");
        assert_eq!(
            ask(&agent, &reading(&guess))?,
            SessionResponse::Web(WebResult::Refused {
                reason: String::from(ADDRESS_HOLDS)
            })
        );
        install.last_check_found(true)?;
        let checks = install.checks()?.len();
        session_closed(&agent)?;
        session_is_over(&agent)?;
        served
            .join()
            .map_err(|_| String::from("the session panicked"))??;
        assert_eq!(install.checks()?.len(), checks, "the next op wasn't read");
        Ok::<(), String>(())
    })?;
    let told = shown.lock().map_err(|e| e.to_string())?.clone();
    assert_eq!(told, [stopped_told(SentTo::Website)]);
    says_nothing_held(&told);
    Ok(())
}

/// A harness's brief is one more way out, and the agent's own text: through
/// a real session, a key in it stops the run as an address does, and the
/// owner's told it was a coding tool.
#[test]
fn a_brief_holding_a_key_ends_the_session() -> Result<(), String> {
    let install = install()?;
    let dir: PathBuf = install.dir.join("harness");
    std::fs::create_dir_all(dir.join("project/src")).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("project/src/lib.rs"), "pub fn a() {}\n").map_err(|e| e.to_string())?;
    let binary = dir.join("harness");
    std::fs::copy(harness_fixture_binary()?, &binary).map_err(|e| e.to_string())?;
    let sha256 = CodeHash::of(&std::fs::read(&binary).map_err(|e| e.to_string())?).to_string();
    let mut config = install.config.clone();
    config.agent_uid = PeerUid::from_kernel(rustix::process::getuid().as_raw());
    config.artifacts.workspace = Some(install.dir.join("tmp"));
    config.scheduler.root = dir.join("schedules");
    let code: HashSet<_> = [HARNESS_CAPABILITY.parse().map_err(|_| "cap")?].into();
    config.skills.grants = HashMap::from([(String::from("claude-code"), code)]);
    let vars = HashMap::from([
        (
            "HATCHLING_HARNESS_CLAUDE_CODE",
            binary.display().to_string(),
        ),
        ("HATCHLING_HARNESS_CLAUDE_CODE_SHA256", sha256),
        (
            "HATCHLING_HARNESS_ROOTS",
            format!("project={}", dir.join("project").display()),
        ),
    ]);
    let harnesses =
        Harnesses::parse(&config, |key| vars.get(key).cloned()).map_err(|e| e.to_string())?;
    let skills = Skills::default()
        .with_runtime(SkillRuntime::new(PathBuf::from(
            "/nonexistent/hatchling-spawner",
        )))
        .with_harnesses(harnesses);
    let (hub, shown) = owners_chat();
    let (agent, broker) = UnixStream::pair().map_err(|e| e.to_string())?;
    let request = SessionRequest::Skill(SkillOp::Invoke {
        skill: String::from("claude-code@1"),
        capability: "code".parse().map_err(|_| "capability")?,
        input: json!({
            "task": format!("Log in with {API_KEY}."),
            "root": "project",
            "paths": ["src/lib.rs"],
        })
        .to_string(),
    });
    std::thread::scope(|scope| {
        let _hangs = Hangs(&agent);
        let (config, keys, hub, broker, skills) = (&config, &install.keys, &hub, &broker, &skills);
        let served = scope.spawn(move || {
            serve_connection_with_ingress(broker, config, keys, skills, Some(hub))
                .map_err(|e| e.to_string())
        });
        say_hello(&agent, "cell-briefing")?;
        let reply = answer(&agent, &request)?;
        let SessionResponse::Skill(SkillResult::Output { output }) = reply else {
            return Err(format!("no report: {reply:?}"));
        };
        let report: Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
        assert_eq!(report.get("outcome"), Some(&json!("refused")), "{report}");
        session_closed(&agent)?;
        session_is_over(&agent)?;
        served
            .join()
            .map_err(|_| String::from("the session panicked"))?
    })?;
    let told = shown.lock().map_err(|e| e.to_string())?.clone();
    assert_eq!(told, [stopped_told(SentTo::Harness)]);
    says_nothing_held(&told);
    Ok(())
}

/// A hit elsewhere in a run's tree stops a session that did nothing wrong:
/// its next op is refused unread, whatever it is. A run's session has no
/// local channel, so it tells no one: what watches the run does, once.
#[test]
fn a_hit_elsewhere_in_the_tree_refuses_the_next_op() -> Result<(), String> {
    let install = install()?;
    let cell_id: CellId = "cell-sibling".parse().map_err(|_| "cell id")?;
    let (agent, connection) = UnixStream::pair().map_err(|e| e.to_string())?;
    let cell = AgentCell {
        connection,
        cell_id,
        pid: 0,
    };
    let hit = HeldHit::default();
    std::thread::scope(|scope| {
        let _hangs = Hangs(&agent);
        let (hit, cell, config, keys) = (&hit, &cell, &install.config, &install.keys);
        let served = scope.spawn(move || {
            serve_cell_connection(cell, config, keys, &Skills::default(), None, None, hit)
                .map_err(|e| e.to_string())
        });
        say_hello(&agent, "cell-sibling")?;
        let clean = ask(&agent, &reading("https://a.example/?k=nothing-held"))?;
        assert!(
            matches!(clean, SessionResponse::Web(WebResult::Waiting { .. })),
            "a clean text is not refused for a secret"
        );
        hit.record(SentTo::Search);
        let checks = install.checks()?.len();
        session_is_over(&agent)?;
        served
            .join()
            .map_err(|_| String::from("the session panicked"))??;
        assert_eq!(install.checks()?.len(), checks, "unread");
        Ok::<(), String>(())
    })
}

/// End to end, with nothing standing in for the keeper or the cell: a chat
/// run's program sends a key in an address, then goes on spinning as a
/// compromised one would. The broker ends the run anyway, long before its
/// time, kills its cell, and shows the owner one sentence: nothing it wrote.
#[test]
fn a_chat_run_that_sends_a_key_is_stopped_and_the_owner_told_once() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let install = install()?;
    let mut config = install.config.clone();
    config.agent_uid = PeerUid::from_kernel(mapped_agent_uid());
    config.scheduler.root = install.dir.join("schedules");
    let runs = RunEnv {
        spawner: common::spawner(),
        agent_bin: run_fixture_binary()?,
        run_root: config.scheduler.run_root(),
        timeout: Duration::from_secs(120),
    };
    let (hub, shown) = owners_chat();
    let delivery = HubDelivery::new(Some(&hub));
    let send = ScheduleGrant::ChannelSend {
        channel: String::from("local"),
    };
    let task = format!("fetch:https://a.example/?k={API_KEY} hang");
    let started = Instant::now();
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &install.keys, runs.clone());
        let mut chat = Chat::new(vec![send]);
        let mut message = Some((
            Message {
                sender: SenderId(String::from("api")),
                channel: ChannelId::local(),
                text: task.clone(),
            },
            0,
        ));
        loop {
            let step = chat.step(|| message.take(), 0, &launcher, &delivery, Timestamp::now());
            if step == Step::Answered {
                return Ok(());
            }
            if started.elapsed() > Duration::from_secs(60) {
                return Err(format!("the run wasn't stopped: {step:?}"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })?;
    install.last_check_found(true)?;
    let told = shown.lock().map_err(|e| e.to_string())?.clone();
    assert_eq!(told, [stopped_told(SentTo::Website)]);
    says_nothing_held(&told);
    let left = std::fs::read_dir(&runs.run_root)
        .map_err(|e| e.to_string())?
        .count();
    assert_eq!(left, 0, "the run's directory went with it");
    Ok(())
}
