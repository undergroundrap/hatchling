//! Chat runs: each message the local channel queues is one agent run.
//!
//! A message the owner sends on the local channel, through the API or
//! the ingress, waits in the hub until something takes it. [`Chat`]
//! takes the oldest, starts a run cell with its text as the task, and
//! delivers the run's answer back on the local channel, where the API's
//! stream and the ingress client show it. One run at a time: the next
//! message waits for the answer to the last.
//!
//! A chat run is a scheduled run in every way but its trigger: the same
//! launcher, the same fresh cell, the same scoped session. Its grants are
//! the local API caller's (`HATCHLING_API_GRANTS`), because only the
//! owner can queue a message: the API and the ingress both check the
//! peer first. Without `channel-send:local` among them there is nowhere
//! to answer, and the broker starts no chat.
//!
//! What the person sees when a run fails is a fixed sentence; why it
//! failed stays in the broker's log.
//!
//! The chat keeps the conversation (WO-026), and gives each run its last
//! exchanges ([`hatchling_core::conversation`]): the message it started
//! a run for, and the answer, once the person was shown that answer as
//! written. A failed run, a fixed sentence, and an answer the output gate
//! blocked or replaced are never given back. The record is the broker's:
//! a run can't write to it. It lives in the broker's memory, and, when the
//! operator gives it a store (`HATCHLING_CONVERSATION_DIR`), sealed there
//! too ([`crate::conversation_store`]), so a broker restart resumes it.
//! Without one, a restart starts a new conversation.
//!
//! Past the bounds, the chat keeps what the owner said in an exchange,
//! and lets its answer go ([`Conversation`]): each run is also given the
//! owner's earlier messages, verbatim, as the broker recorded them.
//!
//! The owner can start a new conversation from the app, which moves the
//! local hub's conversation number on. The chat then forgets what it
//! kept. A message sent before the new conversation started isn't part
//! of it: its run is given nothing, and its exchange isn't kept. Nor is
//! the exchange of a run in flight when it started.

use std::collections::BTreeSet;
use std::time::Duration;

use hatchling_core::conversation::{self, ChatExchange, Conversation};

use hatchling_channel::{ChannelId, LocalHub, Message};
use hatchling_core::ScheduleGrant;
use hatchling_scheduler::{Delivery, Launcher, Run, RunRequest, Timestamp};

use crate::conversation_store::{self, ConversationStore, Resumed, SealedConversation};

/// How often an idle chat looks for a message, and a running one for its
/// answer.
pub const CHAT_POLL: Duration = Duration::from_millis(200);

/// What the person is told when a chat run fails or can't start, and
/// nothing more is known.
pub const CHAT_FAILED: &str = "Something went wrong and the agent couldn't answer. Try again; if it keeps happening, `sudo hatchling-broker doctor` checks the install.";

/// What the person is told when a chat run failed as `ending` says.
///
/// In plain words, and what they can do about it, naming `model`, the
/// operator's model name, if one is set. Only the broker's own words:
/// nothing the run wrote is shown.
#[must_use]
pub fn chat_failed(ending: Option<hatchling_scheduler::Ending>, model: Option<&str>) -> String {
    use hatchling_core::run_failure::RunFailure;
    let named = model.map_or_else(String::new, |model| format!(" ({model})"));
    match ending {
        Some(hatchling_scheduler::Ending::TimedOut) => String::from(
            "The agent took too long to answer, so Hatchling stopped it. A model that is still loading can do this: try again in a moment.",
        ),
        Some(hatchling_scheduler::Ending::Exited(code)) => match RunFailure::from_code(code) {
            Some(RunFailure::ModelUnavailable) => format!(
                "Hatchling couldn't get an answer from its model{named}. Check that it is running and reachable; `sudo hatchling-broker doctor` tests it."
            ),
            Some(RunFailure::ToolsMisused) => format!(
                "The model{named} kept calling its tools wrongly, so Hatchling stopped it before anything ran. Try again, or switch to a model that is better at tool calls."
            ),
            Some(RunFailure::OutOfSteps) => String::from(
                "The agent used all its steps before it finished. Try asking for something smaller.",
            ),
            Some(RunFailure::OutOfRoom) => format!(
                "The model{named} ran out of room before it finished its answer: its context window is too small for Hatchling. With Ollama, set OLLAMA_CONTEXT_LENGTH to 16384 or more; docs/install.md says how."
            ),
            None => String::from(CHAT_FAILED),
        },
        None => String::from(CHAT_FAILED),
    }
}

