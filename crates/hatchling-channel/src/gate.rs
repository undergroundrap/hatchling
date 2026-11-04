//! The output gate: the runtime's last word on what reaches a
//! person.
//!
//! Every message the local hub delivers passes [`OutputGate::clear`]
//! first, whole: nothing streams, so the verdict lands before any byte
//! does. A message the classifier flags is replaced wholesale with the
//! standard [`notice`], never trimmed around the edges and never dropped
//! in silence. The redaction is recorded for the operator through
//! [`Audit`], and the hub's sender learns the category and the event id,
//! never the flagged text.
//!
//! - **Structural.** The hub's sink takes a [`Cleared`] message, and only
//!   this module can make one: there is no way to a person that skips
//!   the gate, and nothing the model emits configures it. The operator
//!   builds the gate at broker start, outside the cell's reach.
//! - **Fail closed.** A classifier that errs, panics or overruns its
//!   [`DEFAULT_CLASSIFIER_TIMEOUT`] redacts, under the distinct
//!   [`FAILED_CATEGORY`], so a broken classifier is visible.
//! - **No retry.** Once a scope (a session) has had something redacted,
//!   its [`normalize`]d form is blocked there for the rest of the scope,
//!   reworded or not, whatever the classifier says next time. Other
//!   scopes are untouched, and benign text that does not contain the
//!   whole flagged form passes.
//! - **Multi-part.** A message is judged alone and joined to the scope's
//!   recent delivered text, so content split across messages is caught
//!   on the message that completes it.
//! - **A backstop, not a strategy.** The gate complements upstream
//!   behaviour; a rising redaction rate is a signal to fix the model
//!   layer, not to tune the filter quieter.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Message;

/// How long a classifier may take on one message before the gate gives up
/// and redacts.
pub const DEFAULT_CLASSIFIER_TIMEOUT: Duration = Duration::from_secs(2);

/// The category a classifier failure redacts under.
pub const FAILED_CATEGORY: &str = "classifier-failure";

/// The shortest normalized form the no-retry list keeps: shorter forms
/// would block benign text that happens to contain them.
pub const MIN_BLOCK_CHARS: usize = 8;

/// How much of a scope's delivered text, in characters, the gate joins a
/// new message to when judging it.
pub const MAX_TAIL_CHARS: usize = 4096;

/// The most scopes the gate tracks at once; past it the oldest is dropped.
pub const MAX_SCOPES: usize = 1024;

/// The longest category name.
pub const MAX_CATEGORY_CHARS: usize = 40;

/// A redaction category: lower-case ASCII letters, digits and `-`, 1 to
/// [`MAX_CATEGORY_CHARS`] long, not starting with `-`. Categories are
/// shown to the operator and the user.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Category(String);

impl Category {
    /// The category `name`, if it is a valid one.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        let valid = !name.is_empty()
            && name.chars().count() <= MAX_CATEGORY_CHARS
            && !name.starts_with('-')
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        valid.then(|| Self(name.to_owned()))
    }

    /// The category's name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A classifier's verdict on one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Deliver it.
    Allow,
    /// Replace it with the notice.
    Redact {
        /// Why, as an operator-visible category.
        category: Category,
        /// The part of the text that was flagged, when the classifier can
        /// say: what the no-retry list blocks. `None` blocks the whole
        /// message's form.
        flagged: Option<String>,
    },
}

/// What a classifier is shown: the text, and cheap metadata.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// The text to judge.
    pub text: &'a str,
    /// The scope (session) it would be delivered in.
    pub scope: &'a str,
    /// How many messages this scope has been judged before.
    pub turn: u64,
}

/// A pluggable output classifier.
pub trait Classifier: Send + Sync {
    /// The classifier's id, as the audit log records it.
    fn id(&self) -> &str;

    /// Judges one message.
    ///
    /// # Errors
    ///
    /// Returns why it could not judge: the gate then redacts.
    fn classify(&self, request: &Request<'_>) -> Result<Verdict, String>;
}

/// How a classifier failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// It returned an error.
    Error,
    /// It panicked.
    Panic,
    /// It did not answer in time.
    Timeout,
}

/// Why a message was redacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The classifier flagged it.
    Flagged,
    /// It contains content already redacted in this scope.
    NoRetry,
    /// The classifier could not judge it.
    ClassifierFailed(Failure),
}

/// One redaction, as the gate reports it to the [`Audit`]. It carries the
/// text so the audit can hash it (and keep it, if the operator opted in);
/// the audit decides what is written.
#[derive(Debug, Clone, Copy)]
pub struct Event<'a> {
    /// The redaction's id: what the user and the sender are told.
    pub event_id: &'a str,
    /// When, in Unix seconds.
    pub at: u64,
    /// The scope (session) it happened in.
    pub scope: &'a str,
    /// The message's sender stamp.
    pub sender: &'a str,
    /// The classifier's id.
    pub classifier: &'a str,
    /// The category.
    pub category: &'a Category,
    /// Why.
    pub reason: Reason,
    /// The message that was not delivered.
    pub text: &'a str,
    /// The normalized flagged form, when there is one.
    pub flagged: Option<&'a str>,
}

