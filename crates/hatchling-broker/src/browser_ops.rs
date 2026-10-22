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
//!   refused, and listed with the page; the agent may ask the owner for
//!   one of those ([`BrowserOp::Allow`]), as for a page's site.
//! - **Once is one visit.** A site allowed once, a page's or one a page
//!   needed, is held while the browser stays on the site it's visiting,
//!   and revoked, its tunnels closed, when the browser opens another.
//! - **Typed text is checked:** text that holds a secret the broker keeps
//!   is refused before it reaches the page.
//! - **The owner watches, and may stop it** ([`crate::browser_board`]):
//!   each op is shown in the app, the session, the action in the owner's
//!   words (never the text typed) and a frame of the page after it. A
//!   browser the owner stops stays stopped for the session.
//! - **The owner may take it over, and hand it back.** While they drive,
//!   or control changes hands, the agent's ops do nothing
//!   ([`BrowserResult::OwnerDriving`]). Handed back, the agent's next op
//!   looks at the page afresh instead, and is answered with it
//!   ([`BrowserResult::HandedBack`]): the page may have changed under it.
//! - **Bounded:** one browser per session, at most [`MAX_BROWSERS`] in
//!   all, each for at most its lifetime; it ends with the session.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use hatchling_core::browser::{
    BrowserCommand, BrowserFailure, BrowserReply, PageAction, PageSnapshot,
};
use hatchling_core::ui::BrowserState;
use hatchling_core::{BrowserOp, BrowserResult, SessionResponse, WebResult};
use hatchling_egress::web::WebUrl;
use hatchling_vault::OpensStores;

use crate::browser_board::{BrowserBoard, Turn};
use crate::browser_cell::{BrowserCell, BrowserLauncher};
use crate::browser_proxy::{Route, Sites};
use crate::browser_session::BrowserSettings;
use crate::cell::cgroup_parent;
use crate::web_ops::WebSession;
use crate::{CellIds, Config, Error};

/// Most browsers running at once, across every session.
pub const MAX_BROWSERS: usize = 4;

/// The most characters of what the owner is told of an action, or of what
/// a browser is for.
pub const MAX_NARRATION_CHARS: usize = 240;

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
    /// The site the browser is visiting, as `host:port`.
    visit: Option<String>,
    /// The sites held for this visit: its own, if it was allowed once, and
    /// any a page of it needed that was allowed once.
    once: BTreeSet<String>,
    /// The owner's view of the session's browsers.
    board: Option<Arc<BrowserBoard>>,
    /// The browser as the owner sees it, while one runs.
    shown: Option<Shown>,
    /// The owner stopped the session's browser: it stays stopped.
    stopped: bool,
}

/// A browser as the owner sees it.
#[derive(Debug)]
struct Shown {
    /// Its session in the app: its cell's id.
    id: String,
    /// What it's for: the agent's reason when it opened its first page.
    title: String,
    /// The last frame shown.
    frame: u64,
    /// The last page's elements, by id: their role and name.
    elements: BTreeMap<u32, (String, String)>,
    /// Set when the owner stops it.
    stopped: Arc<AtomicBool>,
}

