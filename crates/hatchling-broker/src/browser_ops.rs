//! The agent's browser (WO-029, part 3): each [`BrowserOp`] answered by a
//! browser cell of the session's own, started on its first page and ended
//! with the session.
//!
//! - **A page opens only on a site the session may reach,** decided as a
//!   `web_fetch` is ([`crate::web_ops`]): the session holds it, the owner
//!   always allows it, or the owner is asked and allows it once, for the
//!   task, or always. A site allowed for one is allowed for the other.
//! - **Every session is strict.** The cell's proxy admits only the sites
//!   the session holds ([`Sites`]). What a page asks of another site is
//!   refused, and listed with the page.
//! - **Once is one visit.** A site allowed once is held while the browser
//!   stays on it, and revoked, its tunnels closed, when the browser opens
//!   another site.
//! - **Typed text is checked:** text that holds a secret the broker keeps
//!   is refused before it reaches the page.
//! - **Bounded:** one browser per session, at most [`MAX_BROWSERS`] in
//!   all, each for at most its lifetime; it ends with the session.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use hatchling_core::browser::{BrowserCommand, BrowserFailure, BrowserReply, PageAction};
use hatchling_core::{BrowserOp, BrowserResult, SessionResponse, WebResult};
use hatchling_egress::web::WebUrl;
use hatchling_vault::OpensStores;

use crate::browser_cell::{BrowserCell, BrowserLauncher};
use crate::browser_proxy::{Route, Sites};
use crate::browser_session::BrowserSettings;
use crate::cell::cgroup_parent;
use crate::web_ops::WebSession;
use crate::{CellIds, Config, Error};

/// Most browsers running at once, across every session.
pub const MAX_BROWSERS: usize = 4;

/// How long one browser lives: past it, it takes no more commands, and
/// the next page starts another.
pub const BROWSER_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// A running browser, as a session drives it.
pub trait Browser: Send {
    /// Sends `command`, and returns the reply.
    ///
    /// # Errors
    ///
    /// A sentence for the broker's log: the browser takes no more
    /// commands.
    fn command(&mut self, command: &BrowserCommand) -> Result<BrowserReply, String>;

    /// Closes the browser and ends its cell.
    fn finish(self: Box<Self>);
}

/// What starts browsers.
pub trait Browsers: Send + Sync {
    /// Starts a browser in the cell `cell_id`, reaching only `sites`.
    ///
    /// # Errors
    ///
    /// A sentence for the broker's log: it couldn't start.
    fn start(&self, cell_id: &str, sites: Sites) -> Result<Box<dyn Browser>, String>;
}

impl Browser for BrowserCell {
    fn command(&mut self, command: &BrowserCommand) -> Result<BrowserReply, String> {
        Self::command(self, command)
    }

    fn finish(self: Box<Self>) {
        let count = Self::finish(*self);
        tracing::info!(
            tunnelled = count.tunnelled,
            refused = count.refused,
            "browser closed"
        );
    }
}

impl Browsers for BrowserLauncher {
    fn start(&self, cell_id: &str, sites: Sites) -> Result<Box<dyn Browser>, String> {
        let cell: Box<dyn Browser> = Box::new(Self::start(self, cell_id, sites)?);
        Ok(cell)
    }
}

/// `browsers`, with at most `max` running at once.
pub struct Capped {
    browsers: Arc<dyn Browsers>,
    live: Arc<AtomicUsize>,
    max: usize,
}

impl Capped {
    /// `browsers`, at most `max` at once.
    #[must_use]
    pub fn new(browsers: Arc<dyn Browsers>, max: usize) -> Self {
        Self {
            browsers,
            live: Arc::new(AtomicUsize::new(0)),
            max,
        }
    }
}

impl Browsers for Capped {
    fn start(&self, cell_id: &str, sites: Sites) -> Result<Box<dyn Browser>, String> {
        if self.live.fetch_add(1, Ordering::SeqCst) >= self.max {
            self.live.fetch_sub(1, Ordering::SeqCst);
            return Err(format!("{} browsers are running already", self.max));
        }
        // From here the place is the browser's, given back when it ends,
        // or now if it doesn't start.
        let place = Place(Arc::clone(&self.live));
        let browser = self.browsers.start(cell_id, sites)?;
        Ok(Box::new(Counted {
            browser: Some(browser),
            _place: place,
        }))
    }
}