/// Where the gate records what it did, for the operator.
pub trait Audit: Send + Sync {
    /// A message was redacted.
    fn redacted(&self, event: &Event<'_>);

    /// A message the classifier flagged was delivered, because the
    /// operator overrode that verdict in this scope.
    fn overridden(&self, event: &Event<'_>);
}

/// The operator's overrides: verdicts marked wrong, per scope.
pub trait Overrides: Send + Sync {
    /// Whether the operator overrode `category` for the normalized form
    /// `flagged` in `scope`.
    fn allows(&self, scope: &str, category: &Category, flagged: &str) -> bool;
}

/// No audit: for a gate the operator disabled, and for tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAudit;

impl Audit for NoAudit {
    fn redacted(&self, _event: &Event<'_>) {}

    fn overridden(&self, _event: &Event<'_>) {}
}

/// No overrides.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOverrides;

impl Overrides for NoOverrides {
    fn allows(&self, _scope: &str, _category: &Category, _flagged: &str) -> bool {
        false
    }
}

/// `text`'s comparable form: its letters and digits, lower-cased, with
/// case, whitespace and punctuation folded away. "Hunter 2!" and
/// "hunter2" are the same form.
#[must_use]
pub fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// What the user sees in place of a redacted message: that it happened,
/// why in one word, and the event id the operator can look up. Never the
/// flagged content, never the classifier's reasoning.
#[must_use]
pub fn notice(category: &Category, event_id: &str) -> String {
    format!(
        "[A safety filter redacted this response (category: {}). The operator can review it as event {event_id}.]",
        category.as_str()
    )
}

/// A message the gate has cleared for delivery: the original, or the
/// notice that replaced it. Only [`OutputGate::clear`] makes one.
#[derive(Debug)]
pub struct Cleared {
    message: Message,
    redaction: Option<(Category, String)>,
    origin: Origin,
}

/// Where a delivered message came from, as its deliverer says: never read
/// from the message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origin {
    /// The run that wrote it, by its cell id, when an agent run did.
    pub run: Option<String>,
    /// Whether it's the deliverer's own notice, in its own words, rather
    /// than what a run wrote.
    pub notice: bool,
}

impl Cleared {
    /// The message to deliver.
    #[must_use]
    pub const fn message(&self) -> &Message {
        &self.message
    }

    /// The category and event id, if this is a redaction notice.
    #[must_use]
    pub const fn redaction(&self) -> Option<&(Category, String)> {
        self.redaction.as_ref()
    }

    /// Where the message came from, as its deliverer said.
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The cleared message, said to come from `origin`.
    #[must_use]
    pub fn from_source(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }
}

/// One scope's state: what it may not say again, and what it said.
#[derive(Debug, Default)]
struct Scope {
    blocked: Vec<(Category, String)>,
    tail: String,
    turns: u64,
    born: u64,
}

/// The gate's mutable state.
#[derive(Debug, Default)]
struct State {
    scopes: HashMap<String, Scope>,
    created: u64,
}

/// The output gate. Built once by the operator's configuration, at broker
/// start; never by anything the cell sends.
pub struct OutputGate {
    classifier: Option<Arc<dyn Classifier>>,
    audit: Arc<dyn Audit>,
    overrides: Arc<dyn Overrides>,
    timeout: Duration,
    state: Mutex<State>,
}

impl std::fmt::Debug for OutputGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputGate")
            .field("classifier", &self.classifier_id())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Unix seconds now.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A fresh redaction event id: `red-<unix seconds>-<pid>-<sequence>`.
fn event_id(at: u64) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "red-{at}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// `text`'s last `max` characters.
fn last_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(max)).collect()
}

impl OutputGate {
    /// A gate judging every message with `classifier`, waiting at most
    /// `timeout`, recording to `audit` and honouring `overrides`.
    #[must_use]
    pub fn new(
        classifier: Arc<dyn Classifier>,
        audit: Arc<dyn Audit>,
        overrides: Arc<dyn Overrides>,
        timeout: Duration,
    ) -> Self {
        Self {
            classifier: Some(classifier),
            audit,
            overrides,
            timeout,
            state: Mutex::default(),
        }
    }

