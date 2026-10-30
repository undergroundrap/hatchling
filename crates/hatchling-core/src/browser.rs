//! The browser cell's protocol (WO-029): what the broker asks of the
//! controller that drives a headless Chromium in the cell, and what comes
//! back.
//!
//! One frame each way per command, over the cell's own connection. The
//! controller runs beside page code, so everything it sends is untrusted:
//! a page's text and its elements are data, bounded here before they're
//! sent, and bounded again by whoever reads them ([`PageSnapshot::bounded`]).

use serde::{Deserialize, Serialize};

/// The most characters of a page's text a snapshot carries.
pub const MAX_TEXT_CHARS: usize = 12 * 1024;

/// The most elements a snapshot names.
pub const MAX_ELEMENTS: usize = 150;

/// The most characters of an element's name, a page's title or its URL.
pub const MAX_LABEL_CHARS: usize = 120;

/// The most characters the agent may type in one action.
pub const MAX_TYPED_CHARS: usize = 4096;

/// The most characters of a one-time code the owner gives (WO-030).
pub const MAX_CODE_CHARS: usize = 64;

/// The page's viewport, in CSS pixels, width and height: what a
/// [`BrowserCommand::Frame`] shows, at half size.
pub const VIEWPORT: (u32, u32) = (1280, 800);

/// The most base64 a frame's JPEG may be: room is left in one frame for
/// the event that carries it on to the owner's app, which a frame too big
/// for would end.
pub const MAX_FRAME_JPEG_BASE64: usize = 60 * 1024;

/// The most cookies a jar holds: a jar with more is refused whole, read or
/// restored (WO-030).
pub const MAX_COOKIES: usize = 1024;

/// The most bytes of a cookie's name and value together, as browsers
/// bound them; its domain and path are each bounded by
/// [`MAX_COOKIE_ATTRIBUTE_BYTES`].
pub const MAX_COOKIE_BYTES: usize = 4096;

/// The most bytes of a cookie's domain, and of its path.
pub const MAX_COOKIE_ATTRIBUTE_BYTES: usize = 1024;

/// The most bytes of JSON a page of cookies holds, read or restored: one
/// frame's worth, less room for the message around it.
pub const MAX_COOKIE_PAGE_BYTES: usize = 48 * 1024;

/// A command to the browser cell.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum BrowserCommand {
    /// Load `url` in the session's page, opening the page if there is
    /// none, and reply with its snapshot.
    Open {
        /// Where to go. The broker has already decided it may.
        url: String,
    },
    /// Reply with the page's snapshot as it is now.
    Snapshot,
    /// Act on an element of the last snapshot, and reply with the
    /// snapshot after.
    Act {
        /// The element's id, from the last snapshot.
        element: u32,
        /// What to do to it.
        action: PageAction,
    },
    /// Scroll the page by most of a screen, and reply with the snapshot.
    Scroll {
        /// Down, or up.
        down: bool,
    },
    /// A picture of the page as it is now, for the owner to watch. The
    /// reply is [`BrowserReply::Frame`].
    Frame,
    /// The owner's own input, while they drive: done through the
    /// browser's input, as a person's would be. The reply is a picture of
    /// the page after it, [`BrowserReply::Frame`].
    Input {
        /// What the owner did.
        input: OwnerInput,
    },
    /// The browser's cookies, from the `from`th on, in the jar's order
    /// (domain, path, name): as many as fit one page
    /// ([`MAX_COOKIE_PAGE_BYTES`]). The reply is [`BrowserReply::Cookies`].
    /// What a signed-in profile keeps between its sessions (WO-030).
    Cookies {
        /// Where this page starts.
        from: u32,
    },
    /// Set `cookies` in the browser: a page of a jar the broker kept. The
    /// reply is [`BrowserReply::CookiesSet`].
    SetCookies {
        /// The cookies, within the bounds a read holds them to.
        cookies: Vec<Cookie>,
    },
    /// Fill a sign-in form's field with what the broker keeps (WO-030):
    /// only while the page's top frame is on `origin`, and only into the
    /// kind of field `field` names. The reply is the snapshot after.
    Fill {
        /// The field's element, from the last snapshot.
        element: u32,
        /// Which field it is: a username goes only into a text or email
        /// field, a password only into a password field.
        field: SignInField,
        /// What to type: the broker's, never the agent's. The controller
        /// hides it from every snapshot after (WO-031).
        text: crate::ui::Concealed,
        /// The exact origin the top frame must be on, `https://host[:port]`.
        origin: String,
    },
    /// Fill a sign-in form with the sign-in the vault keeper sent the cell
    /// as `nonce`, on its vault channel (WO-031): its username into
    /// `username`, if one is named, and its password into `password`, each
    /// as [`BrowserCommand::Fill`] does, on the origin the keeper sent. The
    /// broker names the fill and never has it. The reply is the snapshot
    /// after.
    SignIn {
        /// The fill's nonce, as the keeper told the broker.
        nonce: String,
        /// The username field's element, from the last snapshot.
        username: Option<u32>,
        /// The password field's element.
        password: u32,
    },
    /// Close the page and the browser. The reply is [`BrowserReply::Closed`].
    Close,
}

