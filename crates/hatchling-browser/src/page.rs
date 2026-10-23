//! The session's page, driven over DevTools: open, snapshot, act, scroll,
//! close.
//!
//! A snapshot is read in an isolated world: a script context of the
//! controller's own on the page's document, which the page's scripts
//! can't see or change. The elements it found are kept there, so an
//! action names one by its index and the page can't swap the list. What
//! comes back is still the page's, untrusted, and bounded again here
//! ([`PageSnapshot::fitted`]).
//!
//! Clicks and keys go through the browser's input, as a person's would;
//! typed text through `Input.insertText`. The owner's own input, while
//! they drive, goes the same way, at a point of the viewport or where the
//! page's focus is, and is answered with a picture of the page.

use std::time::{Duration, Instant};

use hatchling_core::browser::{
    BrowserCommand, BrowserFailure, BrowserReply, Cookie, Key, MAX_COOKIE_PAGE_BYTES, MAX_COOKIES,
    MAX_TYPED_CHARS, OwnerInput, PageAction, PageSnapshot, SameSite, SignInField, VIEWPORT,
};
use hatchling_core::ui::Concealed;
use serde_json::{Value, json};

use crate::cdp::CdpError;
use crate::launch::Browser;

/// How long a call to the browser may take.
pub const CALL: Duration = Duration::from_secs(10);

/// How long a page may take to load.
pub const LOAD: Duration = Duration::from_secs(30);

/// How long, after an action, a navigation it started has to show itself.
pub const SETTLE: Duration = Duration::from_millis(500);

/// The JPEG qualities a frame is tried at, best first, until it's within
/// [`hatchling_core::browser::MAX_FRAME_JPEG_BASE64`].
pub const FRAME_QUALITIES: [u8; 4] = [60, 40, 25, 15];

/// The snapshot, read in the isolated world. It keeps the elements it
/// names in that world's own global, for the actions that follow.
const SNAPSHOT_JS: &str = include_str!("snapshot.js");

/// An action's first step, in the isolated world: the element named by
/// index scrolled into view, and then its centre, focus, or a choice.
const ELEMENT_JS: &str = include_str!("element.js");

/// The page a session drives.
#[derive(Debug)]
struct Page {
    /// The page's DevTools session.
    session: String,
    /// The isolated world the last snapshot was read in, which holds its
    /// elements.
    world: Option<i64>,
}

/// A browser and the one page it shows.
#[derive(Debug)]
pub struct Session {
    browser: Option<Browser>,
    page: Option<Page>,
}

impl Session {
    /// A session on `browser`, with no page yet.
    #[must_use]
    pub const fn new(browser: Browser) -> Self {
        Self {
            browser: Some(browser),
            page: None,
        }
    }

    /// Does `command`, and says what came of it.
    pub fn handle(&mut self, command: BrowserCommand) -> BrowserReply {
        let done = match command {
            BrowserCommand::Open { url } => self.open(&url),
            BrowserCommand::Snapshot => self.snapshot(),
            BrowserCommand::Act { element, action } => self.act(element, &action),
            BrowserCommand::Scroll { down } => self.scroll(down),
            BrowserCommand::Frame => {
                return match self.frame() {
                    Ok(jpeg) => BrowserReply::Frame { jpeg },
                    Err(failure) => BrowserReply::Failed { failure },
                };
            }
            BrowserCommand::Input { input } => {
                return match self.input(&input) {
                    Ok(jpeg) => BrowserReply::Frame { jpeg },
                    Err(failure) => BrowserReply::Failed { failure },
                };
            }
            BrowserCommand::Cookies { from } => {
                return match self.cookies(from) {
                    Ok((cookies, next)) => BrowserReply::Cookies { cookies, next },
                    Err(failure) => BrowserReply::Failed { failure },
                };
            }
            BrowserCommand::SetCookies { cookies } => {
                return match self.set_cookies(&cookies) {
                    Ok(()) => BrowserReply::CookiesSet,
                    Err(failure) => BrowserReply::Failed { failure },
                };
            }
            BrowserCommand::Fill {
                element,
                field,
                text,
                origin,
            } => self.fill(element, field, &text, &origin),
            BrowserCommand::Close => {
                self.close();
                return BrowserReply::Closed;
            }
        };
        match done {
            Ok(snapshot) => BrowserReply::Page { snapshot },
            Err(failure) => BrowserReply::Failed { failure },
        }
    }

    /// Closes the browser, if it's still open.
    pub fn close(&mut self) {
        self.page = None;
        if let Some(browser) = self.browser.take() {
            let _ = browser.close();
        }
    }

    fn browser(&mut self) -> Result<&mut Browser, BrowserFailure> {
        self.browser.as_mut().ok_or(BrowserFailure::BrowserGone)
    }

    /// A call on the page's session, or the browser's when `session` is
    /// `None`.
    fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, CdpError> {
        let browser = self.browser.as_mut().ok_or(CdpError::Gone)?;
        browser
            .cdp
            .call(session, method, params, Instant::now() + CALL)
    }

    fn page_session(&self) -> Result<String, BrowserFailure> {
        self.page
            .as_ref()
            .map(|page| page.session.clone())
            .ok_or(BrowserFailure::NoPage)
    }

    fn open(&mut self, url: &str) -> Result<PageSnapshot, BrowserFailure> {
        if self.page.is_none() {
            let target = self
                .call(None, "Target.createTarget", json!({"url": "about:blank"}))
                .map_err(gone_or(BrowserFailure::LoadFailed))?;
            let target = text(&target, "targetId")?;
            let attached = self
                .call(
                    None,
                    "Target.attachToTarget",
                    json!({"targetId": target, "flatten": true}),
                )
                .map_err(gone_or(BrowserFailure::LoadFailed))?;
            let session = text(&attached, "sessionId")?;
            self.call(Some(&session), "Page.enable", json!({}))
                .map_err(gone_or(BrowserFailure::LoadFailed))?;
            self.page = Some(Page {
                session,
                world: None,
            });
        }
        let session = self.page_session()?;
        self.browser()?.cdp.forget_events();
        let navigated = self
            .call(Some(&session), "Page.navigate", json!({"url": url}))
            .map_err(gone_or(BrowserFailure::LoadFailed))?;
        if navigated.get("errorText").and_then(Value::as_str).is_some() {
            return Err(BrowserFailure::LoadFailed);
        }
        self.loaded(&session, Instant::now() + LOAD)?;
        self.snapshot()
    }

    /// Waits for the page's load event, by `deadline`.
    fn loaded(&mut self, session: &str, deadline: Instant) -> Result<(), BrowserFailure> {
        self.browser()?
            .cdp
            .event(deadline, |event| {
                is_event(event, "Page.loadEventFired", session)
            })
            .map(drop)
            .map_err(gone_or(BrowserFailure::Timeout))
    }

