//! The local channel's hub: where the broker's ingress meets its
//! sessions.
//!
//! A [`LocalHub`] is shared between the broker's ingress (which queues
//! what the local user sends) and every session's local channel (which
//! receives from the queue and delivers replies back). Replies go to
//! the one *sink* the ingress attached for its connected client; with no
//! client attached, a reply has nowhere to go and the send fails.
//!
//! The hub carries messages only. Identity is not its concern: whatever
//! sender or channel an injected [`Message`] claims, the broker stamps
//! both on receive, as it does for any backend.
//!
//! Every reply passes the hub's [`OutputGate`] before its sink sees it
//!: the sink takes a [`Cleared`] message, which only the gate
//! makes, so no delivery skips it.
//!
//! The hub also numbers the local channel's conversation (WO-026): the
//! owner starts a new one with [`LocalHub::new_conversation`], and each
//! queued message carries the number it was sent in, so a message sent
//! before the owner started a new conversation is never taken as part of
//! it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::gate::{Cleared, OutputGate};
use crate::{Error, Message};

/// Most injected messages the hub holds unreceived. Past it the ingress
/// refuses more: a flood cannot grow broker memory without bound.
pub const MAX_QUEUED_MESSAGES: usize = 64;

/// Largest injected message text, in bytes. Well inside a session
/// frame, so a queued message always fits the reply that delivers it.
pub const MAX_INGRESS_TEXT_BYTES: usize = 16 * 1024;

/// What the ingress tells its client, one frame per event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum IngressEvent {
    /// The injected message is queued: a receive will now deliver it.
    #[serde(rename = "queued")]
    Queued,
    /// The injected message was not queued: the queue is full or the
    /// text is over [`MAX_INGRESS_TEXT_BYTES`].
    #[serde(rename = "refused")]
    Refused,
    /// A reply the agent sent on the local channel.
    #[serde(rename = "reply")]
    Reply {
        /// The reply text.
        text: String,
    },
}

/// Delivers one reply, cleared by the output gate, to the ingress client.
pub type Sink = Box<dyn FnMut(&Cleared) -> Result<(), Error> + Send>;

/// Identifies an attached sink, so only its own connection detaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkId(u64);

#[derive(Default)]
struct HubState {
    /// Each queued message, with the conversation it was sent in.
    inbound: VecDeque<(Message, u64)>,
    conversation: u64,
    sink: Option<(SinkId, Sink)>,
    attached: u64,
    arrivals: usize,
}

/// The hub's state and the gate every reply passes.
struct Shared {
    state: Mutex<HubState>,
    gate: OutputGate,
}

/// The shared queue and reply sink behind the local channel.
#[derive(Clone)]
pub struct LocalHub(Arc<Shared>);

impl std::fmt::Debug for LocalHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        f.debug_struct("LocalHub")
            .field("queued", &state.inbound.len())
            .field("client", &state.sink.is_some())
            .finish()
    }
}

impl LocalHub {
    /// A hub with an empty queue and no client, whose replies pass
    /// `gate`.
    #[must_use]
    pub fn new(gate: OutputGate) -> Self {
        Self(Arc::new(Shared {
            state: Mutex::default(),
            gate,
        }))
    }

    /// The state, even if a holder panicked: the queue and the sink stay
    /// structurally valid across any panic, so poisoning is not fatal.
    fn state(&self) -> MutexGuard<'_, HubState> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The gate every reply passes.
    #[must_use]
    pub fn gate(&self) -> &OutputGate {
        &self.0.gate
    }

    /// Queues an injected message, or refuses it when the queue is full
    /// or the text is over [`MAX_INGRESS_TEXT_BYTES`].
    #[must_use]
    pub fn inject(&self, msg: Message) -> IngressEvent {
        let mut state = self.state();
        if msg.text.len() > MAX_INGRESS_TEXT_BYTES || state.inbound.len() >= MAX_QUEUED_MESSAGES {
            return IngressEvent::Refused;
        }
        let conversation = state.conversation;
        state.inbound.push_back((msg, conversation));
        state.arrivals = state.arrivals.saturating_add(1);
        IngressEvent::Queued
    }