impl std::fmt::Debug for BrowserSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSession")
            .field("open", &self.browser.is_some())
            .field("visit", &self.visit)
            .field("once", &self.once)
            .field("shown", &self.shown)
            .field("stopped", &self.stopped)
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
            visit: None,
            once: BTreeSet::new(),
            board: None,
            shown: None,
            stopped: false,
        }
    }

    /// This session, shown to the owner on `board`, who may stop it.
    #[must_use]
    pub fn with_board(mut self, board: Arc<BrowserBoard>) -> Self {
        self.board = Some(board);
        self
    }

    /// What the agent's next op may do: the owner may be driving.
    fn turn(&self) -> Turn {
        match (self.board.as_ref(), self.shown.as_ref()) {
            (Some(board), Some(shown)) => board.turn(&shown.id),
            _ => Turn::Act,
        }
    }

    /// The agent's op is done.
    fn done(&self) {
        if let (Some(board), Some(shown)) = (self.board.as_ref(), self.shown.as_ref()) {
            board.done(&shown.id);
        }
    }

    /// The owner handed the browser back: the agent looks at the page
    /// afresh, takes a frame of it, and drives again. The page as it is
    /// now.
    fn resume(&mut self) -> BrowserResult {
        let looked = self.command(&BrowserCommand::Snapshot);
        let result = if let BrowserResult::Page { snapshot, refused } = looked {
            self.remember(&snapshot);
            if let (Some(board), Some(shown)) = (self.board.clone(), self.shown.as_ref()) {
                board.act(
                    &shown.id,
                    String::from("Looked at the page again, after the owner handed it back"),
                );
                if board.watching() {
                    self.frame(&board, &snapshot.title);
                }
            }
            BrowserResult::HandedBack { snapshot, refused }
        } else {
            looked
        };
        if let (Some(board), Some(shown)) = (self.board.as_ref(), self.shown.as_ref()) {
            board.resumed(&shown.id);
        }
        result
    }

    /// Whether the owner has stopped the session's browser. The first time
    /// it's seen, the browser is closed, as cancelled.
    fn owner_stopped(&mut self) -> bool {
        if !self.stopped
            && self
                .shown
                .as_ref()
                .is_some_and(|shown| shown.stopped.load(Ordering::SeqCst))
        {
            self.stopped = true;
            self.end(BrowserState::Cancelled);
        }
        self.stopped
    }

    /// A browser started in the cell `id`, for `reason`: shown to the
    /// owner, who may stop it from now on.
    fn shown_as(&mut self, id: String, reason: &str) {
        let stopped = self.board.as_ref().map_or_else(
            || Arc::new(AtomicBool::new(false)),
            |board| board.begin(&id, self.sites.clone()),
        );
        self.shown = Some(Shown {
            id,
            title: reason.chars().take(MAX_NARRATION_CHARS).collect(),
            frame: 0,
            elements: BTreeMap::new(),
            stopped,
        });
    }

    /// What the owner is told of `op`, before it's done: in their words,
    /// never the text typed.
    fn narrate(&self, op: &BrowserOp) -> String {
        let named = |element: u32| {
            self.shown
                .as_ref()
                .and_then(|shown| shown.elements.get(&element))
                .map_or_else(
                    || format!("element {element}"),
                    |(role, name)| format!("{role} \"{name}\""),
                )
        };
        let said = match op {
            BrowserOp::Open { url, .. } => format!("Opened {url}"),
            BrowserOp::Read => String::from("Read the page"),
            BrowserOp::Act { element, action } => match action {
                PageAction::Click => format!("Clicked {}", named(*element)),
                PageAction::Type { text } => format!(
                    "Typed {} characters into {}",
                    text.chars().count(),
                    named(*element)
                ),
                PageAction::Key { key } => format!("Pressed {key:?} in {}", named(*element)),
                PageAction::Choose { option } => {
                    format!("Chose \"{option}\" in {}", named(*element))
                }
            },
            BrowserOp::Scroll { down: true } => String::from("Scrolled down"),
            BrowserOp::Scroll { down: false } => String::from("Scrolled up"),
            BrowserOp::Allow { site, .. } => format!("Asked the owner to allow {site}"),
            BrowserOp::Close => String::from("Closed the browser"),
        };
        said.chars().take(MAX_NARRATION_CHARS).collect()
    }

    /// Shows the owner what `narration` did, from `result`: the session,
    /// the action, and a frame of the page when they're watching.
    fn report(&mut self, narration: String, result: &BrowserResult) {
        let (Some(board), Some(shown)) = (self.board.clone(), self.shown.as_ref()) else {
            return;
        };
        let id = shown.id.clone();
        let (step, confirmed) = match result {
            BrowserResult::Page { snapshot, .. } => (narration.clone(), snapshot.title.clone()),
            BrowserResult::Failed { .. } => (format!("{narration}: it didn't work"), String::new()),
            BrowserResult::Allowed { .. } => (narration.clone(), String::new()),
            BrowserResult::Waiting { .. }
            | BrowserResult::Denied { .. }
            | BrowserResult::Expired { .. }
            | BrowserResult::Unasked { .. }
            | BrowserResult::Refused { .. }
            | BrowserResult::Closed
            | BrowserResult::Stopped
            | BrowserResult::OwnerDriving
            | BrowserResult::HandedBack { .. }
            | BrowserResult::Off => return,
        };
        board.act(&id, narration);
        self.show(&board, BrowserState::Running, step, confirmed);
        let BrowserResult::Page { snapshot, .. } = result else {
            return;
        };
        self.remember(snapshot);
        if board.watching() {
            self.frame(&board, &snapshot.title);
        }
    }

    /// Keeps the elements of `snapshot`, to name them to the owner.
    fn remember(&mut self, snapshot: &PageSnapshot) {
        if let Some(shown) = self.shown.as_mut() {
            shown.elements = snapshot
                .elements
                .iter()
                .map(|element| (element.id, (element.role.clone(), element.name.clone())))
                .collect();
        }
    }

    /// Shows the session as it is now: `state`, at `step`, having last
    /// confirmed `confirmed`.
    fn show(&self, board: &BrowserBoard, state: BrowserState, step: String, confirmed: String) {
        let Some(shown) = self.shown.as_ref() else {
            return;
        };
        let site = self
            .visit
            .as_deref()
            .map(|visit| visit.strip_suffix(":443").unwrap_or(visit))
            .unwrap_or_default()
            .to_owned();
        // Every browser shows in the owner's one chat, whatever run
        // started it: there they open it, watch it, stop it, or take it
        // over.
        board.show(hatchling_core::ui::BrowserSession {
            session_id: shown.id.clone(),
            chat_id: Some(String::from(crate::api_ops::LOCAL_CHAT)),
            title: shown.title.clone(),
            site,
            state,
            step,
            last_confirmed: confirmed,
        });
    }

    /// Takes a frame of the page, titled `title`, and shows it.
    fn frame(&mut self, board: &BrowserBoard, title: &str) {
        let Some(browser) = self.browser.as_mut() else {
            return;
        };
        let Ok(BrowserReply::Frame { jpeg }) = browser.command(&BrowserCommand::Frame) else {
            return;
        };
        if let Some(shown) = self.shown.as_mut() {
            shown.frame += 1;
            board.frame(&shown.id, shown.frame, title, &jpeg);
        }
    }

    /// The browser, if one runs, closed and shown to the owner as `state`;
    /// the board forgets it.
    fn end(&mut self, state: BrowserState) {
        if let Some(browser) = self.browser.take() {
            browser.finish();
        }
        if let (Some(board), Some(shown)) = (self.board.clone(), self.shown.as_ref()) {
            let step = match state {
                BrowserState::Cancelled => String::from("Stopped by the owner"),
                BrowserState::Failed => String::from("The browser stopped working"),
                BrowserState::Queued
                | BrowserState::Running
                | BrowserState::Parked
                | BrowserState::Completed => String::from("Closed"),
            };
            self.show(&board, state, step, String::new());
            board.end(&shown.id);
        }
        self.shown = None;
    }

    /// Leaves the visit: every site held only for it is revoked, and its
    /// tunnels closed.
    fn leave(&mut self) {
        for site in std::mem::take(&mut self.once) {
            let closed = self.sites.revoke(&site);
            tracing::debug!(%site, closed, "a site held once is left");
        }
        self.visit = None;
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
                self.end(BrowserState::Completed);
                BrowserResult::Closed
            }
            // A picture for an agent's command: the controller answered
            // out of turn, and isn't trusted with another.
            Ok(BrowserReply::Frame { .. }) => {
                tracing::warn!("the browser answered out of turn: closed");
                self.end(BrowserState::Failed);
                BrowserResult::Failed {
                    failure: BrowserFailure::BrowserGone,
                }
            }
            Err(reason) => {
                tracing::warn!(%reason, "the browser takes no more commands: closed");
                self.end(BrowserState::Failed);
                BrowserResult::Failed {
                    failure: BrowserFailure::BrowserGone,
                }
            }
        }
    }

    /// Closes the browser, if it's running. The sites stay the session's.
    fn close(&mut self) {
        self.end(BrowserState::Completed);
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
    if browser.owner_stopped() {
        return BrowserResult::Stopped;
    }
    match browser.turn() {
        Turn::Wait => return BrowserResult::OwnerDriving,
        Turn::Resume => {
            let result = browser.resume();
            browser.done();
            return result;
        }
        Turn::Act => {}
    }
    let narration = browser.narrate(op);
    let result = answer(config, keys, web, browser, op);
    browser.report(narration, &result);
    browser.done();
    result
}