/// What the owner does to the page, from the app, while they drive it.
/// Where the page's focus is, or at a point of its viewport: the owner
/// sees a picture, not the page's elements.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "input", rename_all = "snake_case")]
pub enum OwnerInput {
    /// A left click at a point of the viewport, in CSS pixels from its
    /// top left: within [`VIEWPORT`].
    Click {
        /// Across.
        x: u32,
        /// Down.
        y: u32,
    },
    /// Type `text` where the page's focus is, as keystrokes would.
    Type {
        /// At most [`MAX_TYPED_CHARS`] characters.
        text: String,
    },
    /// Press one key where the page's focus is.
    Key {
        /// The key.
        key: Key,
    },
    /// Scroll the page by most of a screen.
    Scroll {
        /// Down, or up.
        down: bool,
    },
}

/// What to do to an element.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PageAction {
    /// Click it.
    Click,
    /// Focus it and type `text`, as keystrokes would.
    Type {
        /// At most [`MAX_TYPED_CHARS`] characters.
        text: String,
    },
    /// Focus it and press one key.
    Key {
        /// The key.
        key: Key,
    },
    /// Choose the option of a list whose text is `option`.
    Choose {
        /// The option's text, as the snapshot showed it.
        option: String,
    },
}

/// The keys an action may press: the ones a form needs, and nothing that
/// reaches the browser's own shortcuts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Key {
    /// Enter.
    Enter,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// Backspace.
    Backspace,
    /// The up arrow.
    ArrowUp,
    /// The down arrow.
    ArrowDown,
    /// The left arrow.
    ArrowLeft,
    /// The right arrow.
    ArrowRight,
}

/// The browser cell's answer to a command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum BrowserReply {
    /// The page, after the command.
    Page {
        /// What it shows.
        snapshot: PageSnapshot,
    },
    /// A picture of the page, what its viewport shows at half size: a
    /// JPEG, as standard base64, at most [`MAX_FRAME_JPEG_BASE64`]. The page's pixels, untrusted like the rest: the broker checks
    /// it's a JPEG before anyone sees it.
    Frame {
        /// The JPEG's bytes, standard base64.
        jpeg: String,
    },
    /// A page of the browser's cookies, and where the next starts, if one
    /// does. Untrusted, like the rest: a page sets its own cookies.
    Cookies {
        /// The cookies.
        cookies: Vec<Cookie>,
        /// The `from` of the next page.
        next: Option<u32>,
    },
    /// The cookies are set.
    CookiesSet,
    /// The command wasn't done.
    Failed {
        /// Why.
        failure: BrowserFailure,
    },
    /// The browser is closed.
    Closed,
}

/// Why a command wasn't done.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BrowserFailure {
    /// There's no page yet: open one first.
    NoPage,
    /// No element of the last snapshot has that id, or it's gone from
    /// the page: take a new snapshot.
    UnknownElement,
    /// The element can't take that action (typing into a button, a
    /// choice it doesn't offer), or the text is too long.
    NotPossible,
    /// The page didn't load, or the action didn't finish, in time.
    Timeout,
    /// The page couldn't be loaded.
    LoadFailed,
    /// The browser has exited, or its connection broke.
    BrowserGone,
    /// A command that wasn't understood.
    BadCommand,
    /// The page isn't on the origin a sign-in is for: nothing was filled.
    OtherOrigin,
}