/// The label every chat run is launched under: its cell id and run
/// directory get their own random token beside it.
pub const CHAT_LABEL: &str = "chat";

/// The grants chat runs get: the local API caller's, when they let a run
/// answer on the local channel; `None` (no chat) when they don't.
#[must_use]
pub fn chat_grants(api: &BTreeSet<ScheduleGrant>) -> Option<Vec<ScheduleGrant>> {
    let answer = ScheduleGrant::ChannelSend {
        channel: ChannelId::local().0,
    };
    api.contains(&answer).then(|| api.iter().cloned().collect())
}

/// What one [`Chat::step`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// No run, and no message waiting.
    Idle,
    /// A run is still working on its message.
    Running,
    /// A message's run started.
    Started,
    /// A run ended, and its answer, or [`CHAT_FAILED`], was delivered.
    Answered,
    /// A run ended, or couldn't start, and nothing reached the person.
    Undelivered,
}

/// Where the chat keeps the conversation beyond the broker's memory.
pub trait Keeps {
    /// Keeps `kept`, the conversation `sent_in` as the chat holds it once
    /// its newest exchange, the last of `kept.exchanges` if any, is kept.
    fn keep(&self, sent_in: u64, kept: &Conversation);
}

/// Nowhere: the conversation lives in the broker's memory alone.
impl Keeps for () {
    fn keep(&self, _: u64, _: &Conversation) {}
}

impl<K: Keeps> Keeps for Option<K> {
    fn keep(&self, sent_in: u64, kept: &Conversation) {
        if let Some(keeps) = self {
            keeps.keep(sent_in, kept);
        }
    }
}

/// The chat's one run in flight, and the conversation so far.
#[derive(Debug)]
pub struct Chat<R, K = ()> {
    /// The run in flight, the message it answers, and the conversation
    /// that message was sent in.
    running: Option<(R, String, u64)>,
    grants: Vec<ScheduleGrant>,
    /// The conversation kept is in.
    conversation: u64,
    /// The conversation kept: no more than a run is given.
    kept: Conversation,
    /// Where each exchange is kept beyond memory.
    keeps: K,
    /// The operator's model name, for telling the person why a run failed.
    model: Option<String>,
}

impl<R: Run> Chat<R> {
    /// A chat whose runs hold `grants`, keeping the conversation in memory
    /// alone.
    #[must_use]
    pub const fn new(grants: Vec<ScheduleGrant>) -> Self {
        let kept = Conversation {
            earlier: Vec::new(),
            exchanges: Vec::new(),
        };
        Self::resuming(grants, 0, kept, ())
    }
}

impl<R: Run, K: Keeps> Chat<R, K> {
    /// A chat whose runs hold `grants`, going on with `kept`, the local
    /// channel's `conversation` so far, and keeping each exchange in
    /// `keeps` too.
    #[must_use]
    pub const fn resuming(
        grants: Vec<ScheduleGrant>,
        conversation: u64,
        kept: Conversation,
        keeps: K,
    ) -> Self {
        Self {
            running: None,
            grants,
            conversation,
            kept,
            keeps,
            model: None,
        }
    }

    /// The chat, naming `model` when it tells the person a run failed.
    #[must_use]
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// The exchanges a run started now is given, oldest first.
    #[must_use]
    pub fn history(&self) -> Vec<ChatExchange> {
        conversation::recent(&self.kept.exchanges)
    }

    /// What the owner said earlier that a run started now is given,
    /// oldest first.
    #[must_use]
    pub fn earlier(&self) -> Vec<String> {
        conversation::earlier(&self.kept.earlier)
    }

    /// Keeps the exchange of `user` and `answer`, the person having been
    /// shown `answer` as written; past the bounds, the oldest answers go,
    /// and what the owner said in them is kept as earlier.
    fn keep(&mut self, user: String, answer: String) {
        self.kept.push(ChatExchange {
            user,
            assistant: answer,
        });
        self.keeps.keep(self.conversation, &self.kept);
    }