/// A browser's place among [`Capped`]'s, given back when it's dropped.
struct Place(Arc<AtomicUsize>);

impl Drop for Place {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A browser holding its place.
struct Counted {
    browser: Option<Box<dyn Browser>>,
    _place: Place,
}

impl Browser for Counted {
    fn command(&mut self, command: &BrowserCommand) -> Result<BrowserReply, String> {
        self.browser.as_mut().map_or_else(
            || Err(String::from("closed")),
            |browser| browser.command(command),
        )
    }

    fn finish(mut self: Box<Self>) {
        if let Some(browser) = self.browser.take() {
            browser.finish();
        }
    }
}

/// The broker's browsers, once [`configure`] has set them up.
static BROWSERS: OnceLock<Arc<dyn Browsers>> = OnceLock::new();

/// The broker's browsers, if the operator set one up.
#[must_use]
pub fn configured() -> Option<Arc<dyn Browsers>> {
    BROWSERS.get().cloned()
}

/// Sets up the broker's browsers from its environment, started by
/// `spawner`: [`configure`] with the broker's own browser settings,
/// resource group and cell ids.
///
/// # Errors
///
/// As [`configure`], and an invalid browser setting, resource group or
/// `HATCHLING_CELL_IDS`.
pub fn configure_from_env(spawner: PathBuf) -> Result<bool, Error> {
    let cgroup = cgroup_parent(&|key| crate::cell::env_value(std::env::var(key)))?;
    let settings = BrowserSettings::parse(|key| crate::cell::env_value(std::env::var(key)))?;
    let ids = CellIds::parse(|key| crate::cell::env_value(std::env::var(key)))?;
    configure(settings, cgroup, spawner, ids)
}

/// Sets up the broker's browsers for `settings`, in groups of their own
/// under `cgroup`, started by `spawner` on the ids `ids`: nothing when no
/// browser is configured. Called once, at startup.
///
/// # Errors
///
/// [`Error::InvalidConfig`]: a browser is configured without a resource
/// group (`HATCHLING_CELL_CGROUP`), which every browser cell needs.
pub fn configure(
    settings: Option<BrowserSettings>,
    cgroup: Option<PathBuf>,
    spawner: PathBuf,
    ids: CellIds,
) -> Result<bool, Error> {
    let Some(settings) = settings else {
        return Ok(false);
    };
    let cgroup = cgroup.ok_or_else(|| Error::InvalidConfig {
        key: "HATCHLING_CELL_CGROUP",
        reason: String::from("a browser cell is bounded as a whole or not started: set it"),
    })?;
    let launcher = BrowserLauncher::new(
        spawner,
        settings,
        ids,
        cgroup,
        Route::public(),
        BROWSER_LIFETIME,
    );
    let browsers: Arc<dyn Browsers> = Arc::new(Capped::new(Arc::new(launcher), MAX_BROWSERS));
    Ok(BROWSERS.set(browsers).is_ok())
}

/// One session's browser: the sites it holds, and the browser once it's
/// started. Dropped, its browser is closed.
pub struct BrowserSession {
    browsers: Option<Arc<dyn Browsers>>,
    browser: Option<Box<dyn Browser>>,
    sites: Sites,
    /// The sites held for one visit.
    once: BTreeSet<String>,
}

impl std::fmt::Debug for BrowserSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSession")
            .field("open", &self.browser.is_some())
            .field("once", &self.once)
            .finish_non_exhaustive()
    }
}

impl BrowserSession {
    /// A session's browser, started by `browsers`; with none, the browser
    /// is off.
    #[must_use]
    pub fn new(browsers: Option<Arc<dyn Browsers>>) -> Self {
        Self {
            browsers,
            browser: None,
            sites: Sites::default(),
            once: BTreeSet::new(),
        }
    }

    /// The sites the session's browser may reach.
    #[must_use]
    pub const fn sites(&self) -> &Sites {
        &self.sites
    }