/// The field of a sign-in form a fill types into (WO-030).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignInField {
    /// The username: a text or email field.
    Username,
    /// The password: a password field.
    Password,
    /// A one-time code the owner gave: a text, number or telephone field.
    Code,
}

/// What the vault keeper sends a browser cell to fill a sign-in (WO-031).
///
/// On the cell's vault channel: this, then the username and the password,
/// each a raw frame of its own. The broker, which asked for the fill, is
/// given the nonce alone, and names it to the controller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignInFill {
    /// The fill's own nonce: the controller types a fill only when the
    /// broker names it.
    pub nonce: String,
    /// The exact origin the sign-in is kept for, `https://host[:port]`,
    /// from the keeper's store: the top frame must be on it.
    pub origin: String,
}

/// A cookie, as the browser keeps it. Its value is a secret: what a
/// signed-in session is. It reaches the broker, which seals it, and the
/// browser, and never a snapshot, a log, or anything the agent sees.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cookie {
    /// Its name.
    pub name: String,
    /// Its value.
    pub value: String,
    /// The domain it's sent to: a host, or `.host` for its subdomains too.
    pub domain: String,
    /// The path it's sent for.
    pub path: String,
    /// When it expires, in seconds since the epoch; a session cookie has
    /// none.
    pub expires: Option<i64>,
    /// Sent over `https` only.
    pub secure: bool,
    /// Out of reach of the page's script.
    pub http_only: bool,
    /// When it's sent with a request from another site.
    pub same_site: Option<SameSite>,
}

impl std::fmt::Debug for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cookie")
            .field("name", &self.name)
            .field("domain", &self.domain)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Cookie {
    /// Whether it's within the bounds a jar holds every cookie to:
    /// [`MAX_COOKIE_BYTES`] of name and value, and a domain and a path each
    /// within [`MAX_COOKIE_ATTRIBUTE_BYTES`].
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        self.name.len() + self.value.len() <= MAX_COOKIE_BYTES
            && self.domain.len() <= MAX_COOKIE_ATTRIBUTE_BYTES
            && self.path.len() <= MAX_COOKIE_ATTRIBUTE_BYTES
    }
}

/// When a cookie is sent with a request another site started.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SameSite {
    /// Never.
    Strict,
    /// With a top-level navigation only.
    Lax,
    /// Always (it must be [`Cookie::secure`]).
    None,
}

/// A page, as the agent sees it: its text and the elements it can act
/// on. Untrusted: everything here came from the page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PageSnapshot {
    /// Where the page is.
    pub url: String,
    /// Its title.
    pub title: String,
    /// Its visible text, at most [`MAX_TEXT_CHARS`].
    pub text: String,
    /// What can be acted on, at most [`MAX_ELEMENTS`].
    pub elements: Vec<PageElement>,
    /// Whether the text or the elements were cut to fit.
    pub truncated: bool,
}

/// An element the agent can act on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PageElement {
    /// Its id, for an action: valid until the next snapshot.
    pub id: u32,
    /// What it is: `link`, `button`, `textbox`, `password`, `checkbox`,
    /// `list` and so on.
    pub role: String,
    /// What it says, at most [`MAX_LABEL_CHARS`]. A password field's
    /// value is never here.
    pub name: String,
}

/// What stands in a snapshot for a secret the browser was given (WO-030).
pub const HIDDEN: &str = "[hidden]";

/// What a page shows, under [`MIN_HIDDEN_CHARS`] characters, isn't hidden:
/// a secret that short would hide half the page's words.
pub const MIN_HIDDEN_CHARS: usize = 4;

impl PageSnapshot {
    /// This snapshot with each of `secrets` hidden wherever the page shows
    /// it: its URL, its title, its text, its elements' names (WO-030). A
    /// page can read a field it was given and show it; the agent never
    /// sees it.
    #[must_use]
    pub fn hiding(mut self, secrets: &[crate::ui::Concealed]) -> Self {
        for secret in secrets
            .iter()
            .map(crate::ui::Concealed::expose)
            .filter(|secret| secret.chars().count() >= MIN_HIDDEN_CHARS)
        {
            self.url = self.url.replace(secret, HIDDEN);
            self.title = self.title.replace(secret, HIDDEN);
            self.text = self.text.replace(secret, HIDDEN);
            for element in &mut self.elements {
                element.name = element.name.replace(secret, HIDDEN);
            }
        }
        self
    }