    /// One step at `now`, in the local channel's `conversation`: a
    /// running run is polled, and a finished one answered; with none
    /// running, the message `next` gives, with the conversation it was
    /// sent in, starts one.
    pub fn step<L, D>(
        &mut self,
        next: impl FnOnce() -> Option<(Message, u64)>,
        conversation: u64,
        launcher: &L,
        delivery: &D,
        now: Timestamp,
    ) -> Step
    where
        L: Launcher<Run = R>,
        D: Delivery,
    {
        if conversation != self.conversation {
            self.conversation = conversation;
            self.kept = Conversation::default();
        }
        if let Some((run, _, _)) = &mut self.running {
            let Some(outcome) = run.poll() else {
                return Step::Running;
            };
            let ending = run.ending();
            let Some((_, user, sent_in)) = self.running.take() else {
                return Step::Idle;
            };
            let answer = match outcome {
                Ok(answer) => answer,
                Err(reason) => {
                    tracing::warn!(reason = ?reason, "a chat run failed");
                    return answer_with(delivery, &chat_failed(ending, self.model.as_deref()));
                }
            };
            return match delivery.deliver_verbatim(&ChannelId::local().0, &answer) {
                Ok(shown) => {
                    if shown && sent_in == self.conversation {
                        self.keep(user, answer);
                    }
                    Step::Answered
                }
                Err(error) => {
                    tracing::warn!(error = ?error, "a chat answer was not delivered");
                    Step::Undelivered
                }
            };
        }
        let Some((message, sent_in)) = next() else {
            return Step::Idle;
        };
        let (history, earlier) = if sent_in == self.conversation {
            (self.history(), self.earlier())
        } else {
            (Vec::new(), Vec::new())
        };
        let request = RunRequest {
            schedule_id: String::from(CHAT_LABEL),
            task: message.text.clone(),
            grants: self.grants.clone(),
            due: now,
            missed: false,
            channel: ChannelId::local().0,
            depth: 0,
            event: None,
            space: None,
            feed: None,
            chat: true,
            history,
            earlier,
        };
        match launcher.launch(request) {
            Ok(run) => {
                self.running = Some((run, message.text, sent_in));
                Step::Started
            }
            Err(reason) => {
                tracing::warn!(reason = ?reason, "a chat run could not start");
                answer_with(delivery, CHAT_FAILED)
            }
        }
    }
}

/// Delivers `answer` on the local channel.
fn answer_with<D: Delivery>(delivery: &D, answer: &str) -> Step {
    match delivery.deliver(&ChannelId::local().0, answer) {
        Ok(()) => Step::Answered,
        Err(error) => {
            tracing::warn!(error = ?error, "a chat answer was not delivered");
            Step::Undelivered
        }
    }
}