/// `op`, done for the session's browser `browser`.
fn answer(
    config: &Config,
    keys: &dyn OpensStores,
    web: &mut WebSession,
    browser: &mut BrowserSession,
    op: &BrowserOp,
) -> BrowserResult {
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
        BrowserOp::Allow { site, reason } => allow_site(config, keys, web, browser, site, reason),
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
    let same_visit = browser.visit.as_deref() == Some(destination.as_str());
    // On the site it's visiting, the browser stays: a site allowed once,
    // for the visit, isn't asked for again until it's left.
    if !(same_visit && browser.once.contains(&destination)) {
        if let Err(answer) = crate::web_ops::allow(config, keys, web, &url, reason) {
            return from_web(answer);
        }
    }
    // Allowed: only now is the visit left, if this is another site.
    if !same_visit {
        browser.leave();
    }
    browser.sites.grant(&destination);
    if !web.holds(url.origin()) {
        browser.once.insert(destination.clone());
    }
    browser.visit = Some(destination);
    if browser.browser.is_none() {
        let Some(browsers) = browser.browsers.clone() else {
            return BrowserResult::Off;
        };
        let started = cell_id().and_then(|cell_id| {
            browsers
                .start(&cell_id, browser.sites.clone())
                .map(|started| (cell_id, started))
        });
        match started {
            Ok((cell_id, started)) => {
                browser.browser = Some(started);
                browser.shown_as(cell_id, reason);
            }
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

/// Asks the owner, for `reason`, to allow `site`, one the browser was
/// refused for a page of this visit: allowed once, it's held for the
/// visit.
fn allow_site(
    config: &Config,
    keys: &dyn OpensStores,
    web: &mut WebSession,
    browser: &mut BrowserSession,
    site: &str,
    reason: &str,
) -> BrowserResult {
    if reason.chars().count() > crate::web_ops::MAX_REASON_CHARS {
        return refused(&format!(
            "the reason is longer than {} characters",
            crate::web_ops::MAX_REASON_CHARS
        ));
    }
    // Only what a page asked for: anything else is a page to open.
    let host = browser
        .sites
        .refused()
        .iter()
        .any(|refused| refused == site)
        .then(|| site.strip_suffix(":443"))
        .flatten();
    let Some(url) = host.and_then(|host| WebUrl::parse(&format!("https://{host}/")).ok()) else {
        return refused(
            "only a site the browser was refused can be asked for: open a page with browser_open",
        );
    };
    if config.web.refuses(url.origin().host()) {
        return refused("the operator refuses this site");
    }
    if let Err(answer) = crate::web_ops::allow(config, keys, web, &url, reason) {
        return from_web(answer);
    }
    browser.sites.grant(site);
    if !web.holds(url.origin()) {
        browser.once.insert(site.to_owned());
    }
    BrowserResult::Allowed {
        site: site.to_owned(),
    }
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
        broken: Arc<AtomicBool>,
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
                BrowserCommand::Frame | BrowserCommand::Input { .. } => BrowserReply::Frame {
                    jpeg: String::from("/9j/"),
                },
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

    fn allow(site: &str) -> BrowserOp {
        BrowserOp::Allow {
            site: site.to_owned(),
            reason: String::from("the page's fonts come from there"),
        }
    }

    /// A session of the task `t` whose browser is on `https://a.example/`,
    /// held for the task, and was refused `cdn.example:443` for it.
    fn on_a_page(config: &Config, fake: &Fake) -> (WebSession, BrowserSession) {
        let (sessions, _) = ask(config, session("t", fake), &open("https://a.example/"));
        owner_decides(config, "t", "https://a.example", Decision::ApproveSession);
        let (sessions, answer) = ask(config, sessions, &open("https://a.example/"));
        assert_eq!(answer, page("https://a.example/"));
        assert!(!sessions.1.sites().admit("cdn.example:443"));
        sessions
    }

    #[test]
    fn a_site_a_page_was_refused_is_put_to_the_owner_and_held_for_the_visit() {
        let config = web_config("browser-allow-site");
        let fake = Fake::default();
        let sessions = on_a_page(&config, &fake);
        let (sessions, answer) = ask(&config, sessions, &allow("cdn.example:443"));
        let asked =
            matches!(&answer, BrowserResult::Waiting { site, .. } if site == "https://cdn.example");
        assert!(asked, "{answer:?}");
        assert!(
            !sessions.1.sites().holds("cdn.example:443"),
            "not before the owner says"
        );
        owner_decides(&config, "t", "https://cdn.example", Decision::Approve);
        let (sessions, answer) = ask(&config, sessions, &allow("cdn.example:443"));
        assert_eq!(
            answer,
            BrowserResult::Allowed {
                site: String::from("cdn.example:443")
            }
        );
        assert!(sessions.1.sites().holds("cdn.example:443"));
        assert!(sessions.1.sites().refused().is_empty(), "no longer refused");
        // The page again, on the same visit: the site stays.
        let (sessions, answer) = ask(&config, sessions, &open("https://a.example/"));
        assert_eq!(answer, page("https://a.example/"));
        assert!(
            sessions.1.sites().holds("cdn.example:443"),
            "held for the visit"
        );
        // Another site: the visit is left, and what was held once for it.
        let (sessions, _) = ask(&config, sessions, &open("https://b.example/"));
        owner_decides(&config, "t", "https://b.example", Decision::ApproveSession);
        let (sessions, answer) = ask(&config, sessions, &open("https://b.example/"));
        assert_eq!(answer, page("https://b.example/"));
        assert!(
            !sessions.1.sites().holds("cdn.example:443"),
            "left with the visit"
        );
        assert!(
            sessions.1.sites().holds("a.example:443"),
            "held for the task"
        );
        // A page still waiting on the owner leaves nothing.
        assert!(!sessions.1.sites().admit("fonts.example:443"));
        let (sessions, _) = ask(&config, sessions, &allow("fonts.example:443"));
        owner_decides(&config, "t", "https://fonts.example", Decision::Approve);
        let (sessions, _) = ask(&config, sessions, &allow("fonts.example:443"));
        let (sessions, answer) = ask(&config, sessions, &open("https://c.example/"));
        let waiting = matches!(answer, BrowserResult::Waiting { .. });
        assert!(waiting, "{answer:?}");
        assert!(
            sessions.1.sites().holds("fonts.example:443"),
            "still on b's visit"
        );
    }

    #[test]
    fn a_site_held_for_the_task_outlives_the_visit() {
        let config = web_config("browser-allow-task");
        let fake = Fake::default();
        let sessions = on_a_page(&config, &fake);
        let (sessions, _) = ask(&config, sessions, &allow("cdn.example:443"));
        owner_decides(
            &config,
            "t",
            "https://cdn.example",
            Decision::ApproveSession,
        );
        let (sessions, answer) = ask(&config, sessions, &allow("cdn.example:443"));
        assert!(
            matches!(answer, BrowserResult::Allowed { .. }),
            "{answer:?}"
        );
        let (sessions, _) = ask(&config, sessions, &open("https://b.example/"));
        owner_decides(&config, "t", "https://b.example", Decision::Approve);
        let (sessions, _) = ask(&config, sessions, &open("https://b.example/"));
        assert!(
            sessions.1.sites().holds("cdn.example:443"),
            "held for the task"
        );
    }

    #[test]
    fn only_a_site_the_browser_was_refused_can_be_asked_for() {
        let mut config = web_config("browser-allow-refused");
        config.web.refused = vec![String::from("blocked.example")];
        let fake = Fake::default();
        let sessions = on_a_page(&config, &fake);
        assert!(!sessions.1.sites().admit("blocked.example:443"));
        let only = refused(
            "only a site the browser was refused can be asked for: open a page with browser_open",
        );
        let mut sessions = sessions;
        for (op, expected) in [
            (allow("elsewhere.example:443"), only.clone()),
            (allow("cdn.example"), only),
            (
                allow("blocked.example:443"),
                refused("the operator refuses this site"),
            ),
            (
                BrowserOp::Allow {
                    site: String::from("cdn.example:443"),
                    reason: "x".repeat(crate::web_ops::MAX_REASON_CHARS + 1),
                },
                refused("the reason is longer than 500 characters"),
            ),
        ] {
            let answer;
            (sessions, answer) = ask(&config, sessions, &op);
            assert_eq!(answer, expected, "{op:?}");
        }
        assert!(!crate::web_ops::tests::board_has_pending(
            &config,
            "t",
            "https://elsewhere.example"
        ));
        assert!(!sessions.1.sites().holds("cdn.example:443"));
    }

    #[test]
    fn leaving_a_visit_revokes_only_what_was_held_for_it() {
        let fake = Fake::default();
        let (_, mut browser) = session("t", &fake);
        for site in ["a.example:443", "cdn.example:443", "b.example:443"] {
            browser.sites.grant(site);
        }
        browser.visit = Some(String::from("a.example:443"));
        browser.once = ["a.example:443", "cdn.example:443"]
            .into_iter()
            .map(String::from)
            .collect();
        browser.leave();
        assert!(!browser.sites.holds("a.example:443"));
        assert!(!browser.sites.holds("cdn.example:443"));
        assert!(browser.sites.holds("b.example:443"), "held for the task");
        assert!(browser.visit.is_none() && browser.once.is_empty());
    }

    #[test]
    fn allowing_a_site_no_page_asked_for_is_refused_unasked() {
        let config = web_config("browser-allow-direct");
        let keys = MasterKey::generate().unwrap();
        let fake = Fake::default();
        let (mut web, mut browser) = session("t", &fake);
        assert_eq!(
            allow_site(
                &config,
                &keys,
                &mut web,
                &mut browser,
                "cdn.example:443",
                "fonts"
            ),
            refused(
                "only a site the browser was refused can be asked for: open a page with browser_open"
            )
        );
        assert!(!crate::web_ops::tests::board_has_pending(
            &config,
            "t",
            "https://cdn.example"
        ));
    }

    /// A session of the task `t`, its browser started by `fake`, shown on
    /// a board an app watches through `bus`.
    fn watched(
        fake: &Fake,
    ) -> (
        (WebSession, BrowserSession),
        Arc<BrowserBoard>,
        Arc<crate::api_stream::Bus>,
    ) {
        let board = Arc::new(BrowserBoard::default());
        let bus = Arc::new(crate::api_stream::Bus::starting_now());
        board.attach(Arc::clone(&bus));
        let (web, browser) = session("t", fake);
        ((web, browser.with_board(Arc::clone(&board))), board, bus)
    }

    /// What `bus` carried, from the event after `after` on.
    fn carried_after(
        bus: &crate::api_stream::Bus,
        after: usize,
    ) -> Vec<hatchling_core::ui::UiEventBody> {
        bus.wait_after(None, Duration::ZERO)
            .into_iter()
            .skip(after)
            .map(|event| event.body)
            .collect()
    }

    /// The browser `sessions` shows the owner: its session in the app.
    fn shown_id(sessions: &(WebSession, BrowserSession)) -> String {
        sessions
            .1
            .shown
            .as_ref()
            .map(|shown| shown.id.clone())
            .unwrap_or_default()
    }

    #[test]
    fn the_owner_sees_each_op_and_a_frame_of_its_page() {
        use hatchling_core::ui::{Actor, BrowserAction, UiEventBody};
        let config = web_config("browser-watched");
        let fake = Fake::default();
        let (sessions, board, bus) = watched(&fake);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/x"));
        assert!(
            carried_after(&bus, 0).is_empty(),
            "nothing before the browser"
        );
        owner_decides(&config, "t", "https://a.example", Decision::ApproveSession);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/x"));
        let id = shown_id(&sessions);
        assert!(id.starts_with("browser-"), "{id}");
        let shown = carried_after(&bus, 0);
        assert_eq!(
            shown.first(),
            Some(&UiEventBody::BrowserAction(BrowserAction {
                session_id: id.clone(),
                by: Actor::Agent,
                narration: String::from("Opened https://a.example/x"),
            }))
        );
        assert_eq!(
            shown.get(1),
            Some(&UiEventBody::BrowserSession(
                hatchling_core::ui::BrowserSession {
                    session_id: id.clone(),
                    chat_id: Some(String::from("local")),
                    title: String::from("to check the order's status"),
                    site: String::from("a.example"),
                    state: BrowserState::Running,
                    step: String::from("Opened https://a.example/x"),
                    last_confirmed: String::new(),
                }
            ))
        );
        let Some(UiEventBody::BrowserFrame(frame)) = shown.get(2) else {
            unreachable!("{shown:?}");
        };
        assert_eq!(
            (frame.session_id.as_str(), frame.sequence),
            (id.as_str(), 1)
        );
        assert_eq!(shown.len(), 3);
        // Each op after: its own frame, numbered on.
        let (sessions, _) = ask(&config, sessions, &BrowserOp::Scroll { down: true });
        let next = carried_after(&bus, 3);
        let Some(UiEventBody::BrowserFrame(frame)) = next.get(2) else {
            unreachable!("{next:?}");
        };
        assert_eq!(frame.sequence, 2);
        // Closed: shown completed, and nothing left to stop.
        let (sessions, _) = ask(&config, sessions, &BrowserOp::Close);
        let closed = carried_after(&bus, 6);
        let finished = matches!(
            closed.last(),
            Some(UiEventBody::BrowserSession(session)) if session.state == BrowserState::Completed
        );
        assert!(finished, "{closed:?}");
        assert_eq!(
            board.stop(&id),
            Err(hatchling_core::ui::ControlRefusal::UnknownSession)
        );
        drop(sessions);
    }

    #[test]
    fn a_stop_is_seen_once_and_kept() {
        let fake = Fake::default();
        let (_, mut browser) = session("t", &fake);
        assert!(!browser.owner_stopped(), "nothing to stop");
        browser.browser = Some(Box::new(fake.clone()));
        browser.shown_as(String::from("browser-s"), "why");
        assert!(!browser.owner_stopped(), "not stopped");
        if let Some(shown) = browser.shown.as_ref() {
            shown.stopped.store(true, Ordering::SeqCst);
        }
        assert!(browser.owner_stopped());
        assert!(!browser.is_open(), "closed when it's seen");
        assert!(browser.shown.is_none());
        assert!(browser.owner_stopped(), "and kept");
        assert_eq!(fake.log.lock().unwrap().finished, 1, "closed once");
    }

    #[test]
    fn handed_back_the_agent_looks_afresh_and_a_broken_look_is_its_failure() {
        let fake = Fake::default();
        let (_, mut browser) = session("t", &fake);
        browser.browser = Some(Box::new(fake.clone()));
        assert_eq!(
            browser.resume(),
            BrowserResult::HandedBack {
                snapshot: PageSnapshot::default(),
                refused: Vec::new(),
            }
        );
        assert_eq!(
            fake.log.lock().unwrap().commands,
            [BrowserCommand::Snapshot],
            "a look, and no frame with nobody watching"
        );
        fake.broken.store(true, Ordering::SeqCst);
        assert_eq!(
            browser.resume(),
            BrowserResult::Failed {
                failure: BrowserFailure::BrowserGone,
            }
        );
    }

    #[test]
    fn the_agent_waits_while_the_owner_drives_and_looks_afresh_when_handed_back() {
        use hatchling_core::ui::{Handoff, UiEventBody};
        let config = web_config("browser-control");
        let fake = Fake::default();
        let (sessions, board, bus) = watched(&fake);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        owner_decides(&config, "t", "https://a.example", Decision::ApproveSession);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        let id = shown_id(&sessions);
        assert_eq!(board.take(&id), Ok(()));
        let commands_before = fake.log.lock().unwrap().commands.len();
        let (sessions, answer) = ask(&config, sessions, &BrowserOp::Scroll { down: true });
        assert_eq!(answer, BrowserResult::OwnerDriving);
        assert_eq!(
            fake.log.lock().unwrap().commands.len(),
            commands_before,
            "nothing reached the browser"
        );
        // Handed back from the latest frame: the next op looks afresh.
        assert_eq!(board.hand_back(&id, 1), Ok(()));
        let (sessions, answer) = ask(&config, sessions, &BrowserOp::Scroll { down: true });
        let looked = matches!(answer, BrowserResult::HandedBack { .. });
        assert!(looked, "{answer:?}");
        let sent: Vec<BrowserCommand> = fake
            .log
            .lock()
            .unwrap()
            .commands
            .iter()
            .skip(commands_before)
            .cloned()
            .collect();
        assert_eq!(
            sent,
            [BrowserCommand::Snapshot, BrowserCommand::Frame],
            "a fresh look and a frame, not the scroll"
        );
        let handoffs: Vec<(Handoff, u64)> = carried_after(&bus, 0)
            .into_iter()
            .filter_map(|body| {
                if let UiEventBody::BrowserControl {
                    handoff,
                    frame_sequence,
                    ..
                } = body
                {
                    Some((handoff, frame_sequence))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            handoffs,
            [
                (Handoff::Takeover, 1),
                (Handoff::Granted, 1),
                (Handoff::HandBack, 1),
                (Handoff::Resumed, 2),
            ]
        );
        // The agent drives again.
        let (sessions, answer) = ask(&config, sessions, &BrowserOp::Scroll { down: true });
        assert_eq!(answer, page(""));
        drop(sessions);
    }

    #[test]
    fn the_owner_is_told_each_action_in_their_words_never_the_text_typed() {
        let fake = Fake::default();
        let (_, mut browser) = session("t", &fake);
        browser.shown_as(String::from("browser-n"), "why");
        if let Some(shown) = browser.shown.as_mut() {
            shown
                .elements
                .insert(3, (String::from("textbox"), String::from("Search")));
        }
        let act = |action: PageAction| BrowserOp::Act { element: 3, action };
        for (op, said) in [
            (open("https://a.example/"), "Opened https://a.example/"),
            (BrowserOp::Read, "Read the page"),
            (act(PageAction::Click), "Clicked textbox \"Search\""),
            (
                act(PageAction::Type {
                    text: String::from("hunter2"),
                }),
                "Typed 7 characters into textbox \"Search\"",
            ),
            (
                act(PageAction::Key {
                    key: hatchling_core::browser::Key::Enter,
                }),
                "Pressed Enter in textbox \"Search\"",
            ),
            (
                act(PageAction::Choose {
                    option: String::from("Blue"),
                }),
                "Chose \"Blue\" in textbox \"Search\"",
            ),
            (
                BrowserOp::Act {
                    element: 9,
                    action: PageAction::Click,
                },
                "Clicked element 9",
            ),
            (BrowserOp::Scroll { down: true }, "Scrolled down"),
            (BrowserOp::Scroll { down: false }, "Scrolled up"),
            (
                allow("cdn.example:443"),
                "Asked the owner to allow cdn.example:443",
            ),
            (BrowserOp::Close, "Closed the browser"),
        ] {
            assert_eq!(browser.narrate(&op), said);
        }
        let long = open(&format!("https://a.example/{}", "x".repeat(400)));
        assert_eq!(browser.narrate(&long).chars().count(), MAX_NARRATION_CHARS);
    }

    #[test]
    fn a_browser_the_owner_stops_stays_stopped() {
        use hatchling_core::ui::UiEventBody;
        let config = web_config("browser-stopped");
        let fake = Fake::default();
        let (sessions, board, bus) = watched(&fake);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        owner_decides(&config, "t", "https://a.example", Decision::ApproveSession);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        let id = shown_id(&sessions);
        assert_eq!(board.stop(&id), Ok(()));
        assert!(!sessions.1.sites().holds("a.example:443"), "sealed at once");
        let (sessions, answer) = ask(&config, sessions, &BrowserOp::Read);
        assert_eq!(answer, BrowserResult::Stopped);
        assert!(!sessions.1.is_open());
        assert_eq!(fake.log.lock().unwrap().finished, 1);
        let cancelled = carried_after(&bus, 0).into_iter().any(|body| {
            matches!(body, UiEventBody::BrowserSession(session) if session.state == BrowserState::Cancelled)
        });
        assert!(cancelled, "shown as stopped");
        // It stays stopped: no browser is started again.
        let (sessions, answer) = ask(&config, sessions, &open("https://a.example/"));
        assert_eq!(answer, BrowserResult::Stopped);
        assert_eq!(fake.log.lock().unwrap().started.len(), 1);
        drop(sessions);
    }

    #[test]
    fn no_frame_is_taken_when_no_app_watches_and_a_broken_browser_is_shown_failed() {
        use hatchling_core::ui::UiEventBody;
        let config = web_config("browser-unwatched");
        let fake = Fake::default();
        let board = Arc::new(BrowserBoard::default());
        let (web, browser) = session("t", &fake);
        let sessions = (web, browser.with_board(Arc::clone(&board)));
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        owner_decides(&config, "t", "https://a.example", Decision::ApproveSession);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        let frames = fake
            .log
            .lock()
            .unwrap()
            .commands
            .iter()
            .filter(|command| **command == BrowserCommand::Frame)
            .count();
        assert_eq!(frames, 0, "nobody watching: no frame");
        drop(sessions);

        let (sessions, _, bus) = watched(&fake);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        owner_decides(&config, "t", "https://a.example", Decision::ApproveSession);
        let (sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        fake.broken.store(true, Ordering::SeqCst);
        let (sessions, _) = ask(&config, sessions, &BrowserOp::Read);
        let failed = carried_after(&bus, 0).into_iter().any(|body| {
            matches!(body, UiEventBody::BrowserSession(session) if session.state == BrowserState::Failed)
        });
        assert!(failed, "shown as failed");
        drop(sessions);
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
    fn a_picture_where_a_page_was_due_closes_the_browser() {
        let config = web_config("browser-out-of-turn");
        let fake = Fake::default();
        let (sessions, _) = ask(&config, session("t", &fake), &open("https://a.example/"));
        owner_decides(&config, "t", "https://a.example", Decision::ApproveSession);
        let (mut sessions, _) = ask(&config, sessions, &open("https://a.example/"));
        assert_eq!(
            sessions.1.command(&BrowserCommand::Frame),
            BrowserResult::Failed {
                failure: BrowserFailure::BrowserGone
            }
        );
        assert!(!sessions.1.is_open());
        assert_eq!(fake.log.lock().unwrap().finished, 1);
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
