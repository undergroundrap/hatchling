//! The owner's view of the agent's browser (WO-029, part 4): what the
//! broker tells the owner's app about each browser, and the owner's stop.
//!
//! A browser cell is one session in the app's browser view (WO-021), by its
//! cell id. As the agent drives it, the broker publishes on the app's
//! stream: the session (its site, where it stands, the step it's on), each
//! action in the owner's words, for the action log, and a frame of the
//! page after each one, a picture checked to be a JPEG. Everything that
//! came from the page is untrusted text, and the app shows it as such.
//! Nothing is published that the stream couldn't carry in one frame: an
//! event too big would end the app's stream, and stay to end it again.
//!
//! The owner can stop a browser, whoever drives it. Its sites are sealed
//! at once ([`Sites::seal`]): every one revoked, every tunnel closed, and
//! none granted again, so it reaches nothing more even while the agent's
//! op is in flight. The agent's next op closes it, and the agent is told
//! the owner stopped it.
//!
//! The owner can take control, and hand it back. Control moves only as
//! [`Control::after`] allows, the table the app applies too, and each move
//! is published as its handoff:
//!
//! - **Taking it**, only from the agent: `Takeover`, then `Granted` once
//!   no op of the agent's is in flight, at once or when the op ends. The
//!   agent's ops are refused while the owner drives or control changes
//!   hands.
//! - **Handing it back**, only from the latest frame the owner saw:
//!   `HandBack`, then `Resumed` once the agent has looked at the page
//!   afresh, with the frame it took. Its next op isn't done: it's told the
//!   page changed under it, and given the page as it is now.
//!
//! While the owner drives, their own input reaches the browser: a click at
//! a point of the page, text typed or a key pressed where its focus is, or
//! a scroll. Each is shown in the action log in their words, never the
//! text typed, and a frame of the page after it follows. The board and the
//! agent's session share the browser ([`Driven`]); only whoever has
//! control sends it anything. Frames are numbered here, for both.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hatchling_core::browser::{
    BrowserCommand, BrowserFailure, BrowserReply, MAX_FRAME_JPEG_BASE64, MAX_TYPED_CHARS,
    OwnerInput, VIEWPORT,
};
use hatchling_core::ui::{
    Actor, BrowserAction, BrowserFrame, BrowserSession, Control, ControlRefusal, Handoff,
};
use hatchling_core::ui::{ImageType, InlineImage, UiEvent, UiEventBody, UiResponse};
use hatchling_scheduler::Timestamp;

use crate::Config;
use crate::api_stream::Bus;
use crate::browser_ops::Driven;
use crate::browser_proxy::Sites;

/// The boards, by the vault root of the broker each belongs to.
static BOARDS: Mutex<BTreeMap<String, Arc<BrowserBoard>>> = Mutex::new(BTreeMap::new());

/// The browser board of the broker `config` configures.
#[must_use]
pub fn board(config: &Config) -> Arc<BrowserBoard> {
    Arc::clone(
        BOARDS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(config.vault.root.display().to_string())
            .or_insert_with(|| Arc::new(BrowserBoard::default())),
    )
}

/// The browsers the owner can see and stop, and the stream they're shown
/// on.
#[derive(Debug, Default)]
pub struct BrowserBoard {
    bus: Mutex<Option<Arc<Bus>>>,
    sessions: Mutex<BTreeMap<String, Watched>>,
}

/// A browser the owner can stop, and drive.
struct Watched {
    sites: Sites,
    stopped: Arc<AtomicBool>,
    /// Who drives it.
    control: Control,
    /// The latest frame shown of it.
    latest: u64,
    /// Whether an op of the agent's is in flight.
    acting: bool,
    /// The browser, which the owner drives once control is theirs.
    browser: Driven,
    /// The page's title, as the latest frame showed it.
    title: String,
    /// The site it's visiting, `host:port`.
    visit: Option<String>,
    /// The signed-in profile it runs (WO-030), if one.
    profile: Option<String>,
}

impl std::fmt::Debug for Watched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watched")
            .field("sites", &self.sites)
            .field("stopped", &self.stopped)
            .field("control", &self.control)
            .field("latest", &self.latest)
            .field("acting", &self.acting)
            .field("title", &self.title)
            .finish_non_exhaustive()
    }
}