/// The chat loop: forever, with the real launcher and the hub's own
/// delivery, resting [`CHAT_POLL`] whenever there is nothing to do.
pub fn chat_forever<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    config: &'env crate::Config,
    key: &'env dyn hatchling_vault::OpensStores,
    hub: &'env LocalHub,
    env: crate::RunEnv,
    grants: Vec<ScheduleGrant>,
) -> ! {
    let launcher = crate::CellLauncher::new(scope, config, key, env);
    let delivery = crate::schedule_ops::HubDelivery::new(Some(hub));
    let store = ConversationStore::of(config, key);
    let kept = match store.as_ref().map(conversation_store::at_start) {
        Some(Resumed::Kept(kept)) => kept.clone(),
        Some(Resumed::Unreadable) | None => Conversation::default(),
    };
    let keeps = store.map(|store| SealedConversation::new(store, hub));
    let mut chat = Chat::resuming(grants, hub.conversation(), kept, keeps)
        .with_model(config.scheduler.model_name.clone());
    loop {
        let step = chat.step(
            || hub.take_in_conversation(),
            hub.conversation(),
            &launcher,
            &delivery,
            Timestamp::now(),
        );
        match step {
            Step::Idle | Step::Running => std::thread::sleep(CHAT_POLL),
            Step::Started | Step::Answered | Step::Undelivered => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use hatchling_channel::SenderId;

    use super::*;

    /// A run that answers after `polls` polls with `outcome`.
    #[derive(Debug)]
    struct Scripted {
        polls: u32,
        outcome: Result<String, String>,
    }

    impl Run for Scripted {
        fn poll(&mut self) -> Option<Result<String, String>> {
            if self.polls == 0 {
                Some(self.outcome.clone())
            } else {
                self.polls -= 1;
                None
            }
        }
    }

    /// Records each request, and starts a run with the next outcome.
    #[derive(Default)]
    struct Launches {
        requests: RefCell<Vec<RunRequest>>,
        outcomes: RefCell<Vec<Result<Scripted, String>>>,
    }

    impl Launcher for Launches {
        type Run = Scripted;

        fn launch(&self, request: RunRequest) -> Result<Scripted, String> {
            self.requests.borrow_mut().push(request);
            self.outcomes.borrow_mut().remove(0)
        }
    }

    /// Records each delivery; refuses them all when `refuse` is set, and
    /// shows the person something else in their place when `replace` is.
    #[derive(Default)]
    struct Answers {
        delivered: RefCell<Vec<(String, String)>>,
        refuse: bool,
        replace: std::cell::Cell<bool>,
    }

    impl Delivery for Answers {
        fn deliver(&self, channel: &str, text: &str) -> Result<(), String> {
            self.deliver_verbatim(channel, text).map(|_| ())
        }

        fn deliver_verbatim(&self, channel: &str, text: &str) -> Result<bool, String> {
            self.delivered
                .borrow_mut()
                .push((channel.to_owned(), text.to_owned()));
            if self.refuse {
                Err(String::from("no client"))
            } else {
                Ok(!self.replace.get())
            }
        }
    }

    fn answering(answer: &str) -> Scripted {
        Scripted {
            polls: 0,
            outcome: Ok(answer.to_owned()),
        }
    }

    /// Runs `text` to its end in `chat`: started, then answered.
    fn exchange<K: Keeps>(
        chat: &mut Chat<Scripted, K>,
        text: &str,
        launches: &Launches,
        answers: &Answers,
    ) -> Step {
        let now = Timestamp::now();
        assert_eq!(
            chat.step(|| Some((message(text), 0)), 0, launches, answers, now),
            Step::Started
        );
        chat.step(|| None, 0, launches, answers, now)
    }

    fn histories(launches: &Launches) -> Vec<Vec<(String, String)>> {
        launches
            .requests
            .borrow()
            .iter()
            .map(|request| {
                request
                    .history
                    .iter()
                    .map(|turn| (turn.user.clone(), turn.assistant.clone()))
                    .collect()
            })
            .collect()
    }

    fn turn(user: &str, assistant: &str) -> (String, String) {
        (user.to_owned(), assistant.to_owned())
    }

    /// Records the conversation kept beyond memory, with its number, at
    /// each exchange kept.
    #[derive(Default)]
    struct Recorded(RefCell<Vec<(u64, Conversation)>>);

    impl Keeps for &Recorded {
        fn keep(&self, sent_in: u64, kept: &Conversation) {
            self.0.borrow_mut().push((sent_in, kept.clone()));
        }
    }

    fn exchanged(user: &str, assistant: &str) -> ChatExchange {
        ChatExchange {
            user: user.to_owned(),
            assistant: assistant.to_owned(),
        }
    }

    fn said(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| (*text).to_owned()).collect()
    }

    /// WO-026: a chat resumed from what was kept gives its first run that,
    /// and each exchange it keeps is kept beyond its memory too, with its
    /// conversation, as the chat holds it then.
    #[test]
    fn a_resumed_chat_goes_on_and_keeps_each_exchange_beyond_memory() {
        let launches = Launches::default();
        let huge = "x".repeat(conversation::MAX_HISTORY_BYTES);
        launches.outcomes.borrow_mut().extend([
            Ok(answering("a1")),
            Ok(answering("a2")),
            Ok(answering(&huge)),
        ]);
        let answers = Answers::default();
        let recorded = Recorded::default();
        let kept = Conversation {
            earlier: said(&["said before"]),
            exchanges: vec![exchanged("q0", "a0")],
        };
        let mut chat = Chat::resuming(vec![send_local()], 2, kept, &recorded);
        let now = Timestamp::now();
        let mut ask = |text: &str| {
            assert_eq!(
                chat.step(|| Some((message(text), 2)), 2, &launches, &answers, now),
                Step::Started
            );
            assert_eq!(
                chat.step(|| None, 2, &launches, &answers, now),
                Step::Answered
            );
        };
        ask("q1");
        // Replaced, it isn't kept anywhere.
        answers.replace.set(true);
        ask("q2");
        answers.replace.set(false);
        // Too big to give, its answer goes, as do those before it, and
        // what the owner said in each is kept as earlier.
        ask("q3");
        assert_eq!(
            histories(&launches),
            [
                vec![turn("q0", "a0")],
                vec![turn("q0", "a0"), turn("q1", "a1")],
                vec![turn("q0", "a0"), turn("q1", "a1")],
            ]
        );
        let earliers: Vec<_> = launches
            .requests
            .borrow()
            .iter()
            .map(|request| request.earlier.clone())
            .collect();
        assert_eq!(earliers, vec![said(&["said before"]); 3]);
        let after_q1 = Conversation {
            earlier: said(&["said before"]),
            exchanges: vec![exchanged("q0", "a0"), exchanged("q1", "a1")],
        };
        let after_q3 = Conversation {
            earlier: said(&["said before", "q0", "q1", "q3"]),
            exchanges: Vec::new(),
        };
        assert_eq!(*recorded.0.borrow(), [(2, after_q1), (2, after_q3.clone())]);
        assert_eq!(chat.earlier(), after_q3.earlier);
        assert!(chat.history().is_empty());
        // A new conversation's run is given nothing said before it.
        launches.outcomes.borrow_mut().push(Ok(answering("a5")));
        assert_eq!(
            chat.step(|| Some((message("q5"), 3)), 3, &launches, &answers, now),
            Step::Started
        );
        let given = launches
            .requests
            .borrow()
            .last()
            .map(|request| (request.history.clone(), request.earlier.clone()));
        assert_eq!(given, Some((Vec::new(), Vec::new())));
        assert!(chat.earlier().is_empty());
        // Given no store, nothing is kept beyond memory.
        None::<&Recorded>.keep(5, &after_q3);
        Some(&recorded).keep(5, &after_q3);
        assert_eq!(recorded.0.borrow().len(), 3);
    }

    /// WO-026: a follow-up's run is given the exchange before it, as the
    /// person said it and was shown it.
    #[test]
    fn a_follow_ups_run_is_given_the_exchange_before_it() {
        let launches = Launches::default();
        launches.outcomes.borrow_mut().extend([
            Ok(answering("1. Tea. 2. Coffee.")),
            Ok(answering("Coffee, then.")),
            Ok(answering("Done.")),
        ]);
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        assert_eq!(
            exchange(&mut chat, "Two drinks?", &launches, &answers),
            Step::Answered
        );
        assert_eq!(
            exchange(&mut chat, "The second one.", &launches, &answers),
            Step::Answered
        );
        assert_eq!(
            exchange(&mut chat, "Thanks.", &launches, &answers),
            Step::Answered
        );
        let first = turn("Two drinks?", "1. Tea. 2. Coffee.");
        let second = turn("The second one.", "Coffee, then.");
        assert_eq!(
            histories(&launches),
            [vec![], vec![first.clone()], vec![first, second]]
        );
        assert_eq!(chat.history().len(), 3);
    }

    /// WO-026: what the person wasn't shown as written is never given
    /// back: a failed run's sentence, an answer the gate replaced, or one
    /// that reached no one.
    #[test]
    fn only_an_answer_the_person_was_shown_is_given_back() {
        let launches = Launches::default();
        launches.outcomes.borrow_mut().extend([
            Ok(Scripted {
                polls: 0,
                outcome: Err(String::from("the run exited 1")),
            }),
            Err(String::from("cannot spawn")),
            Ok(answering("the key is hunter2")),
            Ok(answering("kept")),
            Ok(answering("lost")),
            Ok(answering("next")),
        ]);
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        assert_eq!(
            exchange(&mut chat, "fails", &launches, &answers),
            Step::Answered
        );
        let now = Timestamp::now();
        assert_eq!(
            chat.step(
                || Some((message("unstarted"), 0)),
                0,
                &launches,
                &answers,
                now
            ),
            Step::Answered
        );
        answers.replace.set(true);
        assert_eq!(
            exchange(&mut chat, "the key?", &launches, &answers),
            Step::Answered
        );
        answers.replace.set(false);
        assert_eq!(
            exchange(&mut chat, "keep me", &launches, &answers),
            Step::Answered
        );
        let refusing = Answers {
            refuse: true,
            ..Answers::default()
        };
        assert_eq!(
            exchange(&mut chat, "nowhere", &launches, &refusing),
            Step::Undelivered
        );
        assert_eq!(
            exchange(&mut chat, "last", &launches, &answers),
            Step::Answered
        );
        let histories = histories(&launches);
        assert_eq!(histories.last(), Some(&vec![turn("keep me", "kept")]));
        assert!(histories.iter().take(4).all(Vec::is_empty), "{histories:?}");
    }

    #[test]
    fn the_chat_keeps_no_more_than_a_run_is_given() {
        let launches = Launches::default();
        let count = conversation::MAX_EXCHANGES + 3;
        launches
            .outcomes
            .borrow_mut()
            .extend((0..count).map(|n| Ok(answering(&format!("a{n}")))));
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        for n in 0..count {
            assert_eq!(
                exchange(&mut chat, &format!("q{n}"), &launches, &answers),
                Step::Answered
            );
        }
        assert_eq!(chat.kept.exchanges.len(), conversation::MAX_EXCHANGES);
        // What the owner said in those past the bound is kept.
        assert_eq!(chat.earlier(), ["q0", "q1", "q2"]);
        let history = chat.history();
        assert_eq!(history.len(), conversation::MAX_EXCHANGES);
        assert_eq!(history.first().map(|turn| turn.user.as_str()), Some("q3"));
        assert_eq!(
            history.last().map(|turn| turn.user.clone()),
            Some(format!("q{}", count - 1))
        );
    }

    /// WO-026: after the owner starts a new conversation, a run is given
    /// nothing said before it. A message sent before it started isn't
    /// part of it, nor is the exchange of a run in flight across it.
    #[test]
    fn a_new_conversation_is_given_nothing_said_before_it() {
        let launches = Launches::default();
        launches
            .outcomes
            .borrow_mut()
            .extend((0..7).map(|n| Ok(answering(&format!("a{n}")))));
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        let now = Timestamp::now();
        let step = |chat: &mut Chat<Scripted>, sent: Option<(&str, u64)>, now_in: u64| {
            chat.step(
                || sent.map(|(text, n)| (message(text), n)),
                now_in,
                &launches,
                &answers,
                now,
            )
        };
        // Two exchanges in the first conversation.
        assert_eq!(step(&mut chat, Some(("q0", 0)), 0), Step::Started);
        assert_eq!(step(&mut chat, None, 0), Step::Answered);
        assert_eq!(step(&mut chat, Some(("q1", 0)), 0), Step::Started);
        // The owner starts a new conversation while q1's run is working:
        // its answer is shown, and not kept.
        assert_eq!(step(&mut chat, None, 1), Step::Answered);
        assert!(chat.history().is_empty());
        // A message sent before the new conversation started: its run is
        // given nothing, and its exchange isn't kept.
        assert_eq!(step(&mut chat, Some(("q2", 0)), 1), Step::Started);
        assert_eq!(step(&mut chat, None, 1), Step::Answered);
        // The new conversation's own messages.
        assert_eq!(step(&mut chat, Some(("q3", 1)), 1), Step::Started);
        assert_eq!(step(&mut chat, None, 1), Step::Answered);
        assert_eq!(step(&mut chat, Some(("q4", 1)), 1), Step::Started);
        assert_eq!(step(&mut chat, None, 1), Step::Answered);
        // The owner starts another between the chat reading the number and
        // taking the message: the message is the new conversation's, and is
        // given nothing kept in the last.
        assert_eq!(step(&mut chat, Some(("q5", 2)), 1), Step::Started);
        assert_eq!(step(&mut chat, None, 2), Step::Answered);
        assert_eq!(chat.history().len(), 1, "only q5's exchange is kept");
        assert_eq!(
            histories(&launches),
            [
                vec![],
                vec![turn("q0", "a0")],
                vec![],
                vec![],
                vec![turn("q3", "a3")],
                vec![],
            ]
        );
        let shown: Vec<String> = answers
            .delivered
            .borrow()
            .iter()
            .map(|(_, text)| text.clone())
            .collect();
        assert_eq!(
            shown,
            ["a0", "a1", "a2", "a3", "a4", "a5"],
            "every answer is shown"
        );
    }

    fn message(text: &str) -> Message {
        Message {
            sender: SenderId(String::from("api")),
            channel: ChannelId::local(),
            text: text.to_owned(),
        }
    }

    fn send_local() -> ScheduleGrant {
        ScheduleGrant::ChannelSend {
            channel: String::from("local"),
        }
    }

    fn egress() -> ScheduleGrant {
        ScheduleGrant::Egress {
            destination: String::from("localhost:18089"),
        }
    }

    #[test]
    fn chat_runs_hold_the_api_grants_only_when_they_can_answer_locally() {
        let both: BTreeSet<ScheduleGrant> = [egress(), send_local()].into();
        assert_eq!(chat_grants(&both), Some(both.iter().cloned().collect()));
        assert_eq!(chat_grants(&[egress()].into()), None);
        let elsewhere = ScheduleGrant::ChannelSend {
            channel: String::from("other"),
        };
        assert_eq!(chat_grants(&[elsewhere].into()), None);
        assert_eq!(chat_grants(&BTreeSet::new()), None);
    }

    #[test]
    fn a_message_is_one_run_whose_answer_goes_back_on_the_local_channel() {
        let launches = Launches::default();
        launches.outcomes.borrow_mut().push(Ok(Scripted {
            polls: 1,
            outcome: Ok(String::from("Two meetings.")),
        }));
        let answers = Answers::default();
        let grants = vec![egress(), send_local()];
        let mut chat = Chat::new(grants.clone());
        let now = Timestamp::now();
        assert_eq!(chat.step(|| None, 0, &launches, &answers, now), Step::Idle);
        assert_eq!(
            chat.step(
                || Some((message("What is on today?"), 0)),
                0,
                &launches,
                &answers,
                now
            ),
            Step::Started
        );
        // While it runs, the next message waits in the queue.
        let taken = std::cell::Cell::new(false);
        let untouched = || {
            taken.set(true);
            Some((message("too soon"), 0))
        };
        assert_eq!(
            chat.step(untouched, 0, &launches, &answers, now),
            Step::Running
        );
        assert_eq!(
            chat.step(untouched, 0, &launches, &answers, now),
            Step::Answered
        );
        assert!(!taken.get(), "a message was taken while a run was working");
        assert_eq!(
            *answers.delivered.borrow(),
            [(String::from("local"), String::from("Two meetings."))]
        );
        let requests = launches.requests.borrow();
        let request = requests.first().unwrap();
        assert_eq!(
            (
                request.schedule_id.as_str(),
                request.task.as_str(),
                request.channel.as_str()
            ),
            (CHAT_LABEL, "What is on today?", "local")
        );
        assert_eq!(request.grants, grants);
        assert_eq!(
            (request.due, request.missed, request.depth),
            (now, false, 0)
        );
        assert!(request.event.is_none() && request.space.is_none() && request.feed.is_none());
        assert!(
            request.chat,
            "a chat turn: its agent speaks with the chat prompt"
        );
        drop(requests);
        // Answered, it is idle again.
        assert_eq!(chat.step(|| None, 0, &launches, &answers, now), Step::Idle);
    }

    #[test]
    fn a_failed_or_unstarted_run_tells_the_person_a_fixed_sentence() {
        let launches = Launches::default();
        launches.outcomes.borrow_mut().extend([
            Ok(Scripted {
                polls: 0,
                outcome: Err(String::from("the run exited 1: /tmp/run-4182 gone")),
            }),
            Err(String::from("cannot spawn: /srv/hatchling/runs busy")),
        ]);
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        let now = Timestamp::now();
        assert_eq!(
            chat.step(|| Some((message("one"), 0)), 0, &launches, &answers, now),
            Step::Started
        );
        assert_eq!(
            chat.step(|| None, 0, &launches, &answers, now),
            Step::Answered
        );
        assert_eq!(
            chat.step(|| Some((message("two"), 0)), 0, &launches, &answers, now),
            Step::Answered
        );
        let local = String::from("local");
        assert_eq!(
            *answers.delivered.borrow(),
            [
                (local.clone(), String::from(CHAT_FAILED)),
                (local, String::from(CHAT_FAILED))
            ]
        );
        assert_eq!(chat.step(|| None, 0, &launches, &answers, now), Step::Idle);
    }

    /// A run that fails as `ending` says, at once.
    #[derive(Debug)]
    struct Ended(hatchling_scheduler::Ending);

    impl Run for Ended {
        fn poll(&mut self) -> Option<Result<String, String>> {
            Some(Err(String::from("the run exited: /tmp/run-4182 said so")))
        }

        fn ending(&self) -> Option<hatchling_scheduler::Ending> {
            Some(self.0)
        }
    }

    /// Launches one [`Ended`] run.
    struct OneEnded(RefCell<Option<Ended>>);

    impl Launcher for OneEnded {
        type Run = Ended;

        fn launch(&self, _: RunRequest) -> Result<Ended, String> {
            self.0.borrow_mut().take().ok_or_else(String::new)
        }
    }

    /// The person is told why a run failed in plain words, and what to do,
    /// from how it ended alone: nothing the run wrote reaches them.
    #[test]
    fn a_failed_run_is_explained_in_plain_words() {
        use hatchling_core::run_failure::RunFailure;
        use hatchling_scheduler::Ending;
        let exited = |failure: RunFailure| Some(Ending::Exited(failure.code().into()));
        let model = chat_failed(exited(RunFailure::ModelUnavailable), Some("qwen3.5:9b"));
        assert!(
            model.contains("its model (qwen3.5:9b)") && model.contains("doctor"),
            "{model}"
        );
        let tools = chat_failed(exited(RunFailure::ToolsMisused), Some("qwen3.5:9b"));
        assert!(
            tools.starts_with("The model (qwen3.5:9b) kept calling"),
            "{tools}"
        );
        assert!(!chat_failed(exited(RunFailure::ToolsMisused), None).contains('('));
        let steps = chat_failed(exited(RunFailure::OutOfSteps), Some("m"));
        assert!(steps.contains("all its steps"), "{steps}");
        let slow = chat_failed(Some(Ending::TimedOut), Some("m"));
        assert!(slow.contains("too long"), "{slow}");
        let room = chat_failed(exited(RunFailure::OutOfRoom), Some("qwen3.5:9b"));
        assert!(
            room.starts_with("The model (qwen3.5:9b) ran out of room")
                && room.contains("OLLAMA_CONTEXT_LENGTH"),
            "{room}"
        );
        for other in [Some(Ending::Exited(1)), Some(Ending::Exited(64)), None] {
            assert_eq!(chat_failed(other, Some("m")), CHAT_FAILED, "{other:?}");
        }
        let said: BTreeSet<String> = [model, tools, steps, slow, room, String::from(CHAT_FAILED)]
            .into_iter()
            .collect();
        assert_eq!(said.len(), 6, "each says something of its own");

        // Through the chat: the run's own words never reach the person.
        let launches = OneEnded(RefCell::new(Some(Ended(Ending::Exited(
            RunFailure::ToolsMisused.code().into(),
        )))));
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]).with_model(Some(String::from("m")));
        let now = Timestamp::now();
        chat.step(|| Some((message("one"), 0)), 0, &launches, &answers, now);
        assert_eq!(
            chat.step(|| None, 0, &launches, &answers, now),
            Step::Answered
        );
        let delivered = answers.delivered.borrow();
        let [(_, told)] = delivered.as_slice() else {
            unreachable!("one answer: {delivered:?}");
        };
        assert!(told.starts_with("The model (m) kept calling"), "{told}");
        assert!(!told.contains("/tmp"), "{told}");
    }

    #[test]
    fn an_answer_with_nowhere_to_go_is_reported_and_the_chat_goes_on() {
        let launches = Launches::default();
        launches.outcomes.borrow_mut().extend([
            Ok(Scripted {
                polls: 0,
                outcome: Ok(String::from("lost")),
            }),
            Err(String::from("no")),
        ]);
        let answers = Answers {
            refuse: true,
            ..Answers::default()
        };
        let mut chat = Chat::new(vec![send_local()]);
        let now = Timestamp::now();
        assert_eq!(
            chat.step(|| Some((message("one"), 0)), 0, &launches, &answers, now),
            Step::Started
        );
        assert_eq!(
            chat.step(|| None, 0, &launches, &answers, now),
            Step::Undelivered
        );
        assert_eq!(
            chat.step(|| Some((message("two"), 0)), 0, &launches, &answers, now),
            Step::Undelivered
        );
        assert_eq!(answers.delivered.borrow().len(), 2);
    }

    #[test]
    fn answer_with_reports_whether_the_answer_was_delivered() {
        let answers = Answers::default();
        assert_eq!(answer_with(&answers, "hi"), Step::Answered);
        let refusing = Answers {
            refuse: true,
            ..Answers::default()
        };
        assert_eq!(answer_with(&refusing, "hi"), Step::Undelivered);
        assert_eq!(
            *answers.delivered.borrow(),
            [(String::from("local"), String::from("hi"))]
        );
    }
}
