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
//! typed text through `Input.insertText`.

use std::time::{Duration, Instant};

use hatchling_core::browser::{
    BrowserCommand, BrowserFailure, BrowserReply, Key, MAX_TYPED_CHARS, PageAction, PageSnapshot,
};
use serde_json::{Value, json};

use crate::cdp::CdpError;
use crate::launch::Browser;

/// How long a call to the browser may take.
pub const CALL: Duration = Duration::from_secs(10);

/// How long a page may take to load.
pub const LOAD: Duration = Duration::from_secs(30);

/// How long, after an action, a navigation it started has to show itself.
pub const SETTLE: Duration = Duration::from_millis(500);

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
                self.call(Some(&session), "Input.insertText", json!({"text": text}))
                    .map_err(gone_or(BrowserFailure::NotPossible))?;
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

    /// A left click at the element's centre, through the browser's input.
    fn click(&mut self, session: &str, element: u32) -> Result<(), BrowserFailure> {
        let centre = self.element(element, "box", "")?;
        let (Some(x), Some(y)) = (
            centre.get("x").and_then(Value::as_f64),
            centre.get("y").and_then(Value::as_f64),
        ) else {
            return Err(BrowserFailure::NotPossible);
        };
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
        let world = self
            .page
            .as_ref()
            .and_then(|page| page.world)
            .ok_or(BrowserFailure::NoPage)?;
        let sign = if down { "" } else { "-" };
        self.evaluate(
            &session,
            world,
            &format!("window.scrollBy(0, {sign}0.8 * window.innerHeight)"),
        )?;
        self.snapshot()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
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
            "Page.getFrameTree" => (json!({"frameTree": {"frame": {"id": "F"}}}), Vec::new()),
            "Page.createIsolatedWorld" => (json!({"executionContextId": 7}), Vec::new()),
            "Runtime.evaluate" => (evaluated(&param("expression")), Vec::new()),
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
            (Some(_), Some("focus"), _) | (Some(_), Some("choose"), Some("B")) => {
                json!({"outcome": "ok"})
            }
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
    fn an_action_finds_its_element_in_the_isolated_world_and_acts_through_input() {
        let (mut session, seen) = fake();
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