    /// How many messages were queued since the last call, resetting the
    /// count: what the broker's hook dispatcher fires `channel-message`
    /// hooks on. Only [`inject`](Self::inject) counts, and only
    /// what it queued; a reply delivered to the client is not an arrival.
    #[must_use]
    pub fn take_arrivals(&self) -> usize {
        std::mem::take(&mut self.state().arrivals)
    }

    /// Takes the oldest queued message, if any.
    #[must_use]
    pub fn take(&self) -> Option<Message> {
        self.take_in_conversation().map(|(msg, _)| msg)
    }

    /// Takes the oldest queued message, if any, with the number of the
    /// conversation it was sent in.
    #[must_use]
    pub fn take_in_conversation(&self) -> Option<(Message, u64)> {
        self.state().inbound.pop_front()
    }

    /// The number of the conversation now: messages queued from here on
    /// are sent in it.
    #[must_use]
    pub fn conversation(&self) -> u64 {
        self.state().conversation
    }

    /// Starts a new conversation. Messages already queued stay queued, in
    /// the conversation they were sent in.
    pub fn new_conversation(&self) {
        let mut state = self.state();
        state.conversation = state.conversation.wrapping_add(1);
    }

    /// How many messages wait unreceived.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.state().inbound.len()
    }

    /// Attaches `sink` as the reply path, replacing any earlier one.
    #[must_use]
    pub fn attach(&self, sink: Sink) -> SinkId {
        let mut state = self.state();
        state.attached += 1;
        let id = SinkId(state.attached);
        state.sink = Some((id, sink));
        id
    }

    /// Detaches the sink `id`, if it is still the attached one; a newer
    /// client's sink stays.
    pub fn detach(&self, id: SinkId) {
        let mut state = self.state();
        if state
            .sink
            .as_ref()
            .is_some_and(|(current, _)| *current == id)
        {
            state.sink = None;
        }
    }

    /// Delivers a reply to the attached client, in `scope` (the
    /// sending session), after the output gate has judged it whole.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Backend`] when no client is attached, the sink's
    /// own error when delivery fails, or [`Error::Redacted`] when the gate
    /// delivered its notice in place of the reply.
    pub fn deliver(&self, msg: &Message, scope: &str) -> Result<(), Error> {
        // The sink runs under the lock: one delivery at a time, and no
        // detach can land mid-delivery.
        let mut state = self.state();
        let Some((_, sink)) = state.sink.as_mut() else {
            return Err(Error::Backend(String::from(
                "no local ingress client to deliver to",
            )));
        };
        let cleared = self.0.gate.clear(msg, scope);
        let delivered = sink(&cleared);
        drop(state);
        delivered?;
        cleared.redaction().map_or(Ok(()), |(category, event_id)| {
            Err(Error::Redacted {
                category: category.as_str().to_owned(),
                event_id: event_id.clone(),
            })
        })
    }

    /// Forgets `scope`'s gate state: its session ended.
    pub fn end_scope(&self, scope: &str) {
        self.0.gate.end_scope(scope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelId, SenderId};

    #[test]
    fn only_queued_messages_are_arrivals_and_taking_resets_the_count() {
        let hub = LocalHub::new(OutputGate::disabled());
        let msg = |text: &str| Message {
            sender: SenderId(String::from("someone")),
            channel: ChannelId::local(),
            text: text.to_owned(),
        };
        assert_eq!(hub.take_arrivals(), 0);
        assert_eq!(hub.inject(msg("one")), IngressEvent::Queued);
        assert_eq!(hub.inject(msg("two")), IngressEvent::Queued);
        // A refused message did not arrive.
        assert_eq!(
            hub.inject(msg(&"x".repeat(MAX_INGRESS_TEXT_BYTES + 1))),
            IngressEvent::Refused
        );
        // Neither does a reply, nor a receive.
        let _ = hub.deliver(&msg("reply"), "s");
        let _ = hub.take();
        assert_eq!(hub.take_arrivals(), 2);
        assert_eq!(hub.take_arrivals(), 0, "taking resets the count");
        assert_eq!(hub.inject(msg("three")), IngressEvent::Queued);
        assert_eq!(hub.take_arrivals(), 1);
    }

    #[test]
    fn a_queued_message_keeps_the_conversation_it_was_sent_in() {
        let hub = LocalHub::new(OutputGate::disabled());
        assert_eq!(hub.conversation(), 0);
        assert_eq!(hub.inject(msg("before")), IngressEvent::Queued);
        hub.new_conversation();
        assert_eq!(hub.conversation(), 1);
        assert_eq!(hub.inject(msg("after")), IngressEvent::Queued);
        hub.new_conversation();
        // Nothing queued is dropped: each is taken in its own order, with
        // the conversation it was sent in.
        let taken = |hub: &LocalHub| hub.take_in_conversation().map(|(m, n)| (m.text, n));
        assert_eq!(taken(&hub), Some((String::from("before"), 0)));
        assert_eq!(taken(&hub), Some((String::from("after"), 1)));
        assert_eq!(taken(&hub), None);
        assert_eq!(hub.inject(msg("now")), IngressEvent::Queued);
        assert_eq!(hub.take().map(|m| m.text), Some(String::from("now")));
        assert_eq!(hub.conversation(), 2);
    }

    fn msg(text: &str) -> Message {
        Message {
            sender: SenderId(String::from("someone")),
            channel: ChannelId::local(),
            text: text.to_owned(),
        }
    }

    fn recorder() -> (Sink, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let sink: Sink = Box::new(move |m: &Cleared| {
            sink_seen.lock().unwrap().push(m.message().text.clone());
            Ok(())
        });
        (sink, seen)
    }

    #[test]
    fn injected_messages_are_taken_in_order() {
        let hub = LocalHub::new(OutputGate::disabled());
        assert_eq!(hub.take(), None);
        assert_eq!(hub.inject(msg("one")), IngressEvent::Queued);
        assert_eq!(hub.inject(msg("two")), IngressEvent::Queued);
        assert_eq!(hub.queued(), 2);
        assert_eq!(hub.take().map(|m| m.text), Some(String::from("one")));
        assert_eq!(hub.take().map(|m| m.text), Some(String::from("two")));
        assert_eq!(hub.take(), None);
        assert_eq!(hub.queued(), 0);
    }

    #[test]
    fn the_queue_and_the_text_are_bounded() {
        let hub = LocalHub::new(OutputGate::disabled());
        for n in 0..MAX_QUEUED_MESSAGES {
            assert_eq!(hub.inject(msg(&n.to_string())), IngressEvent::Queued, "{n}");
        }
        assert_eq!(hub.inject(msg("one more")), IngressEvent::Refused);
        assert_eq!(hub.queued(), MAX_QUEUED_MESSAGES);
        let _ = hub.take();
        assert_eq!(hub.inject(msg("room again")), IngressEvent::Queued);

        let hub = LocalHub::new(OutputGate::disabled());
        let at_bound = "x".repeat(MAX_INGRESS_TEXT_BYTES);
        assert_eq!(hub.inject(msg(&at_bound)), IngressEvent::Queued);
        assert_eq!(
            hub.inject(msg(&format!("{at_bound}x"))),
            IngressEvent::Refused
        );
        assert_eq!(hub.queued(), 1);
        assert_eq!(MAX_QUEUED_MESSAGES, 64);
        assert_eq!(MAX_INGRESS_TEXT_BYTES, 16 * 1024);
    }

    #[test]
    fn replies_reach_the_attached_client_only() {
        let hub = LocalHub::new(OutputGate::disabled());
        assert!(
            matches!(hub.deliver(&msg("lost"), "s"), Err(Error::Backend(_))),
            "no client, no delivery"
        );
        let (sink, seen) = recorder();
        let first = hub.attach(sink);
        hub.deliver(&msg("hello"), "s").unwrap();
        assert_eq!(*seen.lock().unwrap(), ["hello"]);

        // A newer client replaces the first; the first's late detach
        // leaves the newer one attached.
        let (sink, newer) = recorder();
        let second = hub.attach(sink);
        assert_ne!(first, second);
        hub.detach(first);
        hub.deliver(&msg("to the newer"), "s").unwrap();
        assert_eq!(*newer.lock().unwrap(), ["to the newer"]);
        assert_eq!(*seen.lock().unwrap(), ["hello"]);

        hub.detach(second);
        assert!(hub.deliver(&msg("gone"), "s").is_err());
    }

    /// Flags any text holding `forbidden-words`, counting its calls.
    struct Flags(Mutex<usize>);

    impl crate::Classifier for Flags {
        fn id(&self) -> &'static str {
            "flags@test"
        }

        fn classify(&self, request: &crate::Request<'_>) -> Result<crate::Verdict, String> {
            *self.0.lock().unwrap() += 1;
            Ok(if request.text.contains("forbidden-words") {
                crate::Verdict::Redact {
                    category: crate::Category::new("secret").unwrap(),
                    flagged: Some(String::from("forbidden-words")),
                }
            } else {
                crate::Verdict::Allow
            })
        }
    }

    /// The sink sees only what the gate cleared: the notice in place of
    /// flagged content, which `deliver` reports as redacted, and the
    /// gate's no-retry hold lasts until the scope ends. With no client
    /// nothing is judged at all.
    #[test]
    fn a_gated_hub_delivers_the_notice_and_reports_the_redaction() {
        let flags = Arc::new(Flags(Mutex::new(0)));
        let classifier: Arc<dyn crate::Classifier> = flags.clone();
        let hub = LocalHub::new(OutputGate::new(
            classifier,
            Arc::new(crate::NoAudit),
            Arc::new(crate::NoOverrides),
            crate::gate::DEFAULT_CLASSIFIER_TIMEOUT,
        ));
        assert_eq!(hub.gate().classifier_id(), Some("flags@test"));
        assert!(matches!(
            hub.deliver(&msg("the forbidden-words"), "s"),
            Err(Error::Backend(_))
        ));
        assert_eq!(*flags.0.lock().unwrap(), 0, "no client, nothing judged");

        let (sink, seen) = recorder();
        let _ = hub.attach(sink);
        hub.deliver(&msg("hello"), "s").unwrap();
        let Err(Error::Redacted { category, event_id }) =
            hub.deliver(&msg("the forbidden-words"), "s")
        else {
            unreachable!("flagged content was not reported redacted");
        };
        assert_eq!(category, "secret");
        let shown = crate::notice(&crate::Category::new("secret").unwrap(), &event_id);
        assert_eq!(*seen.lock().unwrap(), ["hello", shown.as_str()]);

        // Reworded, it is held back in its scope only, until that ends.
        assert!(matches!(
            hub.deliver(&msg("Forbidden Words!"), "s"),
            Err(Error::Redacted { .. })
        ));
        hub.deliver(&msg("Forbidden Words!"), "t").unwrap();
        hub.end_scope("s");
        hub.deliver(&msg("Forbidden Words!"), "s").unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 5);
        assert!(
            !seen.iter().any(|text| text.contains("forbidden-words")),
            "{seen:?}"
        );
    }

    #[test]
    fn a_failing_sink_fails_the_delivery() {
        let hub = LocalHub::new(OutputGate::disabled());
        let _ = hub.attach(Box::new(|_: &Cleared| {
            Err(Error::Backend(String::from("client went away")))
        }));
        assert!(
            matches!(hub.deliver(&msg("x"), "s"), Err(Error::Backend(r)) if r == "client went away")
        );
    }

    #[test]
    fn the_hub_debugs_its_shape_not_its_contents() {
        let hub = LocalHub::new(OutputGate::disabled());
        let _ = hub.inject(msg("private"));
        assert_eq!(format!("{hub:?}"), "LocalHub { queued: 1, client: false }");
        let (sink, _) = recorder();
        let _ = hub.attach(sink);
        assert_eq!(format!("{hub:?}"), "LocalHub { queued: 1, client: true }");
    }

    #[test]
    fn ingress_event_wire_forms_are_pinned() {
        for (event, json) in [
            (IngressEvent::Queued, r#"{"event":"queued"}"#),
            (IngressEvent::Refused, r#"{"event":"refused"}"#),
            (
                IngressEvent::Reply {
                    text: String::from("hi"),
                },
                r#"{"event":"reply","text":"hi"}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&event).unwrap(), json);
            assert_eq!(serde_json::from_str::<IngressEvent>(json).unwrap(), event);
        }
    }
}