/// What the owner does to a browser from the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerAct<'a> {
    /// Take control of it.
    Take(&'a str),
    /// Hand it back, from the frame seen.
    Return(&'a str, u64),
    /// Stop it.
    Stop(&'a str),
    /// Give it their own input.
    Input(&'a str, &'a OwnerInput),
    /// Keep its sign-in (WO-030).
    Keep(&'a str),
}

/// Why the owner's input wasn't done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRefusal {
    /// Control isn't the owner's, or the browser isn't there.
    Control(ControlRefusal),
    /// The browser didn't do it.
    Browser(BrowserFailure),
}

/// What the agent's next op may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Turn {
    /// Act: the agent drives.
    Act,
    /// Nothing: the owner drives, or control is passing to them.
    Wait,
    /// Look at the page afresh first: the owner has handed it back.
    Resume,
}

impl BrowserBoard {
    /// Shows the browsers to the owner's app on `bus`.
    pub fn attach(&self, bus: Arc<Bus>) {
        *self.bus.lock().unwrap_or_else(PoisonError::into_inner) = Some(bus);
    }

    /// Whether an app is attached to watch: frames are taken only then.
    #[must_use]
    pub fn watching(&self) -> bool {
        self.bus
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// The browser `session_id`, `browser`, begins, reaching `sites`: the
    /// owner may stop it, and drive it. What its session reads to learn it
    /// was stopped.
    pub(crate) fn begin(&self, session_id: &str, sites: Sites, browser: Driven) -> Arc<AtomicBool> {
        let stopped = Arc::new(AtomicBool::new(false));
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                session_id.to_owned(),
                Watched {
                    sites,
                    stopped: Arc::clone(&stopped),
                    control: Control::Agent,
                    latest: 0,
                    acting: false,
                    browser,
                    title: String::new(),
                    visit: None,
                    profile: None,
                },
            );
        stopped
    }

    /// The browser `session_id` is visiting `site`, `host:port`.
    pub(crate) fn visiting(&self, session_id: &str, site: &str) {
        if let Some(watched) = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(session_id)
        {
            watched.visit = Some(site.to_owned());
        }
    }

    /// The browser `session_id` runs the signed-in profile `profile_id`
    /// (WO-030): its session is kept when the owner hands it back.
    pub(crate) fn profiled(&self, session_id: &str, profile_id: &str) {
        if let Some(watched) = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(session_id)
        {
            watched.profile = Some(profile_id.to_owned());
        }
    }

    /// The signed-in profile the browser `session_id` runs, and the
    /// browser, to read its session from: `None` unless it runs one.
    #[must_use]
    pub(crate) fn profile_of(&self, session_id: &str) -> Option<(String, Driven)> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let held = sessions
            .get(session_id)
            .and_then(|watched| Some((watched.profile.clone()?, Arc::clone(&watched.browser))));
        drop(sessions);
        held
    }

    /// What the owner keeps when they keep the sign-in of the browser
    /// `session_id` (WO-030): its sites, the one it's visiting first, and
    /// the browser, to read its session from. Only while they drive it:
    /// they signed in on it themselves.
    ///
    /// # Errors
    ///
    /// As for [`BrowserBoard::hand_back`]: control isn't the owner's, or
    /// the browser isn't there; and [`ControlRefusal::NotAllowed`] for a
    /// browser that has visited no site.
    pub(crate) fn sign_in_of(
        &self,
        session_id: &str,
    ) -> Result<(Vec<String>, Driven), ControlRefusal> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let watched = sessions
            .get(session_id)
            .ok_or(ControlRefusal::UnknownSession)?;
        Self::open_to_control(watched, Control::User)?;
        let visit = watched.visit.clone().ok_or(ControlRefusal::NotAllowed)?;
        let mut sites = vec![visit];
        for site in watched.sites.granted() {
            if !sites.contains(&site) {
                sites.push(site);
            }
        }
        let browser = Arc::clone(&watched.browser);
        drop(sessions);
        Ok((sites, browser))
    }

    /// The browser `session_id` has ended: there's nothing to stop.
    pub(crate) fn end(&self, session_id: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session_id);
    }

    /// The owner stops the browser `session_id`: its sites sealed now, and
    /// the browser closed at the agent's next op.
    ///
    /// # Errors
    ///
    /// [`ControlRefusal::UnknownSession`] for a browser there isn't, or
    /// that has ended; [`ControlRefusal::Finished`] for one already
    /// stopped.
    pub fn stop(&self, session_id: &str) -> Result<(), ControlRefusal> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let watched = sessions
            .get(session_id)
            .ok_or(ControlRefusal::UnknownSession)?;
        if watched.stopped.swap(true, Ordering::SeqCst) {
            return Err(ControlRefusal::Finished);
        }
        let closed = watched.sites.seal();
        drop(sessions);
        tracing::info!(session = session_id, closed, "the owner stopped a browser");
        Ok(())
    }

    /// The owner takes control of the browser `session_id`, from the
    /// agent: `Takeover`, then `Granted` at once if no op of the agent's is
    /// in flight, or when it ends.
    ///
    /// # Errors
    ///
    /// [`ControlRefusal::UnknownSession`] for a browser there isn't;
    /// [`ControlRefusal::Finished`] for one stopped;
    /// [`ControlRefusal::Switching`] while control changes hands;
    /// [`ControlRefusal::NotAllowed`] when the owner already drives.
    pub fn take(&self, session_id: &str) -> Result<(), ControlRefusal> {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let watched = sessions
            .get_mut(session_id)
            .ok_or(ControlRefusal::UnknownSession)?;
        Self::open_to_control(watched, Control::Agent)?;
        self.hand(session_id, watched, Handoff::Takeover);
        if !watched.acting {
            self.hand(session_id, watched, Handoff::Granted);
        }
        drop(sessions);
        Ok(())
    }

    /// The owner hands the browser `session_id` back, from frame `seen`,
    /// which must be the latest shown: `HandBack`, and `Resumed` when the
    /// agent has looked afresh.
    ///
    /// # Errors
    ///
    /// As [`BrowserBoard::take`], with [`ControlRefusal::NotAllowed`] when
    /// the agent drives, and [`ControlRefusal::StaleFrame`] when `seen`
    /// isn't the latest frame.
    pub fn hand_back(&self, session_id: &str, seen: u64) -> Result<(), ControlRefusal> {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let watched = sessions
            .get_mut(session_id)
            .ok_or(ControlRefusal::UnknownSession)?;
        Self::open_to_control(watched, Control::User)?;
        if seen != watched.latest {
            return Err(ControlRefusal::StaleFrame);
        }
        self.hand(session_id, watched, Handoff::HandBack);
        drop(sessions);
        Ok(())
    }

    /// Whether `watched` may be handed over from `from`, where it must be.
    fn open_to_control(watched: &Watched, from: Control) -> Result<(), ControlRefusal> {
        if watched.stopped.load(Ordering::SeqCst) {
            return Err(ControlRefusal::Finished);
        }
        match watched.control {
            Control::ToUser | Control::ToAgent => Err(ControlRefusal::Switching),
            current if current == from => Ok(()),
            Control::Agent | Control::User => Err(ControlRefusal::NotAllowed),
        }
    }

    /// Moves `watched` by `handoff`, if the table allows it, and tells the
    /// app, at the latest frame.
    fn hand(&self, session_id: &str, watched: &mut Watched, handoff: Handoff) {
        if let Some(next) = watched.control.after(handoff) {
            watched.control = next;
            self.publish(UiEventBody::BrowserControl {
                session_id: session_id.to_owned(),
                handoff,
                frame_sequence: watched.latest,
            });
        }
    }

    /// What the agent's next op on `session_id` may do. One that may act
    /// or resume is in flight until [`BrowserBoard::done`].
    pub(crate) fn turn(&self, session_id: &str) -> Turn {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(watched) = sessions.get_mut(session_id) else {
            return Turn::Act;
        };
        let turn = match watched.control {
            Control::Agent => Turn::Act,
            Control::ToAgent => Turn::Resume,
            Control::User | Control::ToUser => Turn::Wait,
        };
        watched.acting = turn != Turn::Wait;
        drop(sessions);
        turn
    }

    /// The agent's op on `session_id` is done: a takeover asked for while
    /// it was in flight is granted now.
    pub(crate) fn done(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(watched) = sessions.get_mut(session_id) {
            watched.acting = false;
            if watched.control == Control::ToUser {
                self.hand(session_id, watched, Handoff::Granted);
            }
        }
    }

    /// The agent has looked at `session_id`'s page afresh, after the owner
    /// handed it back: it drives again, from the latest frame.
    pub(crate) fn resumed(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(watched) = sessions.get_mut(session_id) {
            self.hand(session_id, watched, Handoff::Resumed);
        }
    }

    /// Shows `session` as it is now.
    pub(crate) fn show(&self, session: BrowserSession) {
        self.publish(UiEventBody::BrowserSession(session));
    }

    /// Logs the agent's action on `session_id`, in the owner's words.
    pub(crate) fn act(&self, session_id: &str, narration: String) {
        self.told(session_id, Actor::Agent, narration);
    }

    /// Logs `by`'s action on `session_id`, `narration`.
    fn told(&self, session_id: &str, by: Actor, narration: String) {
        self.publish(UiEventBody::BrowserAction(BrowserAction {
            session_id: session_id.to_owned(),
            by,
            narration,
        }));
    }

    /// Shows the next frame of `session_id`'s page, titled `title`, from
    /// `jpeg`, which the cell sent: only if it's a JPEG within the bound,
    /// and of a browser on the board. Whether it was shown.
    pub(crate) fn frame(&self, session_id: &str, title: &str, jpeg: &str) -> bool {
        let Some(image) = jpeg_image(jpeg) else {
            tracing::warn!(session = session_id, "a frame that isn't a JPEG: not shown");
            return false;
        };
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(watched) = sessions.get_mut(session_id) else {
            return false;
        };
        let sequence = watched.latest.saturating_add(1);
        let shown = self.publish(UiEventBody::BrowserFrame(BrowserFrame {
            session_id: session_id.to_owned(),
            sequence,
            captured_at: Timestamp::now().to_string(),
            title: title.to_owned(),
            image: Some(image),
        }));
        if shown {
            watched.latest = sequence;
            title.clone_into(&mut watched.title);
        }
        drop(sessions);
        shown
    }

    /// The owner's `input` to the browser `session_id`, which they drive:
    /// done, shown in the action log in their words (never the text typed),
    /// and a frame of the page after it shown.
    ///
    /// # Errors
    ///
    /// [`InputRefusal::Control`] when control isn't the owner's, as for
    /// [`BrowserBoard::hand_back`]; [`InputRefusal::Browser`] when the
    /// browser didn't do it. One that answers out of turn, or not at all,
    /// is closed: it isn't trusted with another command.
    pub fn input(&self, session_id: &str, input: &OwnerInput) -> Result<(), InputRefusal> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let watched = sessions
            .get(session_id)
            .ok_or(InputRefusal::Control(ControlRefusal::UnknownSession))?;
        Self::open_to_control(watched, Control::User).map_err(InputRefusal::Control)?;
        let browser = Arc::clone(&watched.browser);
        drop(sessions);
        if !within_bounds(input) {
            return Err(InputRefusal::Browser(BrowserFailure::NotPossible));
        }
        let command = BrowserCommand::Input {
            input: input.clone(),
        };
        let mut slot = browser.lock().unwrap_or_else(PoisonError::into_inner);
        let jpeg = match slot.as_mut().map(|browser| browser.command(&command)) {
            Some(Ok(BrowserReply::Frame { jpeg })) => jpeg,
            Some(Ok(BrowserReply::Failed { failure })) => {
                return Err(InputRefusal::Browser(failure));
            }
            None => return Err(InputRefusal::Browser(BrowserFailure::BrowserGone)),
            Some(
                Ok(
                    BrowserReply::Page { .. }
                    | BrowserReply::Closed
                    | BrowserReply::Cookies { .. }
                    | BrowserReply::CookiesSet,
                )
                | Err(_),
            ) => {
                tracing::warn!(
                    session = session_id,
                    "the browser took no more of the owner's input: closed"
                );
                if let Some(broken) = slot.take() {
                    broken.finish();
                }
                return Err(InputRefusal::Browser(BrowserFailure::BrowserGone));
            }
        };
        drop(slot);
        self.told(session_id, Actor::User, owner_said(input));
        let title = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session_id)
            .map(|watched| watched.title.clone())
            .unwrap_or_default();
        self.frame(session_id, &title, &jpeg);
        Ok(())
    }

    /// Pushes `body` to the owner's app, if one is attached and the stream
    /// can carry it: as the stream sends it, numbered as high as a number
    /// goes, it fits one frame. Whether it was pushed.
    fn publish(&self, body: UiEventBody) -> bool {
        let bus = self
            .bus
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(bus) = bus else {
            return false;
        };
        let ts = Timestamp::now().to_string();
        let sent = UiResponse::Event(UiEvent {
            seq: u64::MAX,
            ts: ts.clone(),
            body,
        });
        if !hatchling_core::fits_one_frame(&sent) {
            tracing::warn!("a browser event too big for the app's stream: not shown");
            return false;
        }
        let UiResponse::Event(event) = sent else {
            return false;
        };
        bus.push(ts, event.body);
        true
    }
}