    fn snapshot(&mut self) -> Result<PageSnapshot, BrowserFailure> {
        let session = self.page_session()?;
        let tree = self
            .call(Some(&session), "Page.getFrameTree", json!({}))
            .map_err(gone_or(BrowserFailure::NoPage))?;
        let frame = tree
            .pointer("/frameTree/frame/id")
            .and_then(Value::as_str)
            .ok_or(BrowserFailure::NoPage)?
            .to_owned();
        let world = self
            .call(
                Some(&session),
                "Page.createIsolatedWorld",
                json!({"frameId": frame, "worldName": "hatchling"}),
            )
            .map_err(gone_or(BrowserFailure::NoPage))?;
        let world = world
            .get("executionContextId")
            .and_then(Value::as_i64)
            .ok_or(BrowserFailure::NoPage)?;
        let value = self.evaluate(&session, world, SNAPSHOT_JS)?;
        let snapshot: PageSnapshot =
            serde_json::from_value(value).map_err(|_| BrowserFailure::NoPage)?;
        if let Some(page) = self.page.as_mut() {
            page.world = Some(world);
        }
        Ok(snapshot.fitted())
    }

    /// `expression`'s value in the isolated world `world`.
    fn evaluate(
        &mut self,
        session: &str,
        world: i64,
        expression: &str,
    ) -> Result<Value, BrowserFailure> {
        let evaluated = self
            .call(
                Some(session),
                "Runtime.evaluate",
                json!({"expression": expression, "contextId": world, "returnByValue": true}),
            )
            // The world is gone when the page has navigated since.
            .map_err(gone_or(BrowserFailure::UnknownElement))?;
        if evaluated.get("exceptionDetails").is_some() {
            return Err(BrowserFailure::NotPossible);
        }
        Ok(evaluated
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// The element `element` of the last snapshot: scrolled into view,
    /// then `step` done to it (`"box"`, `"focus"`, or `"choose"` with
    /// `option`). Its centre, for `"box"`.
    fn element(&mut self, element: u32, step: &str, option: &str) -> Result<Value, BrowserFailure> {
        let session = self.page_session()?;
        let world = self
            .page
            .as_ref()
            .and_then(|page| page.world)
            .ok_or(BrowserFailure::UnknownElement)?;
        let call = format!(
            "({ELEMENT_JS})({element}, {}, {})",
            Value::from(step),
            Value::from(option)
        );
        let value = self.evaluate(&session, world, &call)?;
        match value.get("outcome").and_then(Value::as_str) {
            Some("ok") => Ok(value),
            Some("unknown") => Err(BrowserFailure::UnknownElement),
            _ => Err(BrowserFailure::NotPossible),
        }
    }

    fn act(&mut self, element: u32, action: &PageAction) -> Result<PageSnapshot, BrowserFailure> {
        let session = self.page_session()?;
        self.browser()?.cdp.forget_events();
        match action {
            PageAction::Click => self.click(&session, element)?,
            PageAction::Type { text } => {
                if text.chars().count() > MAX_TYPED_CHARS {
                    return Err(BrowserFailure::NotPossible);
                }
                self.element(element, "focus", "")?;
                self.insert(&session, text)?;
            }
            PageAction::Key { key } => {
                self.element(element, "focus", "")?;
                self.press(&session, *key)?;
            }
            PageAction::Choose { option } => {
                self.element(element, "choose", option)?;
            }
        }
        self.settle(&session)?;
        self.snapshot()
    }

    /// Fills `element`, the sign-in form's `field`, with `text`: only while
    /// the top frame is on `origin`, before and after the field takes the
    /// focus (a focus handler that navigates gets nothing typed), and only
    /// into the kind of field `field` names.
    fn fill(
        &mut self,
        element: u32,
        field: SignInField,
        text: &Concealed,
        origin: &str,
    ) -> Result<PageSnapshot, BrowserFailure> {
        let session = self.page_session()?;
        if text.expose().chars().count() > MAX_TYPED_CHARS {
            return Err(BrowserFailure::NotPossible);
        }
        self.browser()?.cdp.forget_events();
        self.on_origin(&session, origin)?;
        let step = match field {
            SignInField::Username => "username",
            SignInField::Password => "password",
        };
        self.element(element, step, "")?;
        self.on_origin(&session, origin)?;
        self.insert(&session, text.expose())?;
        self.settle(&session)?;
        self.snapshot()
    }

    /// Whether the top frame is on `origin` now, as the browser reports it
    /// (its security origin, never what the page's script says).
    fn on_origin(&mut self, session: &str, origin: &str) -> Result<(), BrowserFailure> {
        let tree = self
            .call(Some(session), "Page.getFrameTree", json!({}))
            .map_err(gone_or(BrowserFailure::NoPage))?;
        if tree
            .pointer("/frameTree/frame/securityOrigin")
            .and_then(Value::as_str)
            == Some(origin)
        {
            Ok(())
        } else {
            Err(BrowserFailure::OtherOrigin)
        }
    }

    /// The owner's `input`, through the browser's input, and a picture of
    /// the page after it.
    fn input(&mut self, input: &OwnerInput) -> Result<String, BrowserFailure> {
        let session = self.page_session()?;
        self.browser()?.cdp.forget_events();
        match input {
            OwnerInput::Click { x, y } => {
                let (width, height) = VIEWPORT;
                if *x >= width || *y >= height {
                    return Err(BrowserFailure::NotPossible);
                }
                self.click_at(&session, f64::from(*x), f64::from(*y))?;
            }
            OwnerInput::Type { text } => {
                if text.chars().count() > MAX_TYPED_CHARS {
                    return Err(BrowserFailure::NotPossible);
                }
                self.insert(&session, text)?;
            }
            OwnerInput::Key { key } => self.press(&session, *key)?,
            OwnerInput::Scroll { down } => self.scroll_by(&session, *down)?,
        }
        self.settle(&session)?;
        self.frame()
    }

    /// The browser's cookies from the `from`th on, in the jar's order, as
    /// many as fit one page, and where the next page starts. A cookie past
    /// the bounds is left out; a jar of more than [`MAX_COOKIES`] is
    /// refused whole, as is one the browser can't give.
    fn cookies(&mut self, from: u32) -> Result<(Vec<Cookie>, Option<u32>), BrowserFailure> {
        let jar = self
            .call(None, "Storage.getCookies", json!({}))
            .map_err(gone_or(BrowserFailure::NotPossible))?;
        let listed = jar
            .get("cookies")
            .and_then(Value::as_array)
            .ok_or(BrowserFailure::NotPossible)?;
        page_of(listed, from)
    }

    /// Sets `cookies` in the browser, all of them or none: each within the
    /// bounds, and the page within [`MAX_COOKIES`].
    fn set_cookies(&mut self, cookies: &[Cookie]) -> Result<(), BrowserFailure> {
        if cookies.len() > MAX_COOKIES || !cookies.iter().all(Cookie::within_bounds) {
            return Err(BrowserFailure::NotPossible);
        }
        let params: Vec<Value> = cookies.iter().map(cookie_param).collect();
        self.call(None, "Storage.setCookies", json!({ "cookies": params }))
            .map_err(gone_or(BrowserFailure::NotPossible))?;
        Ok(())
    }
}

/// The page of the jar DevTools `listed` from its `from`th cookie on, in the
/// jar's order (domain, path, name), as many as fit
/// [`MAX_COOKIE_PAGE_BYTES`], and where the next page starts. A cookie past
/// the bounds, or partitioned, is left out; a jar of more than
/// [`MAX_COOKIES`] is refused whole.
fn page_of(listed: &[Value], from: u32) -> Result<(Vec<Cookie>, Option<u32>), BrowserFailure> {
    if listed.len() > MAX_COOKIES {
        return Err(BrowserFailure::NotPossible);
    }
    let mut cookies: Vec<Cookie> = listed
        .iter()
        .filter_map(cookie_of)
        .filter(Cookie::within_bounds)
        .collect();
    cookies.sort_by(|a, b| (&a.domain, &a.path, &a.name).cmp(&(&b.domain, &b.path, &b.name)));
    let start = usize::try_from(from).unwrap_or(usize::MAX);
    let mut page = Vec::new();
    let mut bytes = 0;
    for cookie in cookies.iter().skip(start) {
        let size = serde_json::to_vec(cookie).map_or(usize::MAX, |json| json.len() + 1);
        if bytes + size > MAX_COOKIE_PAGE_BYTES {
            break;
        }
        bytes += size;
        page.push(cookie.clone());
    }
    let end = start.saturating_add(page.len());
    let next = (end < cookies.len())
        .then(|| u32::try_from(end).ok())
        .flatten();
    Ok((page, next))
}

impl Session {
    /// Types `text` where the page's focus is.
    fn insert(&mut self, session: &str, text: &str) -> Result<(), BrowserFailure> {
        self.call(Some(session), "Input.insertText", json!({"text": text}))
            .map_err(gone_or(BrowserFailure::NotPossible))?;
        Ok(())
    }

    /// A left click at the element's centre, through the browser's input.
    fn click(&mut self, session: &str, element: u32) -> Result<(), BrowserFailure> {
        let centre = self.element(element, "box", "")?;
        let (Some(x), Some(y)) = (
            centre.get("x").and_then(Value::as_f64),
            centre.get("y").and_then(Value::as_f64),
        ) else {
            return Err(BrowserFailure::NotPossible);
        };
        self.click_at(session, x, y)
    }

    /// A left click at `x`, `y` of the viewport, through the browser's
    /// input.
    fn click_at(&mut self, session: &str, x: f64, y: f64) -> Result<(), BrowserFailure> {
        for kind in ["mousePressed", "mouseReleased"] {
            self.call(
                Some(session),
                "Input.dispatchMouseEvent",
                json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1}),
            )
            .map_err(gone_or(BrowserFailure::NotPossible))?;
        }
        Ok(())
    }