    /// A gate the operator turned off: every message is delivered as it
    /// is, and nothing is recorded (there is nothing to record). The
    /// choice itself is logged where the gate is configured.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            classifier: None,
            audit: Arc::new(NoAudit),
            overrides: Arc::new(NoOverrides),
            timeout: DEFAULT_CLASSIFIER_TIMEOUT,
            state: Mutex::default(),
        }
    }

    /// The classifier's id, or `None` for a disabled gate.
    #[must_use]
    pub fn classifier_id(&self) -> Option<&str> {
        self.classifier.as_deref().map(Classifier::id)
    }

    /// Forgets `scope`: its session ended, and its no-retry list goes
    /// with it.
    pub fn end_scope(&self, scope: &str) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .scopes
            .remove(scope);
    }

    /// Judges `message` for delivery in `scope`: the message itself, or
    /// the notice in its place.
    #[must_use]
    pub fn clear(&self, message: &Message, scope: &str) -> Cleared {
        let Some(classifier) = &self.classifier else {
            return Cleared {
                message: message.clone(),
                redaction: None,
                origin: Origin::default(),
            };
        };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let held = Self::scope(&mut state, scope);
        let turn = held.turns;
        held.turns = turn.saturating_add(1);
        let overridden =
            |category: &Category, form: &str| self.overrides.allows(scope, category, form);
        let judged = Self::blocked_form(held, &message.text, overridden).map_or_else(
            || self.judge(classifier, &message.text, &held.tail, scope, turn),
            |(category, form)| Judged::Redact(category, Some(form), Reason::NoRetry),
        );
        let Judged::Redact(category, flagged, reason) = judged else {
            held.tail = last_chars(&format!("{}{}", held.tail, message.text), MAX_TAIL_CHARS);
            drop(state);
            return Cleared {
                message: message.clone(),
                redaction: None,
                origin: Origin::default(),
            };
        };
        let form = flagged.unwrap_or_else(|| normalize(&message.text));
        let overridden =
            reason == Reason::Flagged && self.overrides.allows(scope, &category, &form);
        // An overridden verdict is delivered on the operator's word, and kept
        // out of the tail, so it does not trip the messages after it.
        if !overridden
            && reason == Reason::Flagged
            && form.chars().count() >= MIN_BLOCK_CHARS
            && !held.blocked.iter().any(|(_, kept)| *kept == form)
        {
            held.blocked.push((category.clone(), form.clone()));
        }
        drop(state);
        let at = now();
        let id = event_id(at);
        let failed = matches!(reason, Reason::ClassifierFailed(_));
        let event = Event {
            event_id: &id,
            at,
            scope,
            sender: &message.sender.0,
            classifier: classifier.id(),
            category: &category,
            reason,
            text: &message.text,
            flagged: (!failed).then_some(form.as_str()),
        };
        if overridden {
            self.audit.overridden(&event);
            return Cleared {
                message: message.clone(),
                redaction: None,
                origin: Origin::default(),
            };
        }
        self.audit.redacted(&event);
        Cleared {
            message: Message {
                sender: message.sender.clone(),
                channel: message.channel.clone(),
                text: notice(&category, &id),
            },
            redaction: Some((category, id)),
            origin: Origin::default(),
        }
    }

    /// The blocked form `text` re-emits in this scope, if any: within the
    /// text itself, or completed across the end of the scope's delivered
    /// tail (but not lying wholly inside what was already delivered). A
    /// form the operator has since `overridden` is no longer blocked.
    fn blocked_form(
        held: &Scope,
        text: &str,
        overridden: impl Fn(&Category, &str) -> bool,
    ) -> Option<(Category, String)> {
        let alone = normalize(text);
        let tail = normalize(&held.tail);
        let joined = format!("{tail}{alone}");
        held.blocked
            .iter()
            .find(|(category, form)| {
                (alone.contains(form.as_str())
                    || (joined.contains(form.as_str()) && !tail.contains(form.as_str())))
                    && !overridden(category, form)
            })
            .cloned()
    }

    /// The scope's state, made if new; the oldest scope goes when there are
    /// too many.
    fn scope<'s>(state: &'s mut State, scope: &str) -> &'s mut Scope {
        if !state.scopes.contains_key(scope) && state.scopes.len() >= MAX_SCOPES {
            if let Some(oldest) = state
                .scopes
                .iter()
                .min_by_key(|(_, held)| held.born)
                .map(|(name, _)| name.clone())
            {
                state.scopes.remove(&oldest);
            }
        }
        let born = state.created;
        state.created = born.saturating_add(1);
        state
            .scopes
            .entry(scope.to_owned())
            .or_insert_with(|| Scope {
                born,
                ..Scope::default()
            })
    }

    /// The classifier's judgement of `text` alone, then joined straight
    /// onto the scope's recent `tail`, as a reader would see them: content
    /// split across messages has no separator to hide behind.
    fn judge(
        &self,
        classifier: &Arc<dyn Classifier>,
        text: &str,
        tail: &str,
        scope: &str,
        turn: u64,
    ) -> Judged {
        let alone = self.run(classifier, text, scope, turn);
        if !matches!(alone, Judged::Allow) || tail.is_empty() {
            return alone;
        }
        self.run(classifier, &format!("{tail}{text}"), scope, turn)
    }

    /// One classifier run, bounded by the gate's timeout, panics caught.
    fn run(&self, classifier: &Arc<dyn Classifier>, text: &str, scope: &str, turn: u64) -> Judged {
        let failed = |failure| {
            Category::new(FAILED_CATEGORY).map_or(Judged::Allow, |category| {
                Judged::Redact(category, None, Reason::ClassifierFailed(failure))
            })
        };
        let (sender, receiver) = mpsc::channel();
        let classifier = Arc::clone(classifier);
        let (text, scope) = (text.to_owned(), scope.to_owned());
        let spawned = std::thread::Builder::new()
            .name(String::from("output-gate"))
            .spawn(move || {
                let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    classifier.classify(&Request {
                        text: &text,
                        scope: &scope,
                        turn,
                    })
                }));
                let _ = sender.send(outcome);
            });
        if spawned.is_err() {
            return failed(Failure::Error);
        }
        match receiver.recv_timeout(self.timeout) {
            Ok(Ok(Ok(Verdict::Allow))) => Judged::Allow,
            Ok(Ok(Ok(Verdict::Redact { category, flagged }))) => {
                Judged::Redact(category, flagged.as_deref().map(normalize), Reason::Flagged)
            }
            Ok(Ok(Err(_))) => failed(Failure::Error),
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => failed(Failure::Panic),
            Err(RecvTimeoutError::Timeout) => failed(Failure::Timeout),
        }
    }
}

