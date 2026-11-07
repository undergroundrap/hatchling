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

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use common::{TestDir, broker_config, test_dir};
use hatchling_broker::browser_credentials::{SignIn, SignInAnswer, SignInRequest};
use hatchling_broker::browser_ops::{Browser, BrowserSession, Browsers, handle_browser_op};
use hatchling_broker::browser_proxy::Sites;
use hatchling_broker::held_check::{CHECK_PURPOSE, MAX_CHECK_BYTES};
use hatchling_broker::keeper::{self, Keeper};
use hatchling_broker::vault_access::{Access, CHECK_ACTION, Caller, Outcome, read_log};
use hatchling_broker::web_ops::{Fetcher, Searcher, WebSession, handle_web_op};
use hatchling_broker::{Config, VaultConfig};
use hatchling_core::browser::{BrowserFailure, PageAction};
use hatchling_core::ui::Concealed;
use hatchling_core::{BrowserOp, BrowserResult, WebOp, WebResult};
use hatchling_egress::web::FetchError;
use hatchling_vault::StoreKeys;

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
        "a clean address goes on to the owner: {clean:?}"
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
        "{clean:?}"
    );
    install.last_check_found(false)?;
    Ok(())
}