    /// One key, down and up, through the browser's input.
    fn press(&mut self, session: &str, key: Key) -> Result<(), BrowserFailure> {
        let (name, code, text) = key_event(key);
        for kind in ["keyDown", "keyUp"] {
            let mut event = json!({
                "type": kind,
                "key": name,
                "code": name,
                "windowsVirtualKeyCode": code,
                "nativeVirtualKeyCode": code,
            });
            if let (true, Some(fields)) =
                (kind == "keyDown" && !text.is_empty(), event.as_object_mut())
            {
                fields.insert(String::from("text"), Value::from(text));
            }
            self.call(Some(session), "Input.dispatchKeyEvent", event)
                .map_err(gone_or(BrowserFailure::NotPossible))?;
        }
        Ok(())
    }

    /// After an action: if it started a navigation within [`SETTLE`],
    /// waits for the new page to load.
    fn settle(&mut self, session: &str) -> Result<(), BrowserFailure> {
        let started = self.browser()?.cdp.event(Instant::now() + SETTLE, |event| {
            is_event(event, "Page.frameStartedLoading", session)
        });
        match started {
            Ok(_) => self.loaded(session, Instant::now() + LOAD),
            Err(CdpError::Timeout) => Ok(()),
            Err(_) => Err(BrowserFailure::BrowserGone),
        }
    }

    fn scroll(&mut self, down: bool) -> Result<PageSnapshot, BrowserFailure> {
        let session = self.page_session()?;
        self.scroll_by(&session, down)?;
        self.snapshot()
    }

    /// Scrolls the page by most of a screen, down or up.
    fn scroll_by(&mut self, session: &str, down: bool) -> Result<(), BrowserFailure> {
        let world = self
            .page
            .as_ref()
            .and_then(|page| page.world)
            .ok_or(BrowserFailure::NoPage)?;
        let sign = if down { "" } else { "-" };
        self.evaluate(
            session,
            world,
            &format!("window.scrollBy(0, {sign}0.8 * window.innerHeight)"),
        )?;
        Ok(())
    }
}