/// A judgement, before the scope's state is updated.
#[derive(Debug)]
enum Judged {
    Allow,
    /// The category, the normalized flagged form if known, and why.
    Redact(Category, Option<String>, Reason),
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::{ChannelId, SenderId};

    /// Flags any text containing one of its phrases, exactly as written
    /// (case and punctuation included): a rewording is left to no-retry.
    struct Phrases {
        rules: Vec<(&'static str, &'static str)>,
        seen: Mutex<Vec<(String, String, u64)>>,
    }

    impl Phrases {
        fn new(rules: &[(&'static str, &'static str)]) -> Self {
            Self {
                rules: rules.to_vec(),
                seen: Mutex::default(),
            }
        }
    }

    impl Classifier for Phrases {
        fn id(&self) -> &'static str {
            "phrases@test"
        }

        fn classify(&self, request: &Request<'_>) -> Result<Verdict, String> {
            self.seen.lock().unwrap().push((
                request.text.to_owned(),
                request.scope.to_owned(),
                request.turn,
            ));
            Ok(self
                .rules
                .iter()
                .find(|(phrase, _)| request.text.contains(phrase))
                .map_or(Verdict::Allow, |(phrase, category)| Verdict::Redact {
                    category: Category::new(category).unwrap(),
                    flagged: Some((*phrase).to_owned()),
                }))
        }
    }

    /// Fails the way it is told, the first `times` times, then allows.
    struct Broken {
        failure: Failure,
        times: Mutex<usize>,
    }

    impl Classifier for Broken {
        fn id(&self) -> &'static str {
            "broken@test"
        }

        fn classify(&self, _request: &Request<'_>) -> Result<Verdict, String> {
            let mut times = self.times.lock().unwrap();
            if *times == 0 {
                return Ok(Verdict::Allow);
            }
            *times -= 1;
            drop(times);
            match self.failure {
                Failure::Error => Err(String::from("no verdict")),
                Failure::Panic => unreachable!("the classifier broke"),
                Failure::Timeout => {
                    std::thread::sleep(Duration::from_millis(400));
                    Ok(Verdict::Allow)
                }
            }
        }
    }

    /// One recorded audit event, owned.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Recorded {
        overridden: bool,
        event_id: String,
        scope: String,
        sender: String,
        classifier: String,
        category: String,
        reason: Reason,
        text: String,
        flagged: Option<String>,
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Recorded>>);

    impl Recorder {
        fn push(&self, overridden: bool, event: &Event<'_>) {
            self.0.lock().unwrap().push(Recorded {
                overridden,
                event_id: event.event_id.to_owned(),
                scope: event.scope.to_owned(),
                sender: event.sender.to_owned(),
                classifier: event.classifier.to_owned(),
                category: event.category.as_str().to_owned(),
                reason: event.reason,
                text: event.text.to_owned(),
                flagged: event.flagged.map(str::to_owned),
            });
        }

        fn events(&self) -> Vec<Recorded> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Audit for Recorder {
        fn redacted(&self, event: &Event<'_>) {
            self.push(false, event);
        }

        fn overridden(&self, event: &Event<'_>) {
            self.push(true, event);
        }
    }

    /// Overrides for exactly the (scope, category, form) triples it holds.
    #[derive(Default)]
    struct Allowed(Mutex<HashSet<(String, String, String)>>);

    impl Allowed {
        fn add(&self, scope: &str, category: &str, form: &str) {
            self.0
                .lock()
                .unwrap()
                .insert((scope.to_owned(), category.to_owned(), form.to_owned()));
        }
    }

    impl Overrides for Allowed {
        fn allows(&self, scope: &str, category: &Category, flagged: &str) -> bool {
            self.0.lock().unwrap().contains(&(
                scope.to_owned(),
                category.as_str().to_owned(),
                flagged.to_owned(),
            ))
        }
    }

    fn msg(text: &str) -> Message {
        Message {
            sender: SenderId(String::from("uid:1000")),
            channel: ChannelId::local(),
            text: text.to_owned(),
        }
    }

    /// A gate over `classifier`, its audit and its overrides.
    fn gate(
        classifier: Arc<dyn Classifier>,
        timeout: Duration,
    ) -> (OutputGate, Arc<Recorder>, Arc<Allowed>) {
        let (audit, allowed) = (Arc::new(Recorder::default()), Arc::new(Allowed::default()));
        let audit_dyn: Arc<dyn Audit> = audit.clone();
        let allowed_dyn: Arc<dyn Overrides> = allowed.clone();
        (
            OutputGate::new(classifier, audit_dyn, allowed_dyn, timeout),
            audit,
            allowed,
        )
    }

    fn phrases(
        rules: &[(&'static str, &'static str)],
    ) -> (OutputGate, Arc<Recorder>, Arc<Allowed>) {
        gate(Arc::new(Phrases::new(rules)), DEFAULT_CLASSIFIER_TIMEOUT)
    }

    fn passed(gate: &OutputGate, text: &str, scope: &str) -> bool {
        let cleared = gate.clear(&msg(text), scope);
        let through = cleared.redaction().is_none();
        assert_eq!(
            cleared.message().text == text,
            through,
            "{text:?}: the message or the notice"
        );
        through
    }

    #[test]
    fn a_category_is_a_short_lower_case_name() {
        for name in [
            "secret",
            "private-key",
            "c2",
            "x",
            &"a".repeat(MAX_CATEGORY_CHARS),
        ] {
            assert_eq!(
                Category::new(name)
                    .map(|c| c.as_str().to_owned())
                    .as_deref(),
                Some(name),
                "{name}"
            );
        }
        for name in [
            "",
            "-x",
            "Secret",
            "a b",
            "a_b",
            "ünïcode",
            &"a".repeat(MAX_CATEGORY_CHARS + 1),
        ] {
            assert_eq!(Category::new(name), None, "{name}");
        }
        assert_eq!(FAILED_CATEGORY, "classifier-failure");
        assert!(Category::new(FAILED_CATEGORY).is_some());
    }

    #[test]
    fn the_normal_form_folds_case_whitespace_and_punctuation() {
        for (text, form) in [
            ("Hunter 2!", "hunter2"),
            ("h-u-n-t-e-r\t2", "hunter2"),
            ("\u{c0}B, c\u{e9}", "\u{e0}bc\u{e9}"),
            ("  ,.;!?", ""),
            ("", ""),
        ] {
            assert_eq!(normalize(text), form, "{text:?}");
        }
    }

    #[test]
    fn a_disabled_gate_delivers_everything_and_judges_nothing() {
        let gate = OutputGate::disabled();
        assert_eq!(gate.classifier_id(), None);
        let cleared = gate.clear(&msg("anything at all"), "s");
        assert_eq!(
            (cleared.message(), cleared.redaction()),
            (&msg("anything at all"), None)
        );
        gate.end_scope("s");
        assert_eq!(
            format!("{gate:?}"),
            "OutputGate { classifier: None, timeout: 2s, .. }"
        );
    }

    #[test]
    fn flagged_output_is_replaced_whole_by_an_honest_notice_and_recorded() {
        let (gate, audit, _) = phrases(&[("ALPHA-7-SEVEN", "launch-codes")]);
        assert_eq!(gate.classifier_id(), Some("phrases@test"));
        let original = msg("The code is ALPHA-7-SEVEN, use it well.");
        let cleared = gate.clear(&original, "session:a");
        let (category, event_id) = cleared.redaction().cloned().unwrap();
        assert_eq!(category.as_str(), "launch-codes");
        assert_eq!(
            cleared.message().text,
            format!(
                "[A safety filter redacted this response (category: launch-codes). The operator can review it as event {event_id}.]"
            )
        );
        assert_eq!(cleared.message().text, notice(&category, &event_id));
        assert!(
            !cleared.message().text.contains("ALPHA")
                && !cleared.message().text.contains("use it well")
        );
        assert_eq!(
            (&cleared.message().sender, &cleared.message().channel),
            (&original.sender, &original.channel)
        );
        let parts: Vec<&str> = event_id.split('-').collect();
        assert_eq!(parts.first(), Some(&"red"));
        assert_eq!(
            parts.get(2).copied(),
            Some(std::process::id().to_string().as_str())
        );
        assert_eq!(
            audit.events(),
            [Recorded {
                overridden: false,
                event_id: event_id.clone(),
                scope: String::from("session:a"),
                sender: String::from("uid:1000"),
                classifier: String::from("phrases@test"),
                category: String::from("launch-codes"),
                reason: Reason::Flagged,
                text: original.text.clone(),
                flagged: Some(String::from("alpha7seven")),
            }]
        );
        let again = gate.clear(&original, "session:b");
        assert_ne!(
            again.redaction().map(|(_, id)| id.clone()),
            Some(event_id),
            "each event its own id"
        );
    }

    #[test]
    fn redacted_content_cannot_come_back_reworded_in_its_session_only() {
        let (gate, audit, _) = phrases(&[("ALPHA-7-SEVEN", "launch-codes")]);
        assert!(!passed(&gate, "ALPHA-7-SEVEN", "s1"));
        // The classifier would let these through; no-retry does not.
        for reworded in [
            "alpha 7 seven",
            "Alpha-7-Seven!",
            "a l p h a 7 s e v e n",
            "so: alpha7seven.",
        ] {
            assert!(!passed(&gate, reworded, "s1"), "{reworded}");
        }
        let last = audit.events().last().cloned().unwrap();
        assert_eq!(
            (last.reason, last.flagged.as_deref()),
            (Reason::NoRetry, Some("alpha7seven"))
        );
        // Benign text that shares words passes, and so does the same text
        // elsewhere, and in the scope once it ends.
        for benign in ["alpha seven", "the seventh alpha", "7 alpha", "alpha 7"] {
            assert!(passed(&gate, benign, "s1"), "{benign}");
        }
        assert!(
            passed(&gate, "alpha 7 seven", "s2"),
            "another session is untouched"
        );
        gate.end_scope("s1");
        assert!(passed(&gate, "alpha 7 seven", "s1"), "the session ended");
    }

    #[test]
    fn a_short_flagged_form_is_not_kept_against_benign_text() {
        let (gate, _, _) = phrases(&[("SEVEN-77", "short"), ("EIGHT-888", "eight")]);
        assert_eq!(normalize("SEVEN-77").chars().count(), MIN_BLOCK_CHARS - 1);
        assert_eq!(normalize("EIGHT-888").chars().count(), MIN_BLOCK_CHARS);
        assert!(!passed(&gate, "SEVEN-77", "s"));
        assert!(passed(&gate, "seven 77", "s"), "under the bound: not kept");
        assert!(!passed(&gate, "EIGHT-888", "s"));
        assert!(!passed(&gate, "eight 888", "s"), "at the bound: kept");
        assert_eq!(MIN_BLOCK_CHARS, 8);
    }

    #[test]
    fn content_split_across_messages_is_caught_on_the_part_that_completes_it() {
        let (gate, audit, _) = phrases(&[("TOPSECRET", "split")]);
        assert!(passed(&gate, "TOP", "s"));
        assert!(
            !passed(&gate, "SECRET", "s"),
            "joined to what went before, it is flagged"
        );
        assert_eq!(
            audit.events().last().map(|e| e.text.clone()).as_deref(),
            Some("SECRET")
        );
        // A blocked form completed across the boundary is caught too.
        assert!(passed(&gate, "top", "s"));
        assert!(!passed(&gate, "secret now", "s"));
        assert_eq!(
            audit.events().last().map(|e| e.reason),
            Some(Reason::NoRetry)
        );
        // But a form lying wholly in what was already delivered blocks
        // nothing after it.
        let (gate, _, _) = phrases(&[("BRAVO TWO TWO", "bravo")]);
        assert!(passed(&gate, "bravo two two", "t"));
        assert!(!passed(&gate, "BRAVO TWO TWO", "t"));
        assert!(
            passed(&gate, "thanks, all done", "t"),
            "the delivered tail does not trip it"
        );
    }

    #[test]
    fn the_classifier_sees_the_text_its_scope_and_its_turn() {
        let classifier = Arc::new(Phrases::new(&[]));
        let (gate, _, _) = gate(classifier.clone(), DEFAULT_CLASSIFIER_TIMEOUT);
        assert!(passed(&gate, "one", "s"));
        assert!(passed(&gate, "two", "s"));
        assert!(passed(&gate, "elsewhere", "t"));
        assert_eq!(
            *classifier.seen.lock().unwrap(),
            [
                (String::from("one"), String::from("s"), 0),
                (String::from("two"), String::from("s"), 1),
                (String::from("onetwo"), String::from("s"), 1),
                (String::from("elsewhere"), String::from("t"), 0),
            ]
        );
    }

    #[test]
    fn a_broken_classifier_fails_closed_and_says_so() {
        for (failure, timeout) in [
            (Failure::Error, DEFAULT_CLASSIFIER_TIMEOUT),
            (Failure::Panic, DEFAULT_CLASSIFIER_TIMEOUT),
            (Failure::Timeout, Duration::from_millis(50)),
        ] {
            let broken = Arc::new(Broken {
                failure,
                times: Mutex::new(1),
            });
            let (gate, audit, _) = gate(broken, timeout);
            let cleared = gate.clear(&msg("an ordinary reply"), "s");
            let (category, event_id) = cleared.redaction().cloned().unwrap();
            assert_eq!(category.as_str(), FAILED_CATEGORY, "{failure:?}");
            assert_eq!(cleared.message().text, notice(&category, &event_id));
            let recorded = audit.events();
            assert_eq!(recorded.len(), 1);
            let event = recorded.first().unwrap();
            assert_eq!(
                (
                    event.reason,
                    event.flagged.as_deref(),
                    event.classifier.as_str()
                ),
                (Reason::ClassifierFailed(failure), None, "broken@test")
            );
            // A failure blocks nothing: once the classifier works, the same
            // text goes through.
            assert!(passed(&gate, "an ordinary reply", "s"), "{failure:?}");
        }
    }

    #[test]
    fn an_override_lets_that_content_through_in_its_scope_and_says_so() {
        let (gate, audit, allowed) = phrases(&[("ALPHA-7-SEVEN", "launch-codes")]);
        assert!(!passed(&gate, "ALPHA-7-SEVEN", "s"));
        assert!(
            !passed(&gate, "alpha 7 seven", "s"),
            "blocked before the override"
        );
        allowed.add("s", "launch-codes", "alpha7seven");
        assert!(
            passed(&gate, "ALPHA-7-SEVEN", "s"),
            "the classifier's verdict is overridden"
        );
        assert!(
            passed(&gate, "alpha 7 seven", "s"),
            "and so is the no-retry block"
        );
        assert!(
            passed(&gate, "and then something else", "s"),
            "delivered content stays out of the tail"
        );
        assert!(
            !passed(&gate, "ALPHA-7-SEVEN", "other"),
            "only in its own scope"
        );
        let overridden: Vec<bool> = audit.events().iter().map(|e| e.overridden).collect();
        assert_eq!(overridden, [false, false, true, false]);
    }

    #[test]
    fn the_oldest_scope_goes_when_too_many_are_held() {
        let (gate, _, _) = phrases(&[("ALPHA-7-SEVEN", "launch-codes")]);
        assert!(!passed(&gate, "ALPHA-7-SEVEN", "first"));
        for n in 0..MAX_SCOPES - 1 {
            let scope = format!("scope-{n}");
            assert!(passed(&gate, "hello", &scope));
        }
        assert!(
            !passed(&gate, "alpha 7 seven", "first"),
            "still held at the bound"
        );
        assert!(passed(&gate, "hello", "one-more"));
        assert!(
            passed(&gate, "alpha 7 seven", "first"),
            "the oldest scope was dropped"
        );
        assert_eq!(MAX_SCOPES, 1024);
    }

    #[test]
    fn a_scope_keeps_only_the_end_of_what_it_delivered() {
        let (gate, _, _) = phrases(&[]);
        let long = "x".repeat(MAX_TAIL_CHARS + 10);
        assert!(passed(&gate, &long, "s"));
        let tail = gate
            .state
            .lock()
            .unwrap()
            .scopes
            .get("s")
            .unwrap()
            .tail
            .clone();
        assert_eq!(tail.chars().count(), MAX_TAIL_CHARS);
        assert_eq!(last_chars("abcdef", 3), "def");
        assert_eq!(last_chars("ab", 3), "ab");
        assert_eq!(MAX_TAIL_CHARS, 4096);
        assert_eq!(DEFAULT_CLASSIFIER_TIMEOUT, Duration::from_secs(2));
    }

    /// The pieces `clear` is made of, each on its own: the no-op audit
    /// and overrides, event ids, the no-retry match, scope upkeep, and the
    /// two-step judgement.
    #[test]
    fn the_gates_parts_each_do_their_one_thing() {
        let category = Category::new("launch-codes").unwrap();
        let event = Event {
            event_id: "red-1-2-3",
            at: 1,
            scope: "s",
            sender: "uid:1",
            classifier: "c",
            category: &category,
            reason: Reason::Flagged,
            text: "t",
            flagged: None,
        };
        NoAudit.redacted(&event);
        NoAudit.overridden(&event);
        assert!(!NoOverrides.allows("s", &category, "anything"));

        let (first, second) = (event_id(7), event_id(7));
        let prefix = format!("red-7-{}-", std::process::id());
        assert!(
            first.starts_with(&prefix) && second.starts_with(&prefix),
            "{first}"
        );
        assert_ne!(first, second, "every event id is fresh");

        let form = String::from("alpha7seven");
        let held = Scope {
            blocked: vec![(category.clone(), form.clone())],
            tail: String::from("we said alpha"),
            ..Scope::default()
        };
        let never = |_: &Category, _: &str| false;
        let found = Some((category.clone(), form));
        assert_eq!(
            OutputGate::blocked_form(&held, "ALPHA 7 seven!", never),
            found
        );
        assert_eq!(
            OutputGate::blocked_form(&held, "-7-SEVEN", never),
            found,
            "completed across the tail"
        );
        assert_eq!(OutputGate::blocked_form(&held, "nothing", never), None);
        assert_eq!(
            OutputGate::blocked_form(&held, "alpha7seven", |_: &Category, kept: &str| kept
                == "alpha7seven"),
            None,
            "an overridden form is no longer blocked"
        );
        let said = Scope {
            tail: String::from("alpha7seven, said already"),
            ..held
        };
        assert_eq!(
            OutputGate::blocked_form(&said, " and more", never),
            None,
            "wholly in the tail"
        );

        let mut state = State::default();
        OutputGate::scope(&mut state, "a").turns = 5;
        assert_eq!(
            OutputGate::scope(&mut state, "a").turns,
            5,
            "an existing scope is kept"
        );
        assert_eq!(OutputGate::scope(&mut state, "b").born, 2);
        assert_eq!((state.scopes.len(), state.created), (2, 3));

        let (gate, _, _) = phrases(&[("ALPHA", "launch-codes")]);
        let classifier: Arc<dyn Classifier> = Arc::new(Phrases::new(&[("ALPHA", "launch-codes")]));
        assert!(matches!(
            gate.judge(&classifier, "fine", "", "s", 0),
            Judged::Allow
        ));
        assert!(matches!(
            gate.judge(&classifier, "PHA", "AL", "s", 0),
            Judged::Redact(_, Some(form), Reason::Flagged) if form == "alpha"
        ));
        assert!(matches!(
            gate.judge(&classifier, "ALPHA", "", "s", 0),
            Judged::Redact(_, Some(_), Reason::Flagged)
        ));
    }

    /// A redaction is stamped with the clock: `now` is the current Unix
    /// second, and the event id carries the second the redaction happened.
    #[test]
    fn a_redaction_is_stamped_with_the_time_it_happened() {
        let seconds = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };
        let before = seconds();
        let stamped = now();
        let (gate, _, _) = phrases(&[("ALPHA-7-SEVEN", "launch-codes")]);
        let cleared = gate.clear(&msg("ALPHA-7-SEVEN"), "s");
        let after = seconds();
        assert!((before..=after).contains(&stamped), "{stamped}");
        let (_, id) = cleared.redaction().cloned().unwrap();
        let at: u64 = id.split('-').nth(1).unwrap().parse().unwrap();
        assert!((before..=after).contains(&at), "{id}");
    }

    /// Every distinct flagged form in a scope is held back, each kept once:
    /// a second form is blocked as surely as the first.
    #[test]
    fn every_distinct_flagged_form_in_a_scope_is_kept_once() {
        let (gate, audit, _) = phrases(&[
            ("ALPHA-7-SEVEN", "launch-codes"),
            ("BRAVO-8-EIGHT", "launch-codes"),
        ]);
        for text in ["ALPHA-7-SEVEN", "BRAVO-8-EIGHT", "ALPHA-7-SEVEN"] {
            assert!(gate.clear(&msg(text), "s").redaction().is_some(), "{text}");
        }
        let blocked: Vec<String> = gate
            .state
            .lock()
            .unwrap()
            .scopes
            .get("s")
            .unwrap()
            .blocked
            .iter()
            .map(|(_, form)| form.clone())
            .collect();
        assert_eq!(blocked, ["alpha7seven", "bravo8eight"]);
        assert!(
            gate.clear(&msg("bravo 8 eight"), "s").redaction().is_some(),
            "the second form, reworded, is held back too"
        );
        assert_eq!(
            audit.events().last().map(|event| event.reason),
            Some(Reason::NoRetry)
        );
    }
}