    /// This snapshot held to the bounds: text, elements, and each label
    /// cut to fit, on character boundaries, and `truncated` set if
    /// anything was cut. What a reader of an untrusted snapshot applies
    /// whatever the sender claimed.
    #[must_use]
    pub fn bounded(self) -> Self {
        let mut cut = self.truncated;
        let mut clip = |text: String, max: usize| {
            if text.chars().count() > max {
                cut = true;
                text.chars().take(max).collect()
            } else {
                text
            }
        };
        let url = clip(self.url, MAX_LABEL_CHARS);
        let title = clip(self.title, MAX_LABEL_CHARS);
        let text = clip(self.text, MAX_TEXT_CHARS);
        let many = self.elements.len() > MAX_ELEMENTS;
        let elements = self
            .elements
            .into_iter()
            .take(MAX_ELEMENTS)
            .map(|element| PageElement {
                id: element.id,
                role: clip(element.role, MAX_LABEL_CHARS),
                name: clip(element.name, MAX_LABEL_CHARS),
            })
            .collect();
        Self {
            url,
            title,
            text,
            elements,
            truncated: cut || many,
        }
    }

    /// This snapshot [`bounded`](Self::bounded), then cut further until
    /// its reply fits one frame: the text halved, and once it's empty the
    /// elements. A page of four-byte characters, or of characters JSON
    /// escapes, can be within the character bounds and past the frame.
    #[must_use]
    pub fn fitted(self) -> Self {
        self.fitted_in(|snapshot| {
            crate::fits_one_frame(&BrowserReply::Page {
                snapshot: snapshot.clone(),
            })
        })
    }