impl Session {
    /// A picture of what the page's viewport shows, at half size, at the
    /// best of [`FRAME_QUALITIES`] within the frame's bound.
    fn frame(&mut self) -> Result<String, BrowserFailure> {
        let session = self.page_session()?;
        let metrics = self
            .call(Some(&session), "Page.getLayoutMetrics", json!({}))
            .map_err(gone_or(BrowserFailure::NotPossible))?;
        let at = |field: &str| {
            metrics
                .pointer(&format!("/cssVisualViewport/{field}"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        };
        let (width, height) = VIEWPORT;
        let clip = json!({
            "x": at("pageX"),
            "y": at("pageY"),
            "width": width,
            "height": height,
            "scale": 0.5,
        });
        for quality in FRAME_QUALITIES {
            let shot = self
                .call(
                    Some(&session),
                    "Page.captureScreenshot",
                    json!({"format": "jpeg", "quality": quality, "clip": clip}),
                )
                .map_err(gone_or(BrowserFailure::NotPossible))?;
            let jpeg = text(&shot, "data")?;
            if jpeg.len() <= hatchling_core::browser::MAX_FRAME_JPEG_BASE64 {
                return Ok(jpeg);
            }
        }
        Err(BrowserFailure::NotPossible)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// A cookie as DevTools lists it, if it's whole: a name, a value, a domain
/// and a path. A partitioned cookie (one site's, kept for another) is left
/// out: restored without its partition, it would be sent where it never
/// was.
fn cookie_of(listed: &Value) -> Option<Cookie> {
    if listed.get("partitionKey").is_some_and(|key| !key.is_null()) {
        return None;
    }
    let text = |field: &str| listed.get(field).and_then(Value::as_str).map(str::to_owned);
    let flag = |field: &str| listed.get(field).and_then(Value::as_bool).unwrap_or(false);
    let session = flag("session");
    // Whole seconds, through their decimal text: one no i64 holds is
    // left out, and the cookie kept as a session's, which never outlives
    // the browser.
    let expires = listed
        .get("expires")
        .and_then(Value::as_f64)
        .filter(|seconds| !session && seconds.is_finite() && *seconds >= 0.0)
        .and_then(|seconds| format!("{}", seconds.floor()).parse::<i64>().ok());
    let same_site = match listed.get("sameSite").and_then(Value::as_str) {
        Some("Strict") => Some(SameSite::Strict),
        Some("Lax") => Some(SameSite::Lax),
        Some("None") => Some(SameSite::None),
        Some(_) | None => None,
    };
    Some(Cookie {
        name: text("name")?,
        value: text("value")?,
        domain: text("domain")?,
        path: text("path")?,
        expires,
        secure: flag("secure"),
        http_only: flag("httpOnly"),
        same_site,
    })
}

/// `cookie` as DevTools sets it.
fn cookie_param(cookie: &Cookie) -> Value {
    let mut param = json!({
        "name": cookie.name,
        "value": cookie.value,
        "domain": cookie.domain,
        "path": cookie.path,
        "secure": cookie.secure,
        "httpOnly": cookie.http_only,
    });
    if let Some(fields) = param.as_object_mut() {
        if let Some(expires) = cookie.expires {
            fields.insert(String::from("expires"), Value::from(expires));
        }
        if let Some(same_site) = cookie.same_site {
            let same_site = match same_site {
                SameSite::Strict => "Strict",
                SameSite::Lax => "Lax",
                SameSite::None => "None",
            };
            fields.insert(String::from("sameSite"), Value::from(same_site));
        }
    }
    param
}

/// A key's name, its virtual key code, and the text it types, if any.
const fn key_event(key: Key) -> (&'static str, u32, &'static str) {
    match key {
        Key::Enter => ("Enter", 13, "\r"),
        Key::Tab => ("Tab", 9, ""),
        Key::Escape => ("Escape", 27, ""),
        Key::Backspace => ("Backspace", 8, ""),
        Key::ArrowUp => ("ArrowUp", 38, ""),
        Key::ArrowDown => ("ArrowDown", 40, ""),
        Key::ArrowLeft => ("ArrowLeft", 37, ""),
        Key::ArrowRight => ("ArrowRight", 39, ""),
    }
}

/// Whether `event` is `method`, on the page session `session`.
fn is_event(event: &Value, method: &str, session: &str) -> bool {
    event.get("method").and_then(Value::as_str) == Some(method)
        && event.get("sessionId").and_then(Value::as_str) == Some(session)
}

/// `field` of a result, as text.
fn text(result: &Value, field: &str) -> Result<String, BrowserFailure> {
    result
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(BrowserFailure::LoadFailed)
}

/// A pipe that's gone is [`BrowserFailure::BrowserGone`], a timeout
/// [`BrowserFailure::Timeout`]; anything else is `otherwise`.
fn gone_or(otherwise: BrowserFailure) -> impl Fn(CdpError) -> BrowserFailure {
    move |error| match error {
        CdpError::Gone | CdpError::Unreadable => BrowserFailure::BrowserGone,
        CdpError::Timeout => BrowserFailure::Timeout,
        CdpError::Refused { .. } => otherwise,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::sync::{Arc, Mutex};

    use hatchling_core::browser::PageElement;

    use hatchling_core::browser::MAX_COOKIE_BYTES;

    use super::*;
    use crate::cdp::Cdp;

    /// The page the fake browser shows.
    fn shown() -> Value {
        json!({
            "url": "http://a.test/",
            "title": "A",
            "text": "hello",
            "elements": [{"id": 0, "role": "button", "name": "Go"}],
            "truncated": false,
        })
    }

    fn event(method: &str) -> Value {
        json!({"method": method, "params": {}, "sessionId": "S"})
    }

    /// What the fake browser answers `request` with, and the events it
    /// sends after.
    fn answer(request: &Value) -> (Value, Vec<Value>) {
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let param = |name: &str| {
            request
                .pointer(&format!("/params/{name}"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        match method {
            "Target.createTarget" => (json!({"targetId": "T"}), Vec::new()),
            "Target.attachToTarget" => (json!({"sessionId": "S"}), Vec::new()),
            "Page.navigate" if param("url") == "http://fail.test/" => (
                json!({"frameId": "F", "errorText": "net::ERR_FAILED"}),
                Vec::new(),
            ),
            "Page.navigate" => (json!({"frameId": "F"}), vec![event("Page.loadEventFired")]),
            "Page.getFrameTree" => (
                json!({"frameTree": {"frame": {"id": "F", "securityOrigin": "https://a.test"}}}),
                Vec::new(),
            ),
            "Page.createIsolatedWorld" => (json!({"executionContextId": 7}), Vec::new()),
            "Runtime.evaluate" => (evaluated(&param("expression")), Vec::new()),
            "Storage.getCookies" => (
                json!({ "cookies": [listed("a.test", "sid", "1")] }),
                Vec::new(),
            ),
            "Page.getLayoutMetrics" => (
                json!({"cssVisualViewport": {"pageX": 0.0, "pageY": 400.0}}),
                Vec::new(),
            ),
            "Page.captureScreenshot" => {
                let quality = request
                    .pointer("/params/quality")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                // Past the bound above 25; exactly at it, at 25.
                let bound = hatchling_core::browser::MAX_FRAME_JPEG_BASE64;
                let data = match quality {
                    26.. => "A".repeat(bound + 1),
                    25 => format!("/9j/{}", "A".repeat(bound - 4)),
                    _ => String::from("/9j/4AAQ"),
                };
                (json!({ "data": data }), Vec::new())
            }
            // A click starts a navigation, which loads.
            "Input.dispatchMouseEvent" if param("type") == "mouseReleased" => (
                json!({}),
                vec![
                    event("Page.frameStartedLoading"),
                    event("Page.loadEventFired"),
                ],
            ),
            _ => (json!({}), Vec::new()),
        }
    }

    /// A cookie as DevTools lists it: `name` for `domain`, valued `value`.
    fn listed(domain: &str, name: &str, value: &str) -> Value {
        json!({
            "name": name, "value": value, "domain": domain, "path": "/",
            "expires": 1_900_000_000.5, "size": 4, "httpOnly": true, "secure": true,
            "session": false, "sameSite": "Lax", "priority": "Medium",
        })
    }

    /// The cookie [`listed`] lists, as the jar keeps it.
    fn kept(domain: &str, name: &str, value: &str) -> Cookie {
        Cookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: domain.to_owned(),
            path: String::from("/"),
            expires: Some(1_900_000_000),
            secure: true,
            http_only: true,
            same_site: Some(SameSite::Lax),
        }
    }

    #[test]
    fn a_cookie_is_read_whole_from_devtools_or_left_out() {
        assert_eq!(
            cookie_of(&listed("a.test", "sid", "1")),
            Some(kept("a.test", "sid", "1"))
        );
        let with = |field: &str, value: Value| {
            let mut cookie = listed("a.test", "sid", "1");
            if let Some(fields) = cookie.as_object_mut() {
                fields.insert(field.to_owned(), value);
            }
            cookie_of(&cookie)
        };
        let session = with("session", json!(true)).unwrap();
        assert_eq!(session.expires, None, "a session cookie has no expiry");
        assert_eq!(with("expires", json!(-1)).unwrap().expires, None);
        assert_eq!(
            with("expires", json!(1e300)).unwrap().expires,
            None,
            "past any i64: kept as a session's"
        );
        assert_eq!(with("expires", json!(12.9)).unwrap().expires, Some(12));
        for (same_site, read) in [
            ("Strict", Some(SameSite::Strict)),
            ("None", Some(SameSite::None)),
            ("Lax", Some(SameSite::Lax)),
            ("Other", None),
        ] {
            assert_eq!(with("sameSite", json!(same_site)).unwrap().same_site, read);
        }
        assert_eq!(
            with("partitionKey", json!({"topLevelSite": "https://b.test"})),
            None,
            "partitioned"
        );
        assert!(with("partitionKey", Value::Null).is_some());
        for field in ["name", "value", "domain", "path"] {
            assert_eq!(with(field, json!(7)), None, "{field}");
        }
        let unflagged = with("secure", Value::Null).unwrap();
        assert!(!unflagged.secure);
    }

    #[test]
    fn a_cookie_is_set_as_devtools_takes_it() {
        assert_eq!(
            cookie_param(&kept("a.test", "sid", "1")),
            json!({"name": "sid", "value": "1", "domain": "a.test", "path": "/", "secure": true,
                   "httpOnly": true, "expires": 1_900_000_000, "sameSite": "Lax"})
        );
        let bare = Cookie {
            expires: None,
            same_site: None,
            ..kept("a.test", "sid", "1")
        };
        assert_eq!(
            cookie_param(&bare),
            json!({"name": "sid", "value": "1", "domain": "a.test", "path": "/", "secure": true, "httpOnly": true})
        );
        for (same_site, wire) in [(SameSite::Strict, "Strict"), (SameSite::None, "None")] {
            let param = cookie_param(&Cookie {
                same_site: Some(same_site),
                ..bare.clone()
            });
            assert_eq!(param.get("sameSite"), Some(&json!(wire)));
        }
    }

    #[test]
    fn a_jar_is_read_in_pages_in_its_order_within_its_bounds() {
        // Thirty cookies of about 2 KiB: more than one page holds.
        let mut jar: Vec<Value> = (0..30)
            .map(|i| listed(&format!("d{:02}.test", 29 - i), "big", &"v".repeat(2000)))
            .collect();
        jar.push(listed("a.test", &"n".repeat(MAX_COOKIE_BYTES), "too big"));
        let (first, next) = page_of(&jar, 0).unwrap();
        let next = next.unwrap();
        assert_eq!(usize::try_from(next).unwrap(), first.len());
        assert!(serde_json::to_vec(&first).unwrap().len() <= MAX_COOKIE_PAGE_BYTES);
        let (rest, after) = page_of(&jar, next).unwrap();
        assert_eq!(after, None, "the last page");
        let all: Vec<String> = first
            .iter()
            .chain(&rest)
            .map(|c| c.domain.clone())
            .collect();
        let ordered: Vec<String> = (0..30).map(|i| format!("d{i:02}.test")).collect();
        assert_eq!(
            all, ordered,
            "in order, each once, the one too big left out"
        );
        assert_eq!(page_of(&jar, 999).unwrap(), (Vec::new(), None));
        // At the bound, whole; past it, refused.
        let full: Vec<Value> = (0..MAX_COOKIES)
            .map(|i| listed("a.test", &format!("c{i}"), "1"))
            .collect();
        assert!(page_of(&full, 0).is_ok());
        let over: Vec<Value> = (0..=MAX_COOKIES)
            .map(|i| listed("a.test", &format!("c{i}"), "1"))
            .collect();
        assert_eq!(page_of(&over, 0), Err(BrowserFailure::NotPossible));
    }

    #[test]
    fn cookies_are_read_from_and_set_in_the_browser_itself() {
        let (mut session, seen) = fake();
        assert_eq!(
            session.handle(BrowserCommand::Cookies { from: 0 }),
            BrowserReply::Cookies {
                cookies: vec![kept("a.test", "sid", "1")],
                next: None
            }
        );
        let asked = sent(&seen, "Storage.getCookies");
        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked.first().and_then(|call| call.get("sessionId")),
            None,
            "the browser's, not a page's"
        );
        let cookies = vec![kept("a.test", "sid", "1"), kept("b.test", "x", "2")];
        assert_eq!(
            session.handle(BrowserCommand::SetCookies {
                cookies: cookies.clone()
            }),
            BrowserReply::CookiesSet
        );
        // And directly: the jar's first page, and an empty page set.
        assert_eq!(
            session.cookies(0),
            Ok((vec![kept("a.test", "sid", "1")], None))
        );
        assert_eq!(session.set_cookies(&[]), Ok(()));
        let set = sent(&seen, "Storage.setCookies");
        assert_eq!(
            set.first()
                .and_then(|call| call.pointer("/params/cookies"))
                .cloned(),
            Some(json!(cookies.iter().map(cookie_param).collect::<Vec<_>>()))
        );
        // Past the bounds: nothing is set.
        let too_big = Cookie {
            value: "v".repeat(MAX_COOKIE_BYTES),
            ..kept("a.test", "sid", "1")
        };
        let too_many = vec![kept("a.test", "sid", "1"); MAX_COOKIES + 1];
        for cookies in [vec![too_big], too_many] {
            assert_eq!(
                session.handle(BrowserCommand::SetCookies { cookies }),
                BrowserReply::Failed {
                    failure: BrowserFailure::NotPossible
                }
            );
        }
        assert_eq!(sent(&seen, "Storage.setCookies").len(), 2);
    }

    /// What an expression evaluates to in the fake page.
    fn evaluated(expression: &str) -> Value {
        if expression == SNAPSHOT_JS {
            return json!({"result": {"value": shown()}});
        }
        if expression.starts_with("window.scrollBy") {
            return json!({"result": {"type": "undefined"}});
        }
        if expression == "throw" {
            return json!({"result": {}, "exceptionDetails": {"text": "x"}});
        }
        // `(ELEMENT_JS)(index, step, option)`: the arguments are JSON.
        let args = expression
            .rsplit_once(")(")
            .and_then(|(_, args)| args.strip_suffix(')'))
            .and_then(|args| serde_json::from_str::<Value>(&format!("[{args}]")).ok())
            .unwrap_or_default();
        let outcome = match (
            args.get(0).and_then(Value::as_u64),
            args.get(1).and_then(Value::as_str),
            args.get(2).and_then(Value::as_str),
        ) {
            (Some(9), _, _) => json!({"outcome": "unknown"}),
            (Some(_), Some("box"), _) => json!({"outcome": "ok", "x": 12.5, "y": 40.0}),
            // Element 1 is a text field, 2 a password field; the rest take
            // neither kind of fill.
            (Some(_), Some("focus"), _)
            | (Some(_), Some("choose"), Some("B"))
            | (Some(1), Some("username"), _)
            | (Some(2), Some("password"), _) => json!({"outcome": "ok"}),
            _ => json!({"outcome": "not_possible"}),
        };
        json!({"result": {"value": outcome}})
    }

    /// A session on a fake browser: a thread answering its pipe with
    /// [`answer`], keeping every request it was sent.
    fn fake() -> (Session, Arc<Mutex<Vec<Value>>>) {
        let (browser_reads, to) = rustix::pipe::pipe().unwrap();
        let (from, browser_writes) = rustix::pipe::pipe().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut reads = std::fs::File::from(browser_reads);
            let mut writes = std::fs::File::from(browser_writes);
            let mut message = Vec::new();
            let mut byte = [0u8; 1];
            while reads.read_exact(&mut byte).is_ok() {
                let [next] = byte;
                if next != 0 {
                    message.push(next);
                    continue;
                }
                let request: Value = serde_json::from_slice(&message).unwrap();
                message.clear();
                kept.lock().unwrap().push(request.clone());
                let (result, events) = answer(&request);
                let mut reply = json!({"id": request.get("id").cloned(), "result": result});
                if let (Some(session), Some(fields)) =
                    (request.get("sessionId"), reply.as_object_mut())
                {
                    fields.insert(String::from("sessionId"), session.clone());
                }
                for out in std::iter::once(reply).chain(events) {
                    let mut bytes = serde_json::to_vec(&out).unwrap();
                    bytes.push(0);
                    if writes.write_all(&bytes).is_err() {
                        return;
                    }
                }
                if request.get("method").and_then(Value::as_str) == Some("Browser.close") {
                    return;
                }
            }
        });
        let browser = Browser::over(Cdp::new(to, from).unwrap());
        (Session::new(browser), seen)
    }

    /// The requests of `method` the fake browser was sent.
    fn sent(seen: &Mutex<Vec<Value>>, method: &str) -> Vec<Value> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|r| r.get("method").and_then(Value::as_str) == Some(method))
            .cloned()
            .collect()
    }

    fn snapshot() -> PageSnapshot {
        PageSnapshot {
            url: String::from("http://a.test/"),
            title: String::from("A"),
            text: String::from("hello"),
            elements: vec![PageElement {
                id: 0,
                role: String::from("button"),
                name: String::from("Go"),
            }],
            truncated: false,
        }
    }

    #[test]
    fn a_frame_is_the_viewport_at_half_size_at_the_best_quality_that_fits() {
        let (mut session, seen) = fake();
        assert_eq!(
            session.handle(BrowserCommand::Frame),
            BrowserReply::Failed {
                failure: BrowserFailure::NoPage
            },
            "no page, no frame"
        );
        let _ = session.handle(BrowserCommand::Open {
            url: String::from("http://a.test/"),
        });
        let at_the_bound = format!(
            "/9j/{}",
            "A".repeat(hatchling_core::browser::MAX_FRAME_JPEG_BASE64 - 4)
        );
        assert_eq!(
            session.handle(BrowserCommand::Frame),
            BrowserReply::Frame {
                jpeg: at_the_bound.clone()
            },
            "exactly at the bound is within it"
        );
        let shots = sent(&seen, "Page.captureScreenshot");
        let qualities: Vec<u64> = shots
            .iter()
            .filter_map(|shot| shot.pointer("/params/quality").and_then(Value::as_u64))
            .collect();
        assert_eq!(qualities, [60, 40, 25], "stepped down until it fit");
        let clip = shots
            .last()
            .and_then(|shot| shot.pointer("/params/clip"))
            .cloned();
        assert_eq!(
            clip,
            Some(json!({"x": 0.0, "y": 400.0, "width": 1280, "height": 800, "scale": 0.5})),
            "what the viewport shows, scrolled as it is"
        );
        assert!(
            shots
                .iter()
                .all(|shot| shot.pointer("/params/format") == Some(&json!("jpeg")))
        );
        assert_eq!(session.frame(), Ok(at_the_bound), "again, directly");
        session.close();
        assert_eq!(session.frame(), Err(BrowserFailure::NoPage), "closed");
    }

    #[test]
    fn a_page_is_opened_once_and_its_snapshot_comes_back_bounded() {
        let (mut session, seen) = fake();
        assert_eq!(session.page_session(), Err(BrowserFailure::NoPage));
        assert!(session.browser().is_ok());
        assert_eq!(
            session.handle(BrowserCommand::Snapshot),
            BrowserReply::Failed {
                failure: BrowserFailure::NoPage
            }
        );
        let open = |url: &str| BrowserCommand::Open {
            url: url.to_owned(),
        };
        assert_eq!(
            session.handle(open("http://a.test/")),
            BrowserReply::Page {
                snapshot: snapshot()
            }
        );
        assert_eq!(session.page_session(), Ok(String::from("S")));
        assert_eq!(
            session.handle(open("http://a.test/again")),
            BrowserReply::Page {
                snapshot: snapshot()
            }
        );
        // One page, attached flat, its events on; each URL navigated to.
        assert_eq!(sent(&seen, "Target.createTarget").len(), 1);
        let attached = sent(&seen, "Target.attachToTarget");
        assert_eq!(
            attached.first().and_then(|a| a.pointer("/params/flatten")),
            Some(&json!(true))
        );
        assert_eq!(
            sent(&seen, "Page.enable")
                .first()
                .and_then(|e| e.get("sessionId")),
            Some(&json!("S"))
        );
        let urls: Vec<Value> = sent(&seen, "Page.navigate")
            .iter()
            .filter_map(|n| n.pointer("/params/url").cloned())
            .collect();
        assert_eq!(
            urls,
            [json!("http://a.test/"), json!("http://a.test/again")]
        );
        // The snapshot is read in the isolated world, by value.
        let read = sent(&seen, "Runtime.evaluate");
        assert!(
            read.iter()
                .all(|r| r.pointer("/params/contextId") == Some(&json!(7)))
        );
        assert!(
            read.iter()
                .all(|r| r.pointer("/params/returnByValue") == Some(&json!(true)))
        );
        assert_eq!(
            session.handle(open("http://fail.test/")),
            BrowserReply::Failed {
                failure: BrowserFailure::LoadFailed
            }
        );
        // No load event comes twice: waiting for another times out.
        assert_eq!(
            session.loaded("S", Instant::now() + Duration::from_millis(100)),
            Err(BrowserFailure::Timeout)
        );
        assert_eq!(session.handle(BrowserCommand::Close), BrowserReply::Closed);
        assert_eq!(sent(&seen, "Browser.close").len(), 1);
        // Closed: nothing more reaches a browser.
        assert_eq!(
            session.handle(open("http://a.test/")),
            BrowserReply::Failed {
                failure: BrowserFailure::BrowserGone
            }
        );
    }

    #[test]
    fn an_element_is_found_in_the_isolated_world_by_index() {
        let (mut session, _seen) = fake();
        assert_eq!(session.element(0, "box", ""), Err(BrowserFailure::NoPage));
        session.open("http://a.test/").unwrap();
        // The element step: its centre, an element that's gone, a choice.
        let centre = session.element(0, "box", "").unwrap();
        assert_eq!(
            (centre.get("x"), centre.get("y")),
            (Some(&json!(12.5)), Some(&json!(40.0)))
        );
        assert_eq!(
            session.element(9, "box", ""),
            Err(BrowserFailure::UnknownElement)
        );
        assert_eq!(
            session.element(0, "choose", "Z"),
            Err(BrowserFailure::NotPossible)
        );
        assert_eq!(
            session.evaluate("S", 7, "throw"),
            Err(BrowserFailure::NotPossible)
        );
        assert_eq!(
            session.evaluate("S", 7, "window.scrollBy(0, 1)"),
            Ok(Value::Null)
        );
    }

    #[test]
    fn a_fill_types_only_on_its_origin_and_only_into_its_kind_of_field() {
        let (mut session, seen) = fake();
        let fill =
            |element: u32, field: SignInField, text: &str, origin: &str| BrowserCommand::Fill {
                element,
                field,
                text: Concealed::new(text.to_owned()),
                origin: origin.to_owned(),
            };
        let failed = |failure| BrowserReply::Failed { failure };
        assert_eq!(
            session.handle(fill(2, SignInField::Password, "s3cret", "https://a.test")),
            failed(BrowserFailure::NoPage)
        );
        session.open("http://a.test/").unwrap();
        assert_eq!(
            session.handle(fill(1, SignInField::Username, "ada", "https://a.test")),
            BrowserReply::Page {
                snapshot: snapshot()
            }
        );
        assert_eq!(
            session.handle(fill(2, SignInField::Password, "s3cret", "https://a.test")),
            BrowserReply::Page {
                snapshot: snapshot()
            }
        );
        let typed = |seen: &Mutex<Vec<Value>>| {
            sent(seen, "Input.insertText")
                .iter()
                .filter_map(|call| {
                    call.pointer("/params/text")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(typed(&seen), ["ada", "s3cret"]);
        // The origin is checked before the field takes the focus, and after.
        let checks = sent(&seen, "Page.getFrameTree").len();
        assert!(checks >= 4, "{checks}");
        for (command, failure) in [
            (
                fill(2, SignInField::Password, "s3cret", "https://b.test"),
                BrowserFailure::OtherOrigin,
            ),
            (
                fill(2, SignInField::Password, "s3cret", "https://a.test:8443"),
                BrowserFailure::OtherOrigin,
            ),
            (
                fill(1, SignInField::Password, "s3cret", "https://a.test"),
                BrowserFailure::NotPossible,
            ),
            (
                fill(2, SignInField::Username, "ada", "https://a.test"),
                BrowserFailure::NotPossible,
            ),
            (
                fill(9, SignInField::Password, "s3cret", "https://a.test"),
                BrowserFailure::UnknownElement,
            ),
            (
                fill(
                    2,
                    SignInField::Password,
                    &"x".repeat(MAX_TYPED_CHARS + 1),
                    "https://a.test",
                ),
                BrowserFailure::NotPossible,
            ),
        ] {
            assert_eq!(session.handle(command), failed(failure));
        }
        assert_eq!(typed(&seen).len(), 2, "nothing more typed");
        // Directly: the origin as the browser reports it.
        assert_eq!(session.on_origin("S", "https://a.test"), Ok(()));
        assert_eq!(
            session.on_origin("S", "https://a.test/"),
            Err(BrowserFailure::OtherOrigin)
        );
        assert_eq!(
            session.fill(
                2,
                SignInField::Password,
                &Concealed::new(String::from("pw")),
                "https://a.test"
            ),
            Ok(snapshot())
        );
    }

    #[test]
    fn an_action_acts_through_the_browsers_input() {
        let (mut session, seen) = fake();
        session.open("http://a.test/").unwrap();
        // A click: pressed and released at the centre, and the navigation
        // it starts waited for.
        assert_eq!(session.act(0, &PageAction::Click), Ok(snapshot()));
        let mouse = sent(&seen, "Input.dispatchMouseEvent");
        let kinds: Vec<Value> = mouse
            .iter()
            .filter_map(|m| m.pointer("/params/type").cloned())
            .collect();
        assert_eq!(kinds, [json!("mousePressed"), json!("mouseReleased")]);
        assert!(
            mouse
                .iter()
                .all(|m| m.pointer("/params/x") == Some(&json!(12.5))
                    && m.pointer("/params/y") == Some(&json!(40.0))
                    && m.pointer("/params/button") == Some(&json!("left")))
        );
        session.click("S", 0).unwrap();
        assert_eq!(sent(&seen, "Input.dispatchMouseEvent").len(), 4);
        assert_eq!(session.click("S", 9), Err(BrowserFailure::UnknownElement));
        // A key: down with its text, up without.
        session.press("S", Key::Enter).unwrap();
        let keys = sent(&seen, "Input.dispatchKeyEvent");
        assert_eq!(
            keys.first().and_then(|k| k.pointer("/params/type")),
            Some(&json!("keyDown"))
        );
        assert_eq!(
            keys.first().and_then(|k| k.pointer("/params/text")),
            Some(&json!("\r"))
        );
        assert_eq!(
            keys.first()
                .and_then(|k| k.pointer("/params/windowsVirtualKeyCode")),
            Some(&json!(13))
        );
        assert_eq!(
            keys.get(1).and_then(|k| k.pointer("/params/type")),
            Some(&json!("keyUp"))
        );
        assert_eq!(keys.get(1).and_then(|k| k.pointer("/params/text")), None);
        assert_eq!(
            session.act(0, &PageAction::Key { key: Key::Tab }),
            Ok(snapshot())
        );
        let tab = sent(&seen, "Input.dispatchKeyEvent");
        assert_eq!(
            tab.get(2).and_then(|k| k.pointer("/params/text")),
            None,
            "a tab types nothing"
        );
        // Typed text, through the input; too much is refused unsent.
        assert_eq!(
            session.act(
                0,
                &PageAction::Type {
                    text: String::from("hi")
                }
            ),
            Ok(snapshot())
        );
        let typed = sent(&seen, "Input.insertText");
        assert_eq!(
            typed.first().and_then(|t| t.pointer("/params/text")),
            Some(&json!("hi"))
        );
        let long = "x".repeat(MAX_TYPED_CHARS + 1);
        assert_eq!(
            session.act(0, &PageAction::Type { text: long }),
            Err(BrowserFailure::NotPossible)
        );
        assert_eq!(sent(&seen, "Input.insertText").len(), 1);
        assert_eq!(
            session.act(
                0,
                &PageAction::Choose {
                    option: String::from("B")
                }
            ),
            Ok(snapshot())
        );
        assert_eq!(
            session.act(9, &PageAction::Click),
            Err(BrowserFailure::UnknownElement)
        );
    }

    #[test]
    fn scrolling_moves_most_of_a_screen_either_way() {
        let (mut session, seen) = fake();
        assert_eq!(session.scroll(true), Err(BrowserFailure::NoPage));
        session.open("http://a.test/").unwrap();
        assert_eq!(session.scroll(true), Ok(snapshot()));
        assert_eq!(session.scroll(false), Ok(snapshot()));
        let scrolls: Vec<Value> = sent(&seen, "Runtime.evaluate")
            .iter()
            .filter_map(|e| e.pointer("/params/expression").cloned())
            .filter(|e| e.as_str().is_some_and(|e| e.starts_with("window.scrollBy")))
            .collect();
        assert_eq!(
            scrolls,
            [
                json!("window.scrollBy(0, 0.8 * window.innerHeight)"),
                json!("window.scrollBy(0, -0.8 * window.innerHeight)"),
            ]
        );
    }

    /// The owner's `input`: the reply.
    fn owner(session: &mut Session, input: OwnerInput) -> BrowserReply {
        session.handle(BrowserCommand::Input { input })
    }

    #[test]
    fn the_owners_input_goes_through_the_browsers_input_and_is_answered_with_a_picture() {
        let (mut session, seen) = fake();
        let not_possible = BrowserReply::Failed {
            failure: BrowserFailure::NotPossible,
        };
        assert_eq!(
            owner(&mut session, OwnerInput::Scroll { down: true }),
            BrowserReply::Failed {
                failure: BrowserFailure::NoPage
            },
            "nothing to drive before a page"
        );
        session.open("http://a.test/").unwrap();
        let picture = |reply: &BrowserReply| matches!(reply, BrowserReply::Frame { jpeg } if jpeg.starts_with("/9j/"));
        // A click at a point of the viewport, which starts a load, waited for.
        let clicked = owner(&mut session, OwnerInput::Click { x: 1279, y: 799 });
        assert!(picture(&clicked), "{clicked:?}");
        let mouse = sent(&seen, "Input.dispatchMouseEvent");
        let at: Vec<(Option<&str>, Option<f64>, Option<f64>)> = mouse
            .iter()
            .map(|event| {
                (
                    event.pointer("/params/type").and_then(Value::as_str),
                    event.pointer("/params/x").and_then(Value::as_f64),
                    event.pointer("/params/y").and_then(Value::as_f64),
                )
            })
            .collect();
        assert_eq!(
            at,
            [
                (Some("mousePressed"), Some(1279.0), Some(799.0)),
                (Some("mouseReleased"), Some(1279.0), Some(799.0)),
            ]
        );
        assert_eq!(
            sent(&seen, "Page.navigate").len(),
            1,
            "the load isn't a navigation of ours"
        );
        // Outside the viewport: nothing is clicked.
        for (x, y) in [(1280, 0), (0, 800)] {
            assert_eq!(
                owner(&mut session, OwnerInput::Click { x, y }),
                not_possible
            );
        }
        assert_eq!(sent(&seen, "Input.dispatchMouseEvent").len(), 2);
        // Text where the focus is, within the bound.
        let typed = owner(
            &mut session,
            OwnerInput::Type {
                text: String::from("owner"),
            },
        );
        assert!(picture(&typed), "{typed:?}");
        let at_the_bound = "é".repeat(MAX_TYPED_CHARS);
        let long = owner(&mut session, OwnerInput::Type { text: at_the_bound });
        assert!(picture(&long), "{long:?}");
        let past = "é".repeat(MAX_TYPED_CHARS + 1);
        assert_eq!(
            owner(&mut session, OwnerInput::Type { text: past }),
            not_possible
        );
        let inserted: Vec<usize> = sent(&seen, "Input.insertText")
            .iter()
            .filter_map(|e| e.pointer("/params/text").and_then(Value::as_str))
            .map(|text| text.chars().count())
            .collect();
        assert_eq!(inserted, [5, MAX_TYPED_CHARS]);
        // A key, and a scroll.
        let pressed = owner(&mut session, OwnerInput::Key { key: Key::Enter });
        assert!(picture(&pressed), "{pressed:?}");
        let keys = sent(&seen, "Input.dispatchKeyEvent");
        assert_eq!(keys.len(), 2, "down and up");
        let scrolled = owner(&mut session, OwnerInput::Scroll { down: false });
        assert!(picture(&scrolled), "{scrolled:?}");
        let scrolls = sent(&seen, "Runtime.evaluate")
            .iter()
            .filter_map(|e| e.pointer("/params/expression").cloned())
            .filter(|e| e == "window.scrollBy(0, -0.8 * window.innerHeight)")
            .count();
        assert_eq!(scrolls, 1);
    }

    #[test]
    fn a_click_at_a_point_presses_and_releases_there_and_a_scroll_needs_a_page() {
        let (mut session, seen) = fake();
        assert_eq!(
            session.scroll_by("S", true),
            Err(BrowserFailure::NoPage),
            "no page, nothing to scroll"
        );
        session.open("http://a.test/").unwrap();
        assert_eq!(session.click_at("S", 3.5, 7.0), Ok(()));
        let mouse = sent(&seen, "Input.dispatchMouseEvent");
        assert_eq!(
            mouse
                .iter()
                .filter_map(|event| event.get("params").cloned())
                .collect::<Vec<_>>(),
            [
                json!({"type": "mousePressed", "x": 3.5, "y": 7.0, "button": "left", "clickCount": 1}),
                json!({"type": "mouseReleased", "x": 3.5, "y": 7.0, "button": "left", "clickCount": 1}),
            ]
        );
        assert_eq!(session.scroll_by("S", true), Ok(()));
        let scrolled = sent(&seen, "Runtime.evaluate")
            .iter()
            .filter_map(|e| e.pointer("/params/expression").cloned())
            .any(|e| e == "window.scrollBy(0, 0.8 * window.innerHeight)");
        assert!(scrolled);
    }

    #[test]
    fn keys_are_the_ones_a_form_needs() {
        assert_eq!(key_event(Key::Enter), ("Enter", 13, "\r"));
        assert_eq!(key_event(Key::Tab), ("Tab", 9, ""));
        assert_eq!(key_event(Key::Escape), ("Escape", 27, ""));
        assert_eq!(key_event(Key::Backspace), ("Backspace", 8, ""));
        assert_eq!(key_event(Key::ArrowUp), ("ArrowUp", 38, ""));
        assert_eq!(key_event(Key::ArrowDown), ("ArrowDown", 40, ""));
        assert_eq!(key_event(Key::ArrowLeft), ("ArrowLeft", 37, ""));
        assert_eq!(key_event(Key::ArrowRight), ("ArrowRight", 39, ""));
    }

    #[test]
    fn a_broken_pipe_is_a_gone_browser_and_a_refusal_is_the_steps_own() {
        let failure = gone_or(BrowserFailure::LoadFailed);
        assert_eq!(failure(CdpError::Gone), BrowserFailure::BrowserGone);
        assert_eq!(failure(CdpError::Unreadable), BrowserFailure::BrowserGone);
        assert_eq!(failure(CdpError::Timeout), BrowserFailure::Timeout);
        assert_eq!(
            failure(CdpError::Refused {
                method: String::from("m"),
                message: String::from("x"),
            }),
            BrowserFailure::LoadFailed
        );
        assert_eq!(text(&json!({"a": "b"}), "a"), Ok(String::from("b")));
        let event = json!({"method": "Page.loadEventFired", "sessionId": "S"});
        assert!(is_event(&event, "Page.loadEventFired", "S"));
        assert!(
            !is_event(&event, "Page.loadEventFired", "T"),
            "another page's"
        );
        assert!(!is_event(&event, "Page.frameStartedLoading", "S"));
        assert_eq!(text(&json!({"a": 1}), "a"), Err(BrowserFailure::LoadFailed));
    }
}