/// Whether `input` is within what the browser takes: a point inside the
/// viewport, and text no longer than the agent itself may type.
fn within_bounds(input: &OwnerInput) -> bool {
    match input {
        OwnerInput::Click { x, y } => {
            let (width, height) = VIEWPORT;
            *x < width && *y < height
        }
        OwnerInput::Type { text } => text.chars().count() <= MAX_TYPED_CHARS,
        OwnerInput::Key { .. } | OwnerInput::Scroll { .. } => true,
    }
}

/// The owner's `input`, in words for the action log: never the text typed.
fn owner_said(input: &OwnerInput) -> String {
    match input {
        OwnerInput::Click { x, y } => format!("Clicked at {x}, {y}"),
        OwnerInput::Type { text } => format!("Typed {} characters", text.chars().count()),
        OwnerInput::Key { key } => format!("Pressed {key:?}"),
        OwnerInput::Scroll { down: true } => String::from("Scrolled down"),
        OwnerInput::Scroll { down: false } => String::from("Scrolled up"),
    }
}

/// `base64` as an inline JPEG, if it's at most [`MAX_FRAME_JPEG_BASE64`]
/// and standard base64 of a JPEG (it begins `FF D8 FF`).
fn jpeg_image(base64: &str) -> Option<InlineImage> {
    use base64::Engine as _;
    if base64.len() > MAX_FRAME_JPEG_BASE64 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64)
        .ok()?;
    bytes.starts_with(&[0xFF, 0xD8, 0xFF]).then(|| InlineImage {
        media_type: ImageType::Jpeg,
        base64: base64.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use base64::Engine as _;
    use hatchling_core::ui::BrowserState;

    use super::*;

    fn encoded(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// A board watched through a bus of its own.
    fn watched() -> (BrowserBoard, Arc<Bus>) {
        let board = BrowserBoard::default();
        let bus = Arc::new(Bus::starting_now());
        board.attach(Arc::clone(&bus));
        (board, bus)
    }

    /// What `bus` has carried.
    fn carried(bus: &Bus) -> Vec<UiEventBody> {
        bus.wait_after(None, Duration::ZERO)
            .into_iter()
            .map(|event| event.body)
            .collect()
    }

    #[test]
    fn a_frame_is_shown_only_if_its_a_jpeg_within_the_bound() {
        let jpeg = encoded(&[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]);
        assert_eq!(
            jpeg_image(&jpeg),
            Some(InlineImage {
                media_type: ImageType::Jpeg,
                base64: jpeg.clone()
            })
        );
        for refused in [
            encoded(b"<svg onload=alert(1)>"),
            encoded(&[0x89, b'P', b'N', b'G']),
            String::from("/9j/ not base64 \"'><"),
            encoded(&[&[0xFF_u8, 0xD8, 0xFF][..], &vec![0; MAX_FRAME_JPEG_BASE64]].concat()),
        ] {
            assert_eq!(
                jpeg_image(&refused),
                None,
                "{}",
                refused.chars().take(40).collect::<String>()
            );
        }
        let (board, bus) = watched();
        assert!(!board.frame("browser-a", "A", &jpeg), "not on the board");
        let _stopped = board.begin("browser-a", Sites::default(), Driven::default());
        assert!(board.frame("browser-a", "A", &jpeg));
        assert!(!board.frame("browser-a", "A", &encoded(b"GIF89a")));
        let shown = carried(&bus);
        assert_eq!(shown.len(), 1, "only the JPEG");
        let Some(UiEventBody::BrowserFrame(frame)) = shown.first() else {
            unreachable!("{shown:?}");
        };
        assert_eq!(
            (
                frame.session_id.as_str(),
                frame.sequence,
                frame.title.as_str()
            ),
            ("browser-a", 1, "A")
        );
    }

    #[test]
    fn sessions_and_actions_reach_the_app_once_one_watches() {
        let unwatched = BrowserBoard::default();
        assert!(!unwatched.watching());
        unwatched.act("browser-a", String::from("Opened a page"));
        let (board, bus) = watched();
        assert!(board.watching());
        let session = BrowserSession {
            session_id: String::from("browser-a"),
            chat_id: None,
            title: String::from("check the order"),
            site: String::from("a.example"),
            state: BrowserState::Running,
            step: String::from("Opened https://a.example/"),
            last_confirmed: String::from("A"),
        };
        board.show(session.clone());
        board.act("browser-a", String::from("Clicked button \"Go\""));
        board.told("browser-a", Actor::User, String::from("Scrolled down"));
        assert_eq!(
            carried(&bus),
            [
                UiEventBody::BrowserSession(session),
                UiEventBody::BrowserAction(BrowserAction {
                    session_id: String::from("browser-a"),
                    by: Actor::Agent,
                    narration: String::from("Clicked button \"Go\""),
                }),
                UiEventBody::BrowserAction(BrowserAction {
                    session_id: String::from("browser-a"),
                    by: Actor::User,
                    narration: String::from("Scrolled down"),
                }),
            ]
        );
    }

    #[test]
    fn nothing_the_stream_couldnt_carry_is_published() {
        let (board, bus) = watched();
        // A page's title, a site and a step are the page's: one could be
        // made as long as a frame.
        let session = |title: String| BrowserSession {
            session_id: String::from("browser-a"),
            chat_id: None,
            title,
            site: String::from("a.example"),
            state: BrowserState::Running,
            step: String::new(),
            last_confirmed: String::new(),
        };
        board.show(session(
            "x".repeat(hatchling_core::framing::MAX_FRAME_BYTES),
        ));
        assert!(carried(&bus).is_empty(), "too big: not shown");
        board.show(session(String::from("fits")));
        assert_eq!(carried(&bus).len(), 1);
        // A frame at its bound, as the stream sends it, still fits.
        let mut at_bound = vec![0xFF_u8, 0xD8, 0xFF];
        at_bound.resize(MAX_FRAME_JPEG_BASE64 / 4 * 3, 0);
        let jpeg = encoded(&at_bound);
        assert!(jpeg.len() <= MAX_FRAME_JPEG_BASE64);
        let _stopped = board.begin("browser-a", Sites::default(), Driven::default());
        assert!(board.frame("browser-a", &"t".repeat(120), &jpeg));
        assert_eq!(carried(&bus).len(), 2, "a frame at its bound is shown");
    }

    #[test]
    fn the_owner_stops_a_browser_once_and_its_sites_are_sealed() {
        let board = BrowserBoard::default();
        let sites = Sites::default();
        sites.grant("a.example:443");
        let stopped = board.begin("browser-a", sites.clone(), Driven::default());
        assert!(!stopped.load(Ordering::SeqCst));
        assert_eq!(board.stop("browser-a"), Ok(()));
        assert!(stopped.load(Ordering::SeqCst), "its session is told");
        assert!(!sites.holds("a.example:443"), "revoked now");
        sites.grant("b.example:443");
        assert!(!sites.holds("b.example:443"), "and none granted after");
        assert_eq!(board.stop("browser-a"), Err(ControlRefusal::Finished));
        board.end("browser-a");
        assert_eq!(board.stop("browser-a"), Err(ControlRefusal::UnknownSession));
        assert_eq!(board.stop("browser-z"), Err(ControlRefusal::UnknownSession));
    }

    /// The handoffs `bus` carried for `session_id`, with their frames.
    fn handoffs(bus: &Bus) -> Vec<(Handoff, u64)> {
        carried(bus)
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
            .collect()
    }

    #[test]
    fn the_owner_takes_control_from_the_agent_and_hands_it_back_from_the_latest_frame() {
        let (board, bus) = watched();
        let _stopped = board.begin("browser-c", Sites::default(), Driven::default());
        let jpeg = encoded(&[0xFF, 0xD8, 0xFF, 0xE0]);
        assert!(board.frame("browser-c", "A", &jpeg));
        assert_eq!(board.turn("browser-c"), Turn::Act);
        board.done("browser-c");
        // Taken while the agent's idle: granted at once.
        assert_eq!(board.take("browser-c"), Ok(()));
        assert_eq!(
            handoffs(&bus),
            [(Handoff::Takeover, 1), (Handoff::Granted, 1)]
        );
        assert_eq!(board.turn("browser-c"), Turn::Wait, "the agent waits");
        assert_eq!(board.take("browser-c"), Err(ControlRefusal::NotAllowed));
        // Handed back only from the latest frame.
        assert_eq!(
            board.hand_back("browser-c", 0),
            Err(ControlRefusal::StaleFrame)
        );
        assert_eq!(board.hand_back("browser-c", 1), Ok(()));
        assert_eq!(
            board.hand_back("browser-c", 1),
            Err(ControlRefusal::Switching)
        );
        assert_eq!(board.take("browser-c"), Err(ControlRefusal::Switching));
        assert_eq!(board.turn("browser-c"), Turn::Resume, "look afresh first");
        assert!(board.frame("browser-c", "A", &jpeg));
        board.resumed("browser-c");
        board.done("browser-c");
        assert_eq!(
            handoffs(&bus),
            [
                (Handoff::Takeover, 1),
                (Handoff::Granted, 1),
                (Handoff::HandBack, 1),
                (Handoff::Resumed, 2),
            ],
            "resumed from the frame the agent took"
        );
        assert_eq!(board.turn("browser-c"), Turn::Act);
        assert_eq!(
            board.hand_back("browser-c", 2),
            Err(ControlRefusal::NotAllowed)
        );
    }

    /// A browser at `control`, at frame 3.
    fn at(control: Control) -> Watched {
        Watched {
            sites: Sites::default(),
            stopped: Arc::new(AtomicBool::new(false)),
            control,
            latest: 3,
            acting: false,
            browser: Driven::default(),
            title: String::new(),
            visit: None,
            profile: None,
        }
    }

    #[test]
    fn control_moves_only_from_where_it_must_be_and_never_once_stopped() {
        use ControlRefusal::{Finished, NotAllowed, Switching};
        for (control, from, expected) in [
            (Control::Agent, Control::Agent, Ok(())),
            (Control::User, Control::User, Ok(())),
            (Control::User, Control::Agent, Err(NotAllowed)),
            (Control::Agent, Control::User, Err(NotAllowed)),
            (Control::ToUser, Control::Agent, Err(Switching)),
            (Control::ToAgent, Control::User, Err(Switching)),
        ] {
            assert_eq!(
                BrowserBoard::open_to_control(&at(control), from),
                expected,
                "at {control:?}, from {from:?}"
            );
        }
        let stopped = at(Control::Agent);
        stopped.stopped.store(true, Ordering::SeqCst);
        assert_eq!(
            BrowserBoard::open_to_control(&stopped, Control::Agent),
            Err(Finished)
        );
    }

    #[test]
    fn a_handoff_moves_control_as_the_table_allows_and_tells_the_app_at_the_latest_frame() {
        let (board, bus) = watched();
        let mut browser = at(Control::Agent);
        board.hand("browser-h", &mut browser, Handoff::HandBack);
        assert_eq!(browser.control, Control::Agent, "the table refuses it");
        assert!(handoffs(&bus).is_empty(), "and the app isn't told");
        board.hand("browser-h", &mut browser, Handoff::Takeover);
        assert_eq!(browser.control, Control::ToUser);
        assert_eq!(handoffs(&bus), [(Handoff::Takeover, 3)]);
    }

    #[test]
    fn a_takeover_during_the_agents_op_is_granted_when_it_ends() {
        let (board, bus) = watched();
        let _stopped = board.begin("browser-d", Sites::default(), Driven::default());
        assert_eq!(board.turn("browser-d"), Turn::Act);
        assert_eq!(board.take("browser-d"), Ok(()));
        assert_eq!(
            handoffs(&bus),
            [(Handoff::Takeover, 0)],
            "not while it acts"
        );
        assert_eq!(board.turn("browser-d"), Turn::Wait);
        board.done("browser-d");
        assert_eq!(
            handoffs(&bus),
            [(Handoff::Takeover, 0), (Handoff::Granted, 0)]
        );
    }

    #[test]
    fn control_is_refused_for_a_browser_there_isnt_or_that_was_stopped() {
        let (board, _) = watched();
        assert_eq!(board.take("browser-z"), Err(ControlRefusal::UnknownSession));
        assert_eq!(
            board.hand_back("browser-z", 0),
            Err(ControlRefusal::UnknownSession)
        );
        assert_eq!(
            board.turn("browser-z"),
            Turn::Act,
            "an unwatched browser acts"
        );
        let _stopped = board.begin("browser-e", Sites::default(), Driven::default());
        assert_eq!(board.stop("browser-e"), Ok(()));
        assert_eq!(board.take("browser-e"), Err(ControlRefusal::Finished));
        assert_eq!(
            board.hand_back("browser-e", 0),
            Err(ControlRefusal::Finished)
        );
    }

    #[test]
    fn a_frame_too_big_for_the_stream_isnt_the_latest() {
        let (board, bus) = watched();
        let _stopped = board.begin("browser-f", Sites::default(), Driven::default());
        assert!(board.frame("browser-f", "A", &encoded(&[0xFF, 0xD8, 0xFF])));
        // A title too long for one frame with it: not shown, not the latest.
        let long = "t".repeat(hatchling_core::framing::MAX_FRAME_BYTES);
        assert!(!board.frame("browser-f", &long, &encoded(&[0xFF, 0xD8, 0xFF])));
        assert_eq!(board.take("browser-f"), Ok(()));
        assert_eq!(handoffs(&bus).first(), Some(&(Handoff::Takeover, 1)));
    }

    /// A browser that answers every command with `reply`, keeping what
    /// it was sent, and saying when it's closed.
    struct Scripted {
        reply: Result<BrowserReply, String>,
        sent: Arc<Mutex<Vec<BrowserCommand>>>,
        closed: Arc<AtomicBool>,
    }

    impl crate::browser_ops::Browser for Scripted {
        fn command(&mut self, command: &BrowserCommand) -> Result<BrowserReply, String> {
            self.sent
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(command.clone());
            self.reply.clone()
        }

        fn finish(self: Box<Self>) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    /// A [`Scripted`] browser, shared as the board holds it, and what it
    /// was sent, and whether it was closed.
    fn scripted(
        reply: Result<BrowserReply, String>,
    ) -> (Driven, Arc<Mutex<Vec<BrowserCommand>>>, Arc<AtomicBool>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let browser: Box<dyn crate::browser_ops::Browser> = Box::new(Scripted {
            reply,
            sent: Arc::clone(&sent),
            closed: Arc::clone(&closed),
        });
        (Arc::new(Mutex::new(Some(browser))), sent, closed)
    }

    #[test]
    fn the_owners_input_reaches_the_browser_only_while_they_drive_and_never_its_text() {
        let (board, bus) = watched();
        let jpeg = encoded(&[0xFF, 0xD8, 0xFF, 0xE0]);
        let (browser, sent, _) = scripted(Ok(BrowserReply::Frame { jpeg }));
        let _stopped = board.begin("browser-i", Sites::default(), browser);
        let typed = OwnerInput::Type {
            text: String::from("hunter2"),
        };
        let control = |reason| Err(InputRefusal::Control(reason));
        assert_eq!(
            board.input("browser-i", &typed),
            control(ControlRefusal::NotAllowed),
            "the agent drives"
        );
        assert_eq!(
            board.input("browser-z", &typed),
            control(ControlRefusal::UnknownSession)
        );
        assert_eq!(board.take("browser-i"), Ok(()));
        assert_eq!(board.input("browser-i", &typed), Ok(()));
        assert_eq!(
            *sent.lock().unwrap(),
            [BrowserCommand::Input {
                input: typed.clone()
            }]
        );
        let shown = carried(&bus);
        let wire = serde_json::to_string(&shown).unwrap();
        assert!(!wire.contains("hunter2"), "never the text typed: {wire}");
        let told = UiEventBody::BrowserAction(BrowserAction {
            session_id: String::from("browser-i"),
            by: Actor::User,
            narration: String::from("Typed 7 characters"),
        });
        assert!(shown.contains(&told), "{shown:?}");
        let frames: Vec<u64> = shown
            .iter()
            .filter_map(|body| {
                if let UiEventBody::BrowserFrame(frame) = body {
                    Some(frame.sequence)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(frames, [1], "the frame after it, numbered on");
        // Past what the browser takes: never sent.
        let not_possible = Err(InputRefusal::Browser(BrowserFailure::NotPossible));
        assert_eq!(
            board.input("browser-i", &OwnerInput::Click { x: 1280, y: 0 }),
            not_possible
        );
        let long = OwnerInput::Type {
            text: "x".repeat(MAX_TYPED_CHARS + 1),
        };
        assert_eq!(board.input("browser-i", &long), not_possible);
        assert_eq!(sent.lock().unwrap().len(), 1);
        // Handed back from that frame: the owner's input waits on the agent.
        assert_eq!(board.hand_back("browser-i", 1), Ok(()));
        assert_eq!(
            board.input("browser-i", &typed),
            control(ControlRefusal::Switching)
        );
        assert_eq!(board.stop("browser-i"), Ok(()));
        assert_eq!(
            board.input("browser-i", &typed),
            control(ControlRefusal::Finished)
        );
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_browser_that_fails_the_owners_input_says_why_and_one_out_of_turn_is_closed() {
        let (board, _) = watched();
        let scroll = OwnerInput::Scroll { down: true };
        let refused = BrowserReply::Failed {
            failure: BrowserFailure::NotPossible,
        };
        let page = BrowserReply::Page {
            snapshot: hatchling_core::browser::PageSnapshot::default(),
        };
        for (id, reply, failure, closes) in [
            ("browser-j", Ok(refused), BrowserFailure::NotPossible, false),
            ("browser-k", Ok(page), BrowserFailure::BrowserGone, true),
            (
                "browser-l",
                Ok(BrowserReply::Closed),
                BrowserFailure::BrowserGone,
                true,
            ),
            (
                "browser-m",
                Err(String::from("gone")),
                BrowserFailure::BrowserGone,
                true,
            ),
        ] {
            let (browser, _, closed) = scripted(reply);
            let _stopped = board.begin(id, Sites::default(), Arc::clone(&browser));
            assert_eq!(board.take(id), Ok(()));
            assert_eq!(
                board.input(id, &scroll),
                Err(InputRefusal::Browser(failure)),
                "{id}"
            );
            assert_eq!(closed.load(Ordering::SeqCst), closes, "{id}");
            assert_eq!(browser.lock().unwrap().is_none(), closes, "{id}");
        }
        assert_eq!(
            board.input("browser-k", &scroll),
            Err(InputRefusal::Browser(BrowserFailure::BrowserGone)),
            "closed, it takes nothing more"
        );
    }

    #[test]
    fn the_owners_input_is_bounded_and_told_in_their_words() {
        for (input, within) in [
            (OwnerInput::Click { x: 1279, y: 799 }, true),
            (OwnerInput::Click { x: 1280, y: 0 }, false),
            (OwnerInput::Click { x: 0, y: 800 }, false),
            (
                OwnerInput::Type {
                    text: "\u{e9}".repeat(MAX_TYPED_CHARS),
                },
                true,
            ),
            (
                OwnerInput::Type {
                    text: "\u{e9}".repeat(MAX_TYPED_CHARS + 1),
                },
                false,
            ),
            (
                OwnerInput::Key {
                    key: hatchling_core::browser::Key::Enter,
                },
                true,
            ),
            (OwnerInput::Scroll { down: false }, true),
        ] {
            assert_eq!(within_bounds(&input), within, "{input:?}");
        }
        for (input, said) in [
            (OwnerInput::Click { x: 12, y: 34 }, "Clicked at 12, 34"),
            (
                OwnerInput::Type {
                    text: String::from("s3cret"),
                },
                "Typed 6 characters",
            ),
            (
                OwnerInput::Key {
                    key: hatchling_core::browser::Key::Tab,
                },
                "Pressed Tab",
            ),
            (OwnerInput::Scroll { down: true }, "Scrolled down"),
            (OwnerInput::Scroll { down: false }, "Scrolled up"),
        ] {
            assert_eq!(owner_said(&input), said);
        }
    }

    #[test]
    fn a_sign_in_is_kept_only_while_the_owner_drives_with_its_visit_first() {
        let (board, _) = watched();
        let sites = Sites::default();
        sites.grant("idp.example:443");
        sites.grant("a.example:443");
        assert_eq!(
            sites.granted(),
            [String::from("a.example:443"), String::from("idp.example:443")],
            "in order"
        );
        let _stopped = board.begin("browser-s", sites, Driven::default());
        assert_eq!(
            board.sign_in_of("browser-s").map(|(sites, _)| sites),
            Err(ControlRefusal::NotAllowed),
            "the agent drives"
        );
        assert_eq!(board.take("browser-s"), Ok(()));
        assert_eq!(
            board.sign_in_of("browser-s").map(|(sites, _)| sites),
            Err(ControlRefusal::NotAllowed),
            "no site visited"
        );
        board.visiting("browser-s", "a.example:443");
        assert_eq!(
            board.sign_in_of("browser-s").map(|(sites, _)| sites),
            Ok(vec![
                String::from("a.example:443"),
                String::from("idp.example:443")
            ]),
            "the visit first, then the rest, each once"
        );
        assert_eq!(
            board.sign_in_of("browser-z").map(|(sites, _)| sites),
            Err(ControlRefusal::UnknownSession)
        );
        assert!(board.profile_of("browser-s").is_none());
        board.profiled("browser-s", "profile-1");
        assert_eq!(
            board.profile_of("browser-s").map(|(profile, _)| profile),
            Some(String::from("profile-1"))
        );
        board.visiting("browser-z", "x.example:443");
        board.profiled("browser-z", "profile-2");
        assert!(board.profile_of("browser-z").is_none(), "no such browser");
    }

    #[test]
    fn a_broker_has_one_board() {
        let config = crate::web_ops::tests::web_config("browser-board");
        assert!(Arc::ptr_eq(&board(&config), &board(&config)));
        let other = crate::web_ops::tests::web_config("browser-board-other");
        assert!(!Arc::ptr_eq(&board(&config), &board(&other)));
    }
}