    /// This snapshot [`bounded`](Self::bounded), then cut as
    /// [`fitted`](Self::fitted) cuts it until `fits` says it fits: for a
    /// message that carries a snapshot with more beside it. Past the text
    /// and the elements there's nothing more to cut, and it's given as it
    /// is.
    #[must_use]
    pub fn fitted_in(self, fits: impl Fn(&Self) -> bool) -> Self {
        let mut snapshot = self.bounded();
        while !fits(&snapshot) {
            if snapshot.text.is_empty() && snapshot.elements.is_empty() {
                break;
            }
            snapshot.truncated = true;
            if snapshot.text.is_empty() {
                let keep = snapshot.elements.len() / 2;
                snapshot.elements.truncate(keep);
            } else {
                let keep = snapshot.text.chars().count() / 2;
                snapshot.text = snapshot.text.chars().take(keep).collect();
            }
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn a_secret_is_hidden_wherever_a_page_shows_it_unless_its_too_short_to_hide() {
        let page = PageSnapshot {
            url: String::from("https://a.example/?u=ada@x"),
            title: String::from("ada@x"),
            text: String::from("ada@x and abc and wxyz"),
            elements: vec![PageElement {
                id: 1,
                role: String::from("textbox"),
                name: String::from("ada@x"),
            }],
            truncated: false,
        };
        let shown = page.hiding(&[
            crate::ui::Concealed::new(String::from("ada@x")),
            crate::ui::Concealed::new(String::from("abc")),
            crate::ui::Concealed::new(String::from("wxyz")),
        ]);
        assert_eq!(shown.url, "https://a.example/?u=[hidden]");
        assert_eq!(shown.title, "[hidden]");
        assert_eq!(
            shown.text, "[hidden] and abc and [hidden]",
            "three characters: left; four: hidden"
        );
        assert_eq!(
            shown.elements.first().map(|element| element.name.as_str()),
            Some("[hidden]")
        );
    }

    #[test]
    fn a_snapshot_is_fitted_to_what_carries_it_and_cut_no_further_than_empty() {
        let page = PageSnapshot {
            url: String::from("https://a.example/"),
            title: String::from("A"),
            text: "abcdefgh".repeat(4),
            elements: (0..8)
                .map(|id| PageElement {
                    id,
                    role: String::from("link"),
                    name: format!("link {id}"),
                })
                .collect(),
            truncated: false,
        };
        // Fits as it is: nothing cut.
        assert_eq!(page.clone().fitted_in(|_| true), page);
        // Fits once its text is at most 8 characters: the text is halved,
        // the elements kept.
        let fitted = page.clone().fitted_in(|s| s.text.chars().count() <= 8);
        assert_eq!(fitted.text, "abcdefgh");
        assert_eq!(fitted.elements.len(), 8);
        assert!(fitted.truncated);
        // Fits with at most 2 elements: the text goes first, then halves.
        let fitted = page.clone().fitted_in(|s| s.elements.len() <= 2);
        assert_eq!((fitted.text.as_str(), fitted.elements.len()), ("", 2));
        // Never fits: cut to nothing, and given back rather than looping.
        let fitted = page.fitted_in(|_| false);
        assert!(fitted.text.is_empty() && fitted.elements.is_empty());
        assert_eq!(fitted.url, "https://a.example/", "the page is still named");
    }

    fn wire<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    #[test]
    fn commands_and_replies_have_their_wire_shapes() {
        assert_eq!(
            wire(&BrowserCommand::Open {
                url: String::from("https://example.com/")
            }),
            json!({"command": "open", "url": "https://example.com/"})
        );
        assert_eq!(
            wire(&BrowserCommand::Act {
                element: 3,
                action: PageAction::Key {
                    key: Key::ArrowDown
                },
            }),
            json!({"command": "act", "element": 3, "action": {"action": "key", "key": "arrow_down"}})
        );
        assert_eq!(
            wire(&BrowserCommand::Scroll { down: true }),
            json!({"command": "scroll", "down": true})
        );
        assert_eq!(wire(&BrowserCommand::Close), json!({"command": "close"}));
        assert_eq!(
            wire(&BrowserCommand::Input {
                input: OwnerInput::Click { x: 10, y: 20 }
            }),
            json!({"command": "input", "input": {"input": "click", "x": 10, "y": 20}})
        );
        for (input, shape) in [
            (
                OwnerInput::Type {
                    text: String::from("hi"),
                },
                json!({"input": "type", "text": "hi"}),
            ),
            (
                OwnerInput::Key { key: Key::Tab },
                json!({"input": "key", "key": "tab"}),
            ),
            (
                OwnerInput::Scroll { down: false },
                json!({"input": "scroll", "down": false}),
            ),
        ] {
            assert_eq!(wire(&input), shape);
        }
        let cookie = Cookie {
            name: String::from("sid"),
            value: String::from("s3cret"),
            domain: String::from(".a.example"),
            path: String::from("/"),
            expires: Some(1_900_000_000),
            secure: true,
            http_only: true,
            same_site: Some(SameSite::Lax),
        };
        assert_eq!(
            wire(&BrowserCommand::Cookies { from: 3 }),
            json!({"command": "cookies", "from": 3})
        );
        assert_eq!(
            wire(&BrowserReply::Cookies {
                cookies: vec![cookie.clone()],
                next: None
            }),
            json!({"reply": "cookies", "next": null, "cookies": [{
                "name": "sid", "value": "s3cret", "domain": ".a.example", "path": "/",
                "expires": 1_900_000_000, "secure": true, "http_only": true, "same_site": "lax"
            }]})
        );
        assert_eq!(
            wire(&BrowserCommand::SetCookies {
                cookies: Vec::new()
            }),
            json!({"command": "set_cookies", "cookies": []})
        );
        assert_eq!(
            wire(&BrowserReply::CookiesSet),
            json!({"reply": "cookies_set"})
        );
        let shown = format!("{cookie:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        let negative: Result<OwnerInput, _> =
            serde_json::from_value(json!({"input": "click", "x": -1, "y": 0}));
        assert!(negative.is_err(), "no point left of the viewport");
        assert_eq!(
            wire(&BrowserReply::Failed {
                failure: BrowserFailure::UnknownElement
            }),
            json!({"reply": "failed", "failure": "unknown_element"})
        );
        assert_eq!(wire(&BrowserReply::Closed), json!({"reply": "closed"}));
        let unknown: Result<BrowserCommand, _> =
            serde_json::from_value(json!({"command": "evaluate", "script": "1"}));
        assert!(unknown.is_err(), "no command runs a script");
    }

    #[test]
    fn a_cookie_is_held_to_a_browsers_bounds() {
        let cookie = |name: usize, value: usize, domain: usize, path: usize| Cookie {
            name: "n".repeat(name),
            value: "v".repeat(value),
            domain: "d".repeat(domain),
            path: "p".repeat(path),
            expires: None,
            secure: false,
            http_only: false,
            same_site: None,
        };
        assert!(
            cookie(
                96,
                MAX_COOKIE_BYTES - 96,
                MAX_COOKIE_ATTRIBUTE_BYTES,
                MAX_COOKIE_ATTRIBUTE_BYTES
            )
            .within_bounds()
        );
        assert!(!cookie(96, MAX_COOKIE_BYTES - 95, 1, 1).within_bounds());
        assert!(!cookie(1, 1, MAX_COOKIE_ATTRIBUTE_BYTES + 1, 1).within_bounds());
        assert!(!cookie(1, 1, 1, MAX_COOKIE_ATTRIBUTE_BYTES + 1).within_bounds());
    }

    #[test]
    fn a_snapshot_is_held_to_its_bounds_on_character_boundaries() {
        let element = |id| PageElement {
            id,
            role: String::from("button"),
            name: "é".repeat(MAX_LABEL_CHARS + 5),
        };
        let snapshot = PageSnapshot {
            url: String::from("https://example.com/"),
            title: String::from("Title"),
            text: "ü".repeat(MAX_TEXT_CHARS + 1),
            elements: (0..u32::try_from(MAX_ELEMENTS).unwrap() + 3)
                .map(element)
                .collect(),
            truncated: false,
        }
        .bounded();
        assert_eq!(snapshot.text.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(snapshot.elements.len(), MAX_ELEMENTS);
        assert!(
            snapshot
                .elements
                .iter()
                .all(|e| e.name.chars().count() == MAX_LABEL_CHARS)
        );
        assert!(snapshot.truncated);
        assert_eq!(snapshot.url, "https://example.com/");
        // Within the bounds, nothing changes, and nothing is said to be cut.
        let small = PageSnapshot {
            url: String::from("u"),
            title: "t".repeat(MAX_LABEL_CHARS),
            text: "x".repeat(MAX_TEXT_CHARS),
            elements: vec![element(1)]
                .into_iter()
                .map(|e| PageElement {
                    name: String::from("ok"),
                    ..e
                })
                .collect(),
            truncated: false,
        };
        assert_eq!(small.clone().bounded(), small);
        // Each bound alone sets `truncated`.
        for over in [
            PageSnapshot {
                url: "u".repeat(MAX_LABEL_CHARS + 1),
                ..small.clone()
            },
            PageSnapshot {
                title: "t".repeat(MAX_LABEL_CHARS + 1),
                ..small.clone()
            },
            PageSnapshot {
                text: "x".repeat(MAX_TEXT_CHARS + 1),
                ..small.clone()
            },
            PageSnapshot {
                elements: (0..=u32::try_from(MAX_ELEMENTS).unwrap())
                    .map(|id| PageElement {
                        id,
                        role: String::from("link"),
                        name: String::from("a"),
                    })
                    .collect(),
                ..small.clone()
            },
        ] {
            assert!(over.bounded().truncated);
        }
        // Within the character bounds and still past a frame: cut to fit.
        let escaped = PageSnapshot {
            text: "\u{1}".repeat(MAX_TEXT_CHARS),
            elements: (0..u32::try_from(MAX_ELEMENTS).unwrap())
                .map(|id| PageElement {
                    id,
                    role: "\u{2}".repeat(MAX_LABEL_CHARS),
                    name: "\u{3}".repeat(MAX_LABEL_CHARS),
                })
                .collect(),
            ..small.clone()
        };
        assert!(!crate::fits_one_frame(&BrowserReply::Page {
            snapshot: escaped.clone().bounded()
        }));
        let fitted = escaped.fitted();
        assert!(crate::fits_one_frame(&BrowserReply::Page {
            snapshot: fitted.clone()
        }));
        assert!(fitted.truncated);
        assert!(
            fitted.text.is_empty() && !fitted.elements.is_empty(),
            "text goes first"
        );
        assert_eq!(small.clone().fitted(), small, "what fits is unchanged");
        // A sender's own `truncated` is kept.
        let said = PageSnapshot {
            truncated: true,
            ..small
        };
        assert!(said.bounded().truncated);
    }
}