    /// Whether the session's browser is running.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.browser.is_some()
    }

    /// `command` sent to the browser, its reply as the agent gets it. A
    /// browser that takes no more commands is closed.
    fn command(&mut self, command: &BrowserCommand) -> BrowserResult {
        let Some(browser) = self.browser.as_mut() else {
            return BrowserResult::Failed {
                failure: BrowserFailure::NoPage,
            };
        };
        match browser.command(command) {
            Ok(BrowserReply::Page { snapshot }) => {
                let refused = self.sites.refused();
                let snapshot = snapshot.fitted_in(|snapshot| {
                    hatchling_core::fits_one_frame(&SessionResponse::Browser(BrowserResult::Page {
                        snapshot: snapshot.clone(),
                        refused: refused.clone(),
                    }))
                });
                BrowserResult::Page { snapshot, refused }
            }
            Ok(BrowserReply::Failed { failure }) => BrowserResult::Failed { failure },
            Ok(BrowserReply::Closed) => {
                self.close();
                BrowserResult::Closed
            }
            Err(reason) => {
                tracing::warn!(%reason, "the browser takes no more commands: closed");
                self.close();
                BrowserResult::Failed {
                    failure: BrowserFailure::BrowserGone,
                }
            }
        }
    }

    /// Closes the browser, if it's running. The sites stay the session's.
    fn close(&mut self) {
        if let Some(browser) = self.browser.take() {
            browser.finish();
        }
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Answers `op` for the session's browser `browser`, under `config`, with
/// the sites its web session `web` holds, reading the held secrets with
/// `keys`.
pub fn handle_browser_op(
    config: &Config,
    keys: &dyn OpensStores,
    web: &mut WebSession,
    browser: &mut BrowserSession,
    op: &BrowserOp,
) -> BrowserResult {
    if browser.browsers.is_none() {
        return BrowserResult::Off;
    }
    match op {
        BrowserOp::Open { url, reason } => open(config, keys, web, browser, url, reason),
        BrowserOp::Read => browser.command(&BrowserCommand::Snapshot),
        BrowserOp::Act { element, action } => {
            if let PageAction::Type { text } = action {
                if let Some(refusal) = typed_secret(config, keys, text) {
                    return refusal;
                }
            }
            browser.command(&BrowserCommand::Act {
                element: *element,
                action: action.clone(),
            })
        }
        BrowserOp::Scroll { down } => browser.command(&BrowserCommand::Scroll { down: *down }),
        BrowserOp::Close => {
            browser.close();
            BrowserResult::Closed
        }
    }
}

/// Opens `url`, which the agent wants for `reason`, once its site is
/// allowed: the browser is started then, if it isn't running.
fn open(
    config: &Config,
    keys: &dyn OpensStores,
    web: &mut WebSession,
    browser: &mut BrowserSession,
    url: &str,
    reason: &str,
) -> BrowserResult {
    if reason.chars().count() > crate::web_ops::MAX_REASON_CHARS {
        return refused(&format!(
            "the reason is longer than {} characters",
            crate::web_ops::MAX_REASON_CHARS
        ));
    }
    let url = match WebUrl::parse(url) {
        Ok(url) => url,
        Err(refusal) => return refused(&refusal.to_string()),
    };
    if config.web.refuses(url.origin().host()) {
        return refused("the operator refuses this site");
    }
    let destination = url.origin().destination();
    // On the site of this visit, the browser stays: it was allowed once,
    // for the visit, and isn't asked for again until it's left.
    if !browser.once.contains(&destination) {
        if let Err(answer) = crate::web_ops::allow(config, keys, web, &url, reason) {
            return from_web(answer);
        }
    }
    browser.sites.grant(&destination);
    // A site held once is held for this visit: the browser is leaving the
    // others.
    let leaving: Vec<String> = browser
        .once
        .iter()
        .filter(|site| **site != destination)
        .cloned()
        .collect();
    for site in &leaving {
        let closed = browser.sites.revoke(site);
        browser.once.remove(site);
        tracing::debug!(%site, closed, "a site held once is left");
    }
    if !web.holds(url.origin()) {
        browser.once.insert(destination);
    }
    if browser.browser.is_none() {
        let Some(browsers) = browser.browsers.clone() else {
            return BrowserResult::Off;
        };
        let started = cell_id().and_then(|cell_id| browsers.start(&cell_id, browser.sites.clone()));
        match started {
            Ok(started) => browser.browser = Some(started),
            Err(reason) => {
                tracing::warn!(%reason, "the browser couldn't start");
                return refused("the browser couldn't start");
            }
        }
    }
    browser.command(&BrowserCommand::Open {
        url: url.to_string(),
    })
}

/// A fresh browser cell's id.
fn cell_id() -> Result<String, String> {
    let random = crate::approval::random_hex("")
        .map_err(|error| format!("cannot name a browser cell: {error}"))?;
    Ok(format!(
        "browser-{}",
        random.chars().take(24).collect::<String>()
    ))
}

/// What the agent is told when typing `text` is refused: it holds a secret
/// the broker keeps, or the secrets couldn't be read to check.
fn typed_secret(config: &Config, keys: &dyn OpensStores, text: &str) -> Option<BrowserResult> {
    match crate::held_secrets(config, keys) {
        Ok(secrets) if secrets.exposed_in(text.as_bytes()) => Some(refused(
            "the text holds a secret the broker keeps: it isn't typed",
        )),
        Ok(_) => None,
        Err(error) => {
            tracing::error!(%error, "the held secrets couldn't be read: typing refused");
            Some(refused(
                "the broker couldn't check the text for secrets: it isn't typed",
            ))
        }
    }
}

/// A site's approval, as the browser's agent is told it.
fn from_web(answer: WebResult) -> BrowserResult {
    match answer {
        WebResult::Waiting { site, request } => BrowserResult::Waiting { site, request },
        WebResult::Denied { site } => BrowserResult::Denied { site },
        WebResult::Expired { site } => BrowserResult::Expired { site },
        WebResult::Unasked { reason } => BrowserResult::Unasked { reason },
        WebResult::Refused { reason } => BrowserResult::Refused { reason },
        WebResult::Off => BrowserResult::Off,
        WebResult::Page { .. } | WebResult::Results { .. } | WebResult::Redirect { .. } => {
            refused("the site's approval answered with a page")
        }
    }
}

fn refused(reason: &str) -> BrowserResult {
    BrowserResult::Refused {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use hatchling_core::browser::PageSnapshot;
    use hatchling_core::ui::Decision;
    use hatchling_core::{ApprovalRefusal, VaultAction};
    use hatchling_vault::{MasterKey, SealedVault, Secret};

    use super::*;
    use crate::web_ops::tests::{owner_decides, web_config};

    /// What the fake browsers saw: each start's cell id, every command,
    /// and how many were finished.
    #[derive(Debug, Default)]
    struct Log {
        started: Vec<String>,
        commands: Vec<BrowserCommand>,
        finished: usize,
    }

    /// Browsers that answer every command with a page, or break once
    /// `broken` is set.
    #[derive(Clone, Default)]
    struct Fake {
        log: Arc<Mutex<Log>>,
        broken: Arc<std::sync::atomic::AtomicBool>,
        refuse_start: bool,
    }

    impl Browsers for Fake {
        fn start(&self, cell_id: &str, _sites: Sites) -> Result<Box<dyn Browser>, String> {
            if self.refuse_start {
                return Err(String::from("no browser here"));
            }
            self.log.lock().unwrap().started.push(cell_id.to_owned());
            Ok(Box::new(self.clone()))
        }
    }

    impl Browser for Fake {
        fn command(&mut self, command: &BrowserCommand) -> Result<BrowserReply, String> {
            self.log.lock().unwrap().commands.push(command.clone());
            if self.broken.load(Ordering::SeqCst) {
                return Err(String::from("broken"));
            }
            Ok(match command {
                BrowserCommand::Open { url } => BrowserReply::Page {
                    snapshot: PageSnapshot {
                        url: url.clone(),
                        ..PageSnapshot::default()
                    },
                },
                BrowserCommand::Close => BrowserReply::Closed,
                BrowserCommand::Snapshot
                | BrowserCommand::Act { .. }
                | BrowserCommand::Scroll { .. } => BrowserReply::Page {
                    snapshot: PageSnapshot::default(),
                },
            })
        }

        fn finish(self: Box<Self>) {
            self.log.lock().unwrap().finished += 1;
        }
    }

    /// A session of the task `task` that never waits for the owner, and a
    /// browser started by `fake`.
    fn session(task: &str, fake: &Fake) -> (WebSession, BrowserSession) {
        let fetcher: crate::web_ops::Fetcher =
            Arc::new(|_| Err(hatchling_egress::web::FetchError::Failed));
        (
            WebSession::with(task, Duration::ZERO, Duration::from_secs(5), fetcher),
            BrowserSession::new(Some(Arc::new(fake.clone()))),
        )
    }

    fn open(url: &str) -> BrowserOp {
        BrowserOp::Open {
            url: url.to_owned(),
            reason: String::from("to check the order's status"),
        }
    }

    /// `op` answered, on its own thread so a wait that never ends fails
    /// the test: the sessions come back with the answer.
    fn ask(
        config: &Config,
        sessions: (WebSession, BrowserSession),
        op: &BrowserOp,
    ) -> ((WebSession, BrowserSession), BrowserResult) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let (config, op) = (config.clone(), op.clone());
        std::thread::spawn(move || {
            let (mut web, mut browser) = sessions;
            let keys = MasterKey::generate().unwrap();
            let answer = handle_browser_op(&config, &keys, &mut web, &mut browser, &op);
            let _ = sender.send(((web, browser), answer));
        });
        receiver.recv_timeout(Duration::from_secs(20)).unwrap()
    }

    fn page(url: &str) -> BrowserResult {
        BrowserResult::Page {
            snapshot: PageSnapshot {
                url: url.to_owned(),
                ..PageSnapshot::default()
            },
            refused: Vec::new(),
        }
    }

    #[test]
    fn the_browser_is_off_unless_the_operator_sets_one_up() {
        let config = web_config("browser-off");
        let keys = MasterKey::generate().unwrap();
        let fetcher: crate::web_ops::Fetcher =
            Arc::new(|_| Err(hatchling_egress::web::FetchError::Failed));
        let mut web = WebSession::with("t", Duration::ZERO, Duration::from_secs(5), fetcher);
        let mut browser = BrowserSession::new(None);
        for op in [
            open("https://a.example/"),
            BrowserOp::Read,
            BrowserOp::Scroll { down: true },
            BrowserOp::Close,
        ] {
            assert_eq!(
                handle_browser_op(&config, &keys, &mut web, &mut browser, &op),
                BrowserResult::Off
            );
        }
        assert!(!browser.is_open());
    }

    #[test]
    fn a_page_opens_only_once_its_site_is_allowed() {
        let config = web_config("browser-allow");
        let fake = Fake::default();
        let (sessions, answer) = ask(&config, session("t", &fake), &open("https://a.example/x"));
        let asked =
            matches!(&answer, BrowserResult::Waiting { site, .. } if site == "https://a.example");
        assert!(asked, "{answer:?}");
        assert!(
            fake.log.lock().unwrap().started.is_empty(),
            "no browser yet"
        );
        assert!(!sessions.1.sites().holds("a.example:443"));
        owner_decides(&config, "t", "https://a.example", Decision::Approve);
        let (sessions, answer) = ask(&config, sessions, &open("https://a.example/x"));
        assert_eq!(answer, page("https://a.example/x"));
        assert!(sessions.1.sites().holds("a.example:443"));
        assert!(sessions.1.is_open());
        let log = fake.log.lock().unwrap();
        assert_eq!(log.started.len(), 1);
        assert!(
            log.started
                .iter()
                .all(|id| id.parse::<hatchling_core::CellId>().is_ok())
        );
        assert_eq!(
            log.commands,
            [BrowserCommand::Open {
                url: String::from("https://a.example/x")
            }]
        );
        drop(log);
    }

    #[test]
    fn once_is_one_visit_and_the_task_keeps_its_site() {
        let config = web_config("browser-once");
        let fake = Fake::default();
        let (sessions, _) = ask(&config, session("t", &fake), &open("https://a.example/"));
        owner_decides(&config, "t", "https://a.example", Decision::Approve);
        let (sessions, answer) = ask(&config, sessions, &open("https://a.example/"));
        assert_eq!(answer, page("https://a.example/"));
        // A second page on the same site: still this visit, not asked again.
        let (sessions, answer) = ask(&config, sessions, &open("https://a.example/next"));
        assert_eq!(answer, page("https://a.example/next"));
        assert!(
            !crate::web_ops::tests::board_has_pending(&config, "t", "https://a.example"),
            "the visit's next page wasn't put to the owner"
        );
        // Another site, for the task: the first is left, and revoked.
        let (sessions, _) = ask(&config, sessions, &open("https://b.example/"));
        owner_decides(&config, "t", "https://b.example", Decision::ApproveSession);
        let (sessions, answer) = ask(&config, sessions, &open("https://b.example/"));
        assert_eq!(answer, page("https://b.example/"));
        assert!(!sessions.1.sites().holds("a.example:443"), "left: revoked");
        assert!(sessions.1.sites().holds("b.example:443"));
        // Back to the first: it's asked for again. The task's site stays.
        let (sessions, answer) = ask(&config, sessions, &open("https://a.example/"));
        let asked_again = matches!(answer, BrowserResult::Waiting { .. });
        assert!(asked_again, "{answer:?}");
        owner_decides(&config, "t", "https://a.example", Decision::Approve);
        let (sessions, answer) = ask(&config, sessions, &open("https://a.example/"));
        assert_eq!(answer, page("https://a.example/"));
        assert!(
            sessions.1.sites().holds("b.example:443"),
            "held for the task, past a visit elsewhere"
        );
        assert_eq!(fake.log.lock().unwrap().started.len(), 1, "one browser");
    }

    #[test]
    fn refusals_come_before_asking_or_starting() {
        let mut config = web_config("browser-refusals");
        config.web.refused = vec![String::from("blocked.example")];
        let fake = Fake::default();
        let mut sessions = session("t", &fake);
        for (op, reason) in [
            (
                open("http://a.example/"),
                "only absolute https URLs are read",
            ),
            (
                open("https://blocked.example/"),
                "the operator refuses this site",
            ),
            (
                BrowserOp::Open {
                    url: String::from("https://a.example/"),
                    reason: "x".repeat(crate::web_ops::MAX_REASON_CHARS + 1),
                },
                "the reason is longer than 500 characters",
            ),
        ] {
            let answer;
            (sessions, answer) = ask(&config, sessions, &op);
            assert_eq!(
                answer,
                BrowserResult::Refused {
                    reason: reason.to_owned()
                }
            );
        }
        let log = fake.log.lock().unwrap();
        assert!(log.started.is_empty() && log.commands.is_empty());
        drop(log);
    }

    #[test]
    fn a_page_before_a_browser_is_no_page_and_a_browser_that_wont_start_is_refused() {
        let config = web_config("browser-nostart");
        let fake = Fake {
            refuse_start: true,
            ..Fake::default()
        };
        let sessions = session("t", &fake);
        let (sessions, answer) = ask(&config, sessions, &BrowserOp::Read);
        assert_eq!(
            answer,
            BrowserResult::Failed {
                failure: BrowserFailure::NoPage
            }
        );
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        owner_decides(&config, "t", "https://a.example", Decision::ApproveSession);
        let (sessions, answer) = ask(&config, sessions, &open("https://a.example/"));
        assert_eq!(answer, refused("the browser couldn't start"));
        assert!(!sessions.1.is_open());
    }

    #[test]
    fn a_broken_browser_is_closed_and_the_next_page_starts_another() {
        let config = web_config("browser-broken");
        let fake = Fake::default();
        let (sessions, _) = ask(&config, session("t", &fake), &open("https://a.example/"));
        owner_decides(&config, "t", "https://a.example", Decision::ApproveSession);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        fake.broken.store(true, Ordering::SeqCst);
        let (sessions, answer) = ask(&config, sessions, &BrowserOp::Scroll { down: true });
        assert_eq!(
            answer,
            BrowserResult::Failed {
                failure: BrowserFailure::BrowserGone
            }
        );
        assert!(!sessions.1.is_open());
        assert_eq!(fake.log.lock().unwrap().finished, 1);
        fake.broken.store(false, Ordering::SeqCst);
        let (sessions, answer) = ask(&config, sessions, &open("https://a.example/"));
        assert_eq!(answer, page("https://a.example/"));
        assert_eq!(fake.log.lock().unwrap().started.len(), 2);
        // Closed by the agent, then by the session's end: once each.
        let (sessions, answer) = ask(&config, sessions, &BrowserOp::Close);
        assert_eq!(answer, BrowserResult::Closed);
        assert_eq!(fake.log.lock().unwrap().finished, 2);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        drop(sessions);
        assert_eq!(
            fake.log.lock().unwrap().finished,
            3,
            "ended with the session"
        );
    }

    #[test]
    fn the_controller_closing_is_the_browser_closed() {
        let config = web_config("browser-closed");
        let fake = Fake::default();
        let (sessions, _) = ask(&config, session("t", &fake), &open("https://a.example/"));
        owner_decides(&config, "t", "https://a.example", Decision::ApproveSession);
        let (mut sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        assert_eq!(
            sessions.1.command(&BrowserCommand::Close),
            BrowserResult::Closed
        );
        assert!(!sessions.1.is_open());
        assert_eq!(fake.log.lock().unwrap().finished, 1);
    }

    #[test]
    fn typed_text_holding_a_secret_is_refused_and_never_typed() {
        let root =
            std::env::temp_dir().join(format!("hatchling-browser-secret-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // The vault is the owner's alone, or it isn't opened.
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
            .create(&root)
            .unwrap();
        let mut config = web_config("browser-secret");
        config.vault.root = root.clone();
        config.vault.grants = vec![crate::VaultGrant {
            action: VaultAction::Fetch,
            name: "token".parse().unwrap(),
        }];
        let keys = MasterKey::generate().unwrap();
        SealedVault::open(&root, &keys)
            .unwrap()
            .store(
                &"token".parse().unwrap(),
                &Secret::new(b"hunter2-sesame".to_vec()).unwrap(),
            )
            .unwrap();
        let fake = Fake::default();
        let (mut web, mut browser) = session("t", &fake);
        let typing = |text: &str| BrowserOp::Act {
            element: 3,
            action: PageAction::Type {
                text: text.to_owned(),
            },
        };
        assert_eq!(
            handle_browser_op(
                &config,
                &keys,
                &mut web,
                &mut browser,
                &typing("my code is hunter2-sesame")
            ),
            refused("the text holds a secret the broker keeps: it isn't typed")
        );
        // Other text is typed, or would be: there's no page yet.
        assert_eq!(
            handle_browser_op(&config, &keys, &mut web, &mut browser, &typing("hello")),
            BrowserResult::Failed {
                failure: BrowserFailure::NoPage
            }
        );
        // Secrets that can't be read: nothing is typed.
        let wrong = MasterKey::generate().unwrap();
        assert_eq!(
            handle_browser_op(&config, &wrong, &mut web, &mut browser, &typing("hello")),
            refused("the broker couldn't check the text for secrets: it isn't typed")
        );
        assert!(fake.log.lock().unwrap().commands.is_empty());
        // The check itself: a secret anywhere in the text, or none.
        assert_eq!(
            typed_secret(&config, &keys, "hunter2-sesame"),
            Some(refused(
                "the text holds a secret the broker keeps: it isn't typed"
            ))
        );
        assert_eq!(typed_secret(&config, &keys, "hunter2"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A browser whose every page is `0`.
    struct Big(PageSnapshot);

    impl Browser for Big {
        fn command(&mut self, _: &BrowserCommand) -> Result<BrowserReply, String> {
            Ok(BrowserReply::Page {
                snapshot: self.0.clone(),
            })
        }

        fn finish(self: Box<Self>) {}
    }

    #[test]
    fn a_page_lists_the_sites_refused_and_fits_one_frame() {
        let fake = Fake::default();
        let (_, mut browser) = session("t", &fake);
        assert!(!browser.sites().admit("cdn.example:443"));
        let big = PageSnapshot {
            url: String::from("https://a.example/"),
            text: "\u{1F600}".repeat(60_000),
            ..PageSnapshot::default()
        };
        browser.browser = Some(Box::new(Big(big)));
        let answer = browser.command(&BrowserCommand::Snapshot);
        let BrowserResult::Page { snapshot, refused } = &answer else {
            unreachable!("{answer:?}");
        };
        assert_eq!(refused, &[String::from("cdn.example:443")]);
        assert!(snapshot.truncated);
        assert!(hatchling_core::fits_one_frame(&SessionResponse::Browser(
            answer.clone()
        )));
    }

    #[test]
    fn at_most_max_browsers_run_and_a_place_comes_back_when_one_ends() {
        let fake = Fake::default();
        let capped = Capped::new(Arc::new(fake.clone()), 2);
        let first = capped.start("browser-a", Sites::default()).unwrap();
        let second = capped.start("browser-b", Sites::default()).unwrap();
        let refused = capped.start("browser-c", Sites::default()).err().unwrap();
        assert_eq!(refused, "2 browsers are running already");
        first.finish();
        assert_eq!(fake.log.lock().unwrap().finished, 1);
        let third = capped.start("browser-c", Sites::default()).unwrap();
        drop((second, third));
        // A browser that doesn't start gives its place back.
        let failing = Capped::new(
            Arc::new(Fake {
                refuse_start: true,
                ..Fake::default()
            }),
            1,
        );
        assert!(failing.start("browser-d", Sites::default()).is_err());
        assert!(failing.start("browser-d", Sites::default()).is_err());
        assert_eq!(failing.live.load(Ordering::SeqCst), 0);
        assert_eq!(capped.live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_counted_browser_passes_commands_and_closes_once() {
        let fake = Fake::default();
        let capped = Capped::new(Arc::new(fake.clone()), 1);
        let mut browser = capped.start("browser-e", Sites::default()).unwrap();
        assert_eq!(
            browser.command(&BrowserCommand::Close).unwrap(),
            BrowserReply::Closed
        );
        browser.finish();
        assert_eq!(fake.log.lock().unwrap().finished, 1);
        let mut empty = Counted {
            browser: None,
            _place: Place(Arc::new(AtomicUsize::new(1))),
        };
        assert_eq!(
            empty.command(&BrowserCommand::Snapshot).unwrap_err(),
            "closed"
        );
    }

    #[test]
    fn approvals_reach_the_agent_as_the_browsers_answers() {
        let site = || String::from("https://a.example");
        for (web, browser) in [
            (
                WebResult::Waiting {
                    site: site(),
                    request: String::from("r"),
                },
                BrowserResult::Waiting {
                    site: site(),
                    request: String::from("r"),
                },
            ),
            (
                WebResult::Denied { site: site() },
                BrowserResult::Denied { site: site() },
            ),
            (
                WebResult::Expired { site: site() },
                BrowserResult::Expired { site: site() },
            ),
            (
                WebResult::Unasked {
                    reason: ApprovalRefusal::QueueFull,
                },
                BrowserResult::Unasked {
                    reason: ApprovalRefusal::QueueFull,
                },
            ),
            (refused_web("no"), refused("no")),
            (WebResult::Off, BrowserResult::Off),
            (
                WebResult::Redirect {
                    to: String::from("https://b.example/"),
                },
                refused("the site's approval answered with a page"),
            ),
        ] {
            assert_eq!(from_web(web), browser);
        }
    }

    fn refused_web(reason: &str) -> WebResult {
        WebResult::Refused {
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn a_browser_needs_its_resource_group_and_none_is_nothing() {
        let none = configure(None, None, PathBuf::from("/x"), CellIds::Own).unwrap();
        assert!(!none);
        let settings = BrowserSettings::parse(|key| match key {
            "HATCHLING_BROWSER" => Some(String::from("/opt/chrome/chrome")),
            "HATCHLING_BROWSER_SHA256" => Some("ab".repeat(32)),
            "HATCHLING_BROWSER_CONTROLLER" => Some(String::from("/opt/hatchling-browser")),
            _ => None,
        })
        .unwrap();
        let refused = configure(settings, None, PathBuf::from("/x"), CellIds::Own).unwrap_err();
        assert!(
            refused.to_string().contains("HATCHLING_CELL_CGROUP"),
            "{refused}"
        );
        assert!(configured().is_none(), "nothing was set up");
    }

    #[test]
    fn the_brokers_own_settings_name_no_browser_here() {
        // The tests' environment names no browser for the broker itself
        // (theirs is `HATCHLING_TEST_BROWSER`): nothing is set up.
        assert!(std::env::var_os("HATCHLING_BROWSER").is_none());
        assert!(!configure_from_env(PathBuf::from("/x")).unwrap());
        assert!(configured().is_none());
    }

    #[test]
    fn a_browser_cell_is_named_fresh_and_validly() {
        let (a, b) = (cell_id().unwrap(), cell_id().unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with("browser-") && a.len() == 32, "{a}");
        assert!(a.parse::<hatchling_core::CellId>().is_ok());
    }
}
