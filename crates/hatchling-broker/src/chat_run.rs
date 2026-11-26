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
//! failed stays in the broker's log. So is what they see of a run stopped
//! for trying to send a secret out ([`stopped_told`]): what kind of thing
//! it tried, once, and never which secret.
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

use hatchling_core::conversation::{Bounds, ChatExchange, Compacted, Conversation};
use hatchling_core::run_record::{MAX_CARRIED, RunRecord};

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

/// What the person is told of a run Hatchling stopped, a secret having been
/// caught going to `to` ([`crate::held_check::HeldHit`]).
///
/// Which secret isn't said, here or anywhere: the words are fixed, so they
/// can't carry one.
#[must_use]
pub const fn stopped_told(to: hatchling_scheduler::SentTo) -> &'static str {
    use hatchling_scheduler::SentTo;
    match to {
        SentTo::Website => {
            "Hatchling stopped this run: it tried to send a saved secret to a website."
        }
        SentTo::Search => {
            "Hatchling stopped this run: it tried to send a saved secret to a search service."
        }
        SentTo::Page => {
            "Hatchling stopped this run: it tried to type a saved secret into a web page."
        }
        SentTo::Harness => {
            "Hatchling stopped this run: it tried to give a saved secret to a coding tool."
        }
        SentTo::HarnessReturn => {
            "Hatchling stopped this run: a coding tool it started handed back a saved secret."
        }
    }
}

/// What the owner is told of a run they stopped, from the app (WO-051).
pub const YOU_STOPPED: &str = "You stopped it.";

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
        Some(hatchling_scheduler::Ending::Stopped(to)) => String::from(stopped_told(to)),
        Some(hatchling_scheduler::Ending::OwnerStopped) => String::from(YOU_STOPPED),
        Some(hatchling_scheduler::Ending::Exited(code)) => match RunFailure::from_code(code) {
            Some(RunFailure::ModelUnavailable) => format!(
                "Hatchling couldn't get an answer from its model{named}. Check that it is running and reachable; `sudo hatchling-broker doctor` tests it."
            ),
            Some(RunFailure::ModelIncapable) => format!(
                "The model{named} doesn't call tools, and Hatchling's agent works by calling them, so it stopped before anything ran. Switch to a model that calls tools; docs/install.md (\"Choosing a model\") lists some."
            ),
            Some(RunFailure::ModelSilent) => format!(
                "The model{named} answered with nothing, no text and no tool call, each time it was asked. Small models do this now and then: try again. If it keeps happening, a model that's better at tool calls will help; docs/install.md (\"Choosing a model\") lists some."
            ),
            Some(RunFailure::ModelThoughtOnly) => format!(
                "The model{named} thought about its answer and then gave none, each time it was asked. Hatchling asked again with its thinking turned off, and its server didn't turn it off. Try again, or switch to a model that answers after it thinks; docs/model-thinking.md says more."
            ),
            Some(RunFailure::ToolsMisused) => format!(
                "The model{named} kept calling its tools wrongly, so Hatchling stopped it before anything ran. Try again, or switch to a model that is better at tool calls."
            ),
            Some(RunFailure::OutOfSteps) => String::from(
                "The agent used all its steps before it finished, and didn't say where it got to. Ask it to go on, or for something smaller.",
            ),
            Some(RunFailure::OutOfTokens) => String::from(
                "The agent used its token budget for one run before it finished, and didn't say where it got to. Ask it to go on, or for something smaller.",
            ),
            Some(RunFailure::OutOfTime) => String::from(
                "The agent's time ran out before it finished, and it didn't say where it got to. Ask it to go on, or for something smaller.",
            ),
            Some(RunFailure::OutOfRoom) => format!(
                "The model{named} ran out of room before it finished its answer: its context window is too small for Hatchling. With Ollama, set OLLAMA_CONTEXT_LENGTH to 32768; docs/install.md says how."
            ),
            None => String::from(CHAT_FAILED),
        },
        None => String::from(CHAT_FAILED),
    }
}

/// What the person is told when a run failed as `ending` says.
///
/// `reached` is a usage limit the owner set, if one is reached now, and
/// `quiet` whether the broker's wait for the run's model ran out with the
/// model still answering.
///
/// A run whose model couldn't be reached while a limit is reached was the
/// broker refusing its model calls: the person is told which limit, and
/// when it turns. One whose model was still answering is told that, and
/// not to look for a model that isn't running ([`model_quiet`]).
/// Otherwise as [`chat_failed`] says.
#[must_use]
pub fn failed_with(
    ending: Option<hatchling_scheduler::Ending>,
    model: Option<&str>,
    reached: Option<&crate::usage::Reached>,
    quiet: bool,
) -> String {
    use hatchling_core::run_failure::RunFailure;
    let unavailable = matches!(
        ending,
        Some(hatchling_scheduler::Ending::Exited(code))
            if RunFailure::from_code(code) == Some(RunFailure::ModelUnavailable)
    );
    match reached {
        Some(reached) if unavailable => reached.told(),
        _ if unavailable && quiet => model_quiet(model),
        _ => chat_failed(ending, model),
    }
}

/// How a hosted model that costs nothing is named, at its end.
const FREE_MODEL: &str = ":free";

/// What the person is told when the broker's wait for `model`'s answer ran
/// out with the model still answering: it is there, and it is slow, or
/// its answer is long. Only the broker's own words.
#[must_use]
pub fn model_quiet(model: Option<&str>) -> String {
    let named = model.map_or_else(String::new, |model| format!(" ({model})"));
    let free = if model.is_some_and(|model| model.ends_with(FREE_MODEL)) {
        " It is a free hosted model, and those are slow when many people are using them."
    } else {
        ""
    };
    format!(
        "The model{named} was still working on its answer after more than five minutes, so Hatchling stopped waiting.{free} A very long answer takes this long too: try again, or ask for less at once."
    )
}

/// What the person is told the first time, in a conversation, that
/// older answers are no longer given to the agent whole.
pub const COMPACTED: &str = "This conversation is getting long. From here on, the agent is given only your most recent exchanges word for word; for older ones, it's given what you said, not its own replies. Start a new conversation to clear it.";

/// What the person is told the first time, in a conversation, that their
/// oldest words are no longer given to the agent at all.
pub const FORGOTTEN: &str = "Your oldest messages in this conversation are no longer given to the agent at all. If something you said early on still matters, say it again, or start a new conversation.";

/// What the person was told of compaction, in this conversation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Told {
    compacted: bool,
    forgotten: bool,
}

impl Told {
    /// What was told of `kept`, a conversation resumed: that it was
    /// compacted, if it holds what the owner said earlier.
    fn of(kept: &Conversation) -> Self {
        Self {
            compacted: !kept.earlier.is_empty(),
            forgotten: false,
        }
    }

    /// The notice `compacted` calls for, the first time each happens.
    const fn notice(&mut self, compacted: Compacted) -> Option<&'static str> {
        if compacted.said > 0 && !self.forgotten {
            self.forgotten = true;
            self.compacted = true;
            return Some(FORGOTTEN);
        }
        if compacted.answers > 0 && !self.compacted {
            self.compacted = true;
            return Some(COMPACTED);
        }
        None
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
    /// its newest exchange, the last of `kept.exchanges` if any, is kept,
    /// with the cell id of the `run` that answered it (WO-043), and the
    /// broker's `record` of that run (WO-053).
    fn keep(
        &self,
        sent_in: u64,
        kept: &Conversation,
        run: Option<&str>,
        record: Option<&RunRecord>,
    );
}

/// Nowhere: the conversation lives in the broker's memory alone.
impl Keeps for () {
    fn keep(&self, _: u64, _: &Conversation, _: Option<&str>, _: Option<&RunRecord>) {}
}

impl<K: Keeps> Keeps for Option<K> {
    fn keep(
        &self,
        sent_in: u64,
        kept: &Conversation,
        run: Option<&str>,
        record: Option<&RunRecord>,
    ) {
        if let Some(keeps) = self {
            keeps.keep(sent_in, kept, run, record);
        }
    }
}

/// Who a message the owner sends with Keep going is from (WO-053): the
/// chat gives its run [`hatchling_prompt::KEEP_GOING_TASK`] in its place.
pub const KEEP_GOING_SENDER: &str = "keep-going";

/// What the owner is shown they said when they ask a run to keep going.
pub const KEEP_GOING_TEXT: &str = "Keep going.";

/// The chat's one run in flight, and the conversation so far.
#[derive(Debug)]
pub struct Chat<R, K = ()> {
    /// The run in flight, the message it answers, the conversation that
    /// message was sent in, and the broker's notes it was given: given
    /// back if it ends without an answer (#498).
    running: Option<(R, String, u64, Noted)>,
    grants: Vec<ScheduleGrant>,
    /// The conversation kept is in.
    conversation: u64,
    /// The conversation kept: no more than a run is given.
    kept: Conversation,
    /// Where each exchange is kept beyond memory.
    keeps: K,
    /// The operator's model name, for telling the person why a run failed.
    model: Option<String>,
    /// Where the owner's choice of model is kept, when it is: named over
    /// the operator's ([`crate::model_choice`]).
    choice: Option<std::path::PathBuf>,
    /// The operator's other model's server, the owner may switch to: its
    /// model is named when they did.
    other: Option<crate::config::OtherModel>,
    /// The operator's first model's window: with the other's, what the
    /// conversation is sized by (#595).
    window: Option<u32>,
    /// Whether the person was told, in this conversation, that older
    /// answers are no longer given whole, and that their oldest words are
    /// no longer given at all.
    told: Told,
    /// Where the broker counts model usage, and keeps the owner's limits.
    usage: Option<std::path::PathBuf>,
    /// Where the owner's messages wait, when a run in flight may be given
    /// what they send while it works ([`crate::follow_ups`]).
    waiting: Option<LocalHub>,
    /// The conversation in which the last run to answer the owner did
    /// so with steps to spare, until it is taken
    /// ([`Chat::take_answered`]).
    answered: Option<u64>,
    /// What the owner asked in the last run whose answer they were
    /// shown and which ran out of steps, until it is taken
    /// ([`Chat::take_offer`]).
    offer: Option<Offer>,
    /// The broker's records of the runs the next one continues, oldest
    /// first, at most [`MAX_CARRIED`] (WO-053): each run that stopped
    /// before it was done, since the last that answered with steps to
    /// spare, in the conversation open now.
    carried: Vec<RunRecord>,
}

/// What the owner may keep for later from a run that ran out of steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The run, by its cell id.
    pub run: String,
    /// What the owner said in the exchange.
    pub said: String,
    /// The run's answer: where it said it stopped, in its own words.
    pub answer: String,
    /// The conversation it was said in.
    pub conversation: u64,
}

impl<R: Run> Chat<R> {
    /// A chat whose runs hold `grants`, keeping the conversation in memory
    /// alone.
    #[must_use]
    pub fn new(grants: Vec<ScheduleGrant>) -> Self {
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
    pub fn resuming(
        grants: Vec<ScheduleGrant>,
        conversation: u64,
        kept: Conversation,
        keeps: K,
    ) -> Self {
        Self {
            running: None,
            grants,
            conversation,
            told: Told::of(&kept),
            kept,
            keeps,
            model: None,
            choice: None,
            other: None,
            window: None,
            usage: None,
            waiting: None,
            answered: None,
            offer: None,
            carried: Vec::new(),
        }
    }

    /// The conversation in which a run answered the owner, was shown,
    /// and had steps to spare, since this was last asked: an answer
    /// given as the steps ran out isn't one, since it may leave work
    /// undone, and neither is one the owner wasn't shown.
    pub const fn take_answered(&mut self) -> Option<u64> {
        self.answered.take()
    }

    /// Notes how a run that answered the owner, shown to them in the
    /// conversation `sent_in` while it was open, ended: with steps to
    /// spare that conversation was answered; out of steps, what they
    /// said, `user`, is offered to keep, from the run in `cell`, with its
    /// `answer`.
    fn ended(&mut self, spent: bool, cell: Option<String>, user: &str, answer: &str, sent_in: u64) {
        self.answered = (!spent).then_some(sent_in);
        self.offer = cell.filter(|_| spent).map(|run| Offer {
            run,
            said: user.to_owned(),
            answer: answer.to_owned(),
            conversation: sent_in,
        });
    }

    /// What the owner asked in a run whose answer they were shown, in
    /// the conversation open then, and which ran out of steps as it
    /// answered, since this was last asked.
    pub const fn take_offer(&mut self) -> Option<Offer> {
        self.offer.take()
    }

    /// The chat, naming `model` when it tells the person a run failed.
    #[must_use]
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// The chat, naming the model the owner chose, kept in `owner_dir`,
    /// over the operator's when a run fails.
    #[must_use]
    pub fn with_choice(mut self, owner_dir: Option<std::path::PathBuf>) -> Self {
        self.choice = owner_dir;
        self
    }

    /// The chat, naming `other`'s model when the owner switched to the
    /// operator's other server.
    #[must_use]
    pub fn with_other(mut self, other: Option<crate::config::OtherModel>) -> Self {
        self.other = other;
        self
    }

    /// The chat, sizing the conversation it keeps and gives by `window`,
    /// the operator's first model's, or the other server's when the owner
    /// switched to it (#595).
    #[must_use]
    pub const fn with_window(mut self, window: Option<u32>) -> Self {
        self.window = window;
        self
    }

    /// The bounds of the conversation now: those of the window of the
    /// server in use.
    fn bounds(&self) -> Bounds {
        Bounds::for_window(crate::model_choice::window(
            self.choice.as_deref(),
            self.window,
            self.other.as_ref(),
        ))
    }

    /// The chat, checking the owner's usage limits in `usage` before each
    /// run, and naming one reached when a run fails for it.
    #[must_use]
    pub fn with_usage(mut self, usage: std::path::PathBuf) -> Self {
        self.usage = Some(usage);
        self
    }

    /// The chat, whose run in flight is given what the owner sends while
    /// it works, from `hub`, where their messages wait.
    #[must_use]
    pub fn with_follow_ups(mut self, hub: LocalHub) -> Self {
        self.waiting = Some(hub);
        self
    }

    /// The usage limit the owner set that's reached at `now`, if one is.
    fn reached(&self, now: Timestamp) -> Option<crate::usage::Reached> {
        crate::usage::reached(self.usage.as_deref()?, now.as_second())
    }

    /// The exchanges a run started now is given, oldest first: within the
    /// bounds of the window in use, which a switch to a smaller one makes
    /// less than is kept.
    #[must_use]
    pub fn history(&self) -> Vec<ChatExchange> {
        self.kept.within(&self.bounds()).exchanges
    }

    /// What the owner said earlier that a run started now is given,
    /// oldest first, within the same bounds.
    #[must_use]
    pub fn earlier(&self) -> Vec<String> {
        self.kept.within(&self.bounds()).earlier
    }

    /// Keeps the exchange of `user` and `answer`, the person having been
    /// shown `answer` as written; past the bounds, the oldest answers go,
    /// and what the owner said in them is kept as earlier.
    fn keep(
        &mut self,
        user: String,
        answer: String,
        run: Option<&str>,
        record: Option<&RunRecord>,
    ) -> Compacted {
        let bounds = self.bounds();
        let compacted = self.kept.push(
            ChatExchange {
                user,
                assistant: answer,
            },
            &bounds,
        );
        self.keeps.keep(self.conversation, &self.kept, run, record);
        compacted
    }

    /// Carries `record`, of a run that stopped before it was done, to the
    /// next run (WO-053): the oldest let go past [`MAX_CARRIED`], and the
    /// owner may ask it to keep going.
    fn carry(&mut self, record: Option<RunRecord>) {
        if let Some(record) = record {
            self.carried.push(record);
            if self.carried.len() > MAX_CARRIED {
                self.carried.remove(0);
            }
        }
        self.say_continuing();
    }

    /// Lets go of what was carried: a run answered with steps to spare,
    /// or the conversation changed.
    fn settled(&mut self) {
        self.carried.clear();
        self.say_continuing();
    }

    /// Tells the hub whether the conversation open now may keep going.
    fn say_continuing(&self) {
        if let Some(hub) = &self.waiting {
            hub.may_keep_going(self.conversation, !self.carried.is_empty());
        }
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
        self.step_noted(next, &(), conversation, launcher, delivery, now)
    }

    /// [`Chat::step`], where a run that starts is given the broker's
    /// `notes` before the owner's message (WO-041): in its task alone,
    /// never as the owner's words.
    pub fn step_noted<L, D>(
        &mut self,
        next: impl FnOnce() -> Option<(Message, u64)>,
        notes: &dyn Notes,
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
            self.told = Told::default();
            self.settled();
        }
        if let Some((run, _, _, _)) = &mut self.running {
            let Some(outcome) = run.poll() else {
                return Step::Running;
            };
            let ending = run.ending();
            let cell = run.cell_id();
            let Some((_, user, sent_in, given)) = self.running.take() else {
                return Step::Idle;
            };
            // The broker's record of the run, as its session left it
            // (WO-053): kept with its answer, and carried to the next run
            // when it stopped before it was done.
            let record = cell.as_deref().and_then(crate::run_record::take);
            let current = sent_in == self.conversation;
            // What the run was given of the owner's further messages is
            // part of what they said in this exchange. The run is given no
            // more, however it ended.
            let user = said(
                user,
                &cell
                    .as_deref()
                    .map(crate::follow_ups::ended)
                    .unwrap_or_default(),
            );
            let answer = match outcome {
                Ok(answer) => answer,
                Err(reason) => {
                    tracing::warn!(reason = ?reason, "a chat run failed");
                    if current {
                        self.carry(record);
                    }
                    // It may have failed before its model saw anything, as
                    // when the model was still loading: its notes go to the
                    // next run. One told twice is told of a file still there.
                    notes.give_back(given);
                    let reached = self.reached(now);
                    let model = crate::model_choice::named(
                        self.choice.as_deref(),
                        self.model.as_deref(),
                        self.other.as_ref(),
                    );
                    let quiet = cell
                        .as_deref()
                        .is_some_and(crate::machine::took_quiet_model);
                    let told = failed_with(ending, model.as_deref(), reached.as_ref(), quiet);
                    return answer_with(delivery, &told);
                }
            };
            let spent = !had_steps_to_spare(ending);
            let local = ChannelId::local().0;
            return match delivery.deliver_answer(&local, &answer, cell.as_deref(), spent) {
                Ok(shown) => {
                    if shown && current {
                        let run = cell.clone();
                        self.ended(spent, cell, &user, &answer, sent_in);
                        let compacted = self.keep(user, answer, run.as_deref(), record.as_ref());
                        if spent {
                            self.carry(record);
                        } else {
                            self.settled();
                        }
                        if let Some(notice) = self.told.notice(compacted) {
                            if let Err(error) = delivery.deliver_notice(&local, notice) {
                                tracing::warn!(error = ?error, "a compaction notice was not delivered");
                            }
                        }
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
        if let Some(reached) = self.reached(now) {
            tracing::info!(span = ?reached.span, "a message wasn't answered: a usage limit is reached");
            return answer_with(delivery, &reached.told());
        }
        self.start(message, sent_in, notes, launcher, delivery, now)
    }

    /// Starts a run for the owner's `message`, sent in `sent_in`, given
    /// `notes` before it.
    fn start<L, D>(
        &mut self,
        message: Message,
        sent_in: u64,
        notes: &dyn Notes,
        launcher: &L,
        delivery: &D,
        now: Timestamp,
    ) -> Step
    where
        L: Launcher<Run = R>,
        D: Delivery,
    {
        let (history, earlier) = if sent_in == self.conversation {
            (self.history(), self.earlier())
        } else {
            (Vec::new(), Vec::new())
        };
        let given = notes.take();
        // Keep going (WO-053): the run is given its fixed task, and the
        // owner is shown what they said.
        let asked = if message.sender.0 == KEEP_GOING_SENDER {
            hatchling_prompt::KEEP_GOING_TASK
        } else {
            message.text.as_str()
        };
        let task = if given.is_empty() {
            asked.to_owned()
        } else {
            format!("{}\n\n{asked}", crate::file_in::told(&given))
        };
        // What the runs it continues did, as the broker recorded it, for a
        // run in the conversation they were in.
        let records = if sent_in == self.conversation {
            self.carried.clone()
        } else {
            Vec::new()
        };
        let request = RunRequest {
            schedule_id: String::from(CHAT_LABEL),
            task,
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
            records,
        };
        // The Room's copy of the skills the owner enabled, brought in line
        // before the run that may use it (WO-046).
        crate::skill_room::before_chat_run(
            crate::room_ops::offered(true, crate::room_ops::open),
            crate::spawner::spawner()
                .map(|spawner| -> &dyn crate::skill_room::RoomSkills { spawner }),
            crate::skills::live(),
        );
        match launcher.launch(request) {
            Ok(run) => {
                if let (Some(hub), Some(cell)) = (&self.waiting, run.cell_id()) {
                    crate::follow_ups::began(&cell, hub, sent_in);
                }
                self.running = Some((run, message.text, sent_in, given));
                Step::Started
            }
            Err(reason) => {
                tracing::warn!(reason = ?reason, "a chat run could not start");
                notes.give_back(given);
                answer_with(delivery, CHAT_FAILED)
            }
        }
    }
}

/// The notes a run was given, each by its key, the oldest first.
type Noted = Vec<(String, String)>;

/// The broker's own notes for the next run that answers the owner
/// (WO-041), each by its key: taken as a run starts, and given back when
/// it doesn't start, or ends without an answer (#498).
pub trait Notes {
    /// Takes every note, the oldest first.
    fn take(&self) -> Vec<(String, String)>;
    /// Gives back `notes`, taken for a run that didn't start or didn't
    /// answer.
    fn give_back(&self, notes: Vec<(String, String)>);
}

/// No notes.
impl Notes for () {
    fn take(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn give_back(&self, _notes: Vec<(String, String)>) {}
}

/// The notes the local channel's hub keeps.
impl Notes for LocalHub {
    fn take(&self) -> Vec<(String, String)> {
        self.take_notes()
    }

    fn give_back(&self, notes: Vec<(String, String)>) {
        self.give_back_notes(notes);
    }
}

/// Whether a run that answered had steps and room to spare, by how it
/// ended: it had, unless its agent says by its status that one ran out
/// as it answered (WO-051). The status is the agent's own word.
fn had_steps_to_spare(ending: Option<hatchling_scheduler::Ending>) -> bool {
    !matches!(
        ending,
        Some(hatchling_scheduler::Ending::Exited(code))
            if hatchling_core::run_failure::answered_at_a_limit(code)
    )
}

/// What the owner said in an exchange: the message its run was started
/// for, then each further one the run was given, a blank line between.
fn said(first: String, more: &[String]) -> String {
    more.iter().fold(first, |mut all, next| {
        all.push_str("\n\n");
        all.push_str(next);
        all
    })
}

/// Delivers `answer`, Hatchling's own words, on the local channel: a
/// notice, never the agent's.
fn answer_with<D: Delivery>(delivery: &D, answer: &str) -> Step {
    match delivery.deliver_notice(&ChannelId::local().0, answer) {
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
        .with_model(config.scheduler.model_name.clone())
        .with_other(config.scheduler.other_model.clone())
        .with_window(config.scheduler.model_context_window)
        .with_choice(
            config
                .scheduler
                .enabled()
                .then(|| config.scheduler.owner_dir()),
        )
        .with_usage(config.scheduler.usage.clone())
        .with_follow_ups(hub.clone());
    loop {
        let step = chat_turn(
            &mut chat,
            config,
            hub,
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

/// One turn of the owner's chat, with what `hub` holds: a step, and
/// then, when a run just answered them with steps to spare, the idea
/// its conversation was started for is answered
/// ([`crate::suggestions::conversation_answered`]).
fn chat_turn<R, K, L, D>(
    chat: &mut Chat<R, K>,
    config: &crate::Config,
    hub: &LocalHub,
    launcher: &L,
    delivery: &D,
    now: Timestamp,
) -> Step
where
    R: Run,
    K: Keeps,
    L: Launcher<Run = R>,
    D: Delivery,
{
    let step = chat.step_noted(
        || hub.take_in_conversation(),
        hub,
        hub.conversation(),
        launcher,
        delivery,
        now,
    );
    if let Some(conversation) = chat.take_answered() {
        crate::suggestions::conversation_answered(config, hub, conversation);
    }
    // What the owner asked may be unfinished: theirs to keep for later,
    // with where the run said it stopped.
    if let Some(offer) = chat.take_offer() {
        hub.offer(
            offer.run,
            crate::suggestions::kept_words(&offer.said),
            crate::suggestions::kept_words(&offer.answer),
            offer.conversation,
        );
    }
    step
}

#[cfg(test)]
mod tests {
    use hatchling_core::conversation;
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

        fn cell_id(&self) -> Option<String> {
            Some(String::from("run-scripted"))
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
        /// The run each answer was said to come from, in order.
        runs: RefCell<Vec<Option<String>>>,
        /// The texts delivered as Hatchling's own notices.
        notices: RefCell<Vec<String>>,
        /// Whether each answer was said to have run out of steps, in
        /// order.
        spent: RefCell<Vec<bool>>,
    }

    impl Delivery for Answers {
        fn deliver(&self, channel: &str, text: &str) -> Result<(), String> {
            self.deliver_verbatim(channel, text).map(|_| ())
        }

        fn deliver_answer(
            &self,
            channel: &str,
            text: &str,
            run: Option<&str>,
            out_of_steps: bool,
        ) -> Result<bool, String> {
            self.runs.borrow_mut().push(run.map(str::to_owned));
            self.spent.borrow_mut().push(out_of_steps);
            self.deliver_verbatim(channel, text)
        }

        fn deliver_notice(&self, channel: &str, text: &str) -> Result<(), String> {
            self.notices.borrow_mut().push(text.to_owned());
            self.deliver(channel, text)
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

    /// A run that starts is given the broker's notes before the owner's
    /// words, in its task alone: what the owner said is kept as theirs.
    /// A run that can't start gives the notes back, and they are given
    /// once.
    #[test]
    fn a_run_is_given_the_brokers_notes_and_the_owners_words_stay_theirs() {
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        hub.note(String::from("0001-a"), String::from("[note a]"));
        hub.note(String::from("0002-b"), String::from("[note b]"));
        let launches = Launches::default();
        launches.outcomes.borrow_mut().extend([
            Err(String::from("no cell")),
            Ok(answering("Read.")),
            Ok(answering("Next.")),
        ]);
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        let now = Timestamp::now();
        let step = |chat: &mut Chat<Scripted>, text: Option<&str>| {
            chat.step_noted(
                || text.map(|text| (message(text), 0)),
                &hub,
                0,
                &launches,
                &answers,
                now,
            )
        };
        assert_ne!(step(&mut chat, Some("hi")), Step::Started);
        assert_eq!(step(&mut chat, Some("Read them.")), Step::Started);
        assert_eq!(step(&mut chat, None), Step::Answered);
        assert!(
            hub.take_notes().is_empty(),
            "a note was kept after it was given"
        );
        assert_eq!(
            exchange(&mut chat, "And then?", &launches, &answers),
            Step::Answered
        );
        let tasks: Vec<String> = launches
            .requests
            .borrow()
            .iter()
            .map(|request| request.task.clone())
            .collect();
        assert_eq!(
            tasks,
            [
                "[note a]\n[note b]\n\nhi",
                "[note a]\n[note b]\n\nRead them.",
                "And then?"
            ]
        );
        assert_eq!(
            histories(&launches).last(),
            Some(&vec![(String::from("Read them."), String::from("Read."))])
        );
    }

    /// A run that starts and then fails, as when the model is still
    /// loading, gives its notes back (#498): the next run is told of the
    /// file, once. One the owner removed while the run ran isn't, and a run
    /// that answered keeps what it was given.
    #[test]
    fn a_run_that_fails_gives_its_notes_to_the_next() {
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        hub.note(String::from("0001-a"), String::from("[note a]"));
        hub.note(String::from("0002-b"), String::from("[note b]"));
        let launches = Launches::default();
        let failing = Scripted {
            polls: 0,
            outcome: Err(String::from(
                "the model endpoint failed its capability handshake",
            )),
        };
        launches.outcomes.borrow_mut().extend([
            Ok(failing),
            Ok(answering("Found it.")),
            Ok(answering("Next.")),
        ]);
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        let now = Timestamp::now();
        let step = |chat: &mut Chat<Scripted>, text: Option<&str>| {
            chat.step_noted(
                || text.map(|text| (message(text), 0)),
                &hub,
                0,
                &launches,
                &answers,
                now,
            )
        };
        assert_eq!(step(&mut chat, Some("Read my notes.")), Step::Started);
        // Removed from the inbox while the run ran.
        hub.forget_note("0002-b");
        hub.note(String::from("0003-c"), String::from("[note c]"));
        assert_eq!(
            step(&mut chat, None),
            Step::Answered,
            "the failure wasn't said"
        );
        assert_eq!(step(&mut chat, Some("Again?")), Step::Started);
        assert_eq!(step(&mut chat, None), Step::Answered);
        assert!(
            hub.take_notes().is_empty(),
            "a note was kept after a run answered with it"
        );
        assert_eq!(
            exchange(&mut chat, "And then?", &launches, &answers),
            Step::Answered
        );
        let tasks: Vec<String> = launches
            .requests
            .borrow()
            .iter()
            .map(|request| request.task.clone())
            .collect();
        assert_eq!(
            tasks,
            [
                "[note a]\n[note b]\n\nRead my notes.",
                "[note a]\n[note c]\n\nAgain?",
                "And then?"
            ]
        );
    }

    /// The chat's own turn gives the run it starts the notes its hub
    /// keeps.
    #[test]
    fn the_chats_own_turn_gives_a_run_the_hubs_notes() {
        let dir = crate::suggestions::tests::scratch("chat-notes");
        let config = crate::suggestions::tests::config(&dir, None);
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        hub.note(String::from("0001-a"), String::from("[note a]"));
        let launches = Launches::default();
        launches.outcomes.borrow_mut().push(Ok(answering("Read.")));
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        assert_eq!(
            hub.inject(message("Read it.")),
            hatchling_channel::IngressEvent::Queued
        );
        assert_eq!(
            chat_turn(
                &mut chat,
                &config,
                &hub,
                &launches,
                &answers,
                Timestamp::now()
            ),
            Step::Started
        );
        assert_eq!(
            launches
                .requests
                .borrow()
                .first()
                .map(|request| request.task.clone()),
            Some(String::from("[note a]\n\nRead it."))
        );
        assert!(hub.take_notes().is_empty());
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
    struct Recorded(
        RefCell<Vec<(u64, Conversation)>>,
        RefCell<Vec<Option<String>>>,
    );

    impl Keeps for &Recorded {
        fn keep(
            &self,
            sent_in: u64,
            kept: &Conversation,
            run: Option<&str>,
            _record: Option<&RunRecord>,
        ) {
            self.0.borrow_mut().push((sent_in, kept.clone()));
            self.1.borrow_mut().push(run.map(str::to_owned));
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
        // Each kept with the run that answered it (WO-043).
        assert_eq!(
            *recorded.1.borrow(),
            [
                Some(String::from("run-scripted")),
                Some(String::from("run-scripted"))
            ]
        );
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
        None::<&Recorded>.keep(5, &after_q3, None);
        Some(&recorded).keep(5, &after_q3, None);
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

    /// #595: a chat on a model with a large window keeps, and gives, as
    /// much of the conversation as the window's bounds hold.
    #[test]
    fn a_chat_keeps_what_its_windows_bounds_hold() {
        let most = Bounds::MOST;
        let launches = Launches::default();
        let count = most.exchanges + 3;
        launches
            .outcomes
            .borrow_mut()
            .extend((0..count).map(|n| Ok(answering(&format!("a{n}")))));
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]).with_window(Some(131_072));
        for n in 0..count {
            assert_eq!(
                exchange(&mut chat, &format!("q{n}"), &launches, &answers),
                Step::Answered
            );
        }
        assert_eq!(chat.kept.exchanges.len(), most.exchanges);
        assert_eq!(chat.history().len(), most.exchanges);
        assert_eq!(chat.earlier(), ["q0", "q1", "q2"]);
        // The last run was given every exchange kept before it.
        assert_eq!(
            histories(&launches).last().map(Vec::len),
            Some(most.exchanges)
        );
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

    /// A run that ends at once: its outcome, and how it ended.
    #[derive(Debug)]
    struct EndedAs {
        outcome: Result<String, String>,
        ending: Option<hatchling_scheduler::Ending>,
    }

    impl Run for EndedAs {
        fn poll(&mut self) -> Option<Result<String, String>> {
            Some(self.outcome.clone())
        }

        fn ending(&self) -> Option<hatchling_scheduler::Ending> {
            self.ending
        }
    }

    /// Launches each [`EndedAs`] run in turn.
    struct InTurn(RefCell<Vec<EndedAs>>);

    impl Launcher for InTurn {
        type Run = EndedAs;

        fn launch(&self, _: RunRequest) -> Result<EndedAs, String> {
            let mut runs = self.0.borrow_mut();
            if runs.is_empty() {
                Err(String::new())
            } else {
                Ok(runs.remove(0))
            }
        }
    }

    /// A run that answered, having ended as `ending` says.
    fn answered_as(ending: Option<hatchling_scheduler::Ending>) -> EndedAs {
        EndedAs {
            outcome: Ok(String::from("done")),
            ending,
        }
    }

    /// A run that failed.
    fn failed_run() -> EndedAs {
        EndedAs {
            outcome: Err(String::from("the run exited 1")),
            ending: Some(hatchling_scheduler::Ending::Exited(1)),
        }
    }

    /// v29: the chat says, once, which conversation a run answered the
    /// owner in: when they were shown its answer, in the conversation
    /// still open, and the run had steps to spare.
    #[test]
    fn the_chat_says_once_which_conversation_a_run_answered_in() {
        use hatchling_core::run_failure::{ANSWERED_OUT_OF_STEPS, RunFailure};
        use hatchling_scheduler::Ending;
        let now = Timestamp::now();
        let shown = Answers::default();
        // Asked in the conversation `sent_in`, answered with `open` open.
        let ask =
            |chat: &mut Chat<EndedAs>, run: EndedAs, answers: &Answers, sent_in: u64, open: u64| {
                let launches = InTurn(RefCell::new(vec![run]));
                assert_eq!(
                    chat.step(
                        || Some((message("q"), sent_in)),
                        sent_in,
                        &launches,
                        answers,
                        now
                    ),
                    Step::Started
                );
                chat.step(|| None, open, &launches, answers, now)
            };
        let mut chat = Chat::new(vec![send_local()]);
        assert_eq!(chat.take_answered(), None, "nothing answered yet");
        assert_eq!(
            ask(&mut chat, answered_as(None), &shown, 3, 3),
            Step::Answered
        );
        assert_eq!(chat.take_answered(), Some(3));
        assert_eq!(chat.take_answered(), None, "said once");
        // Given as its steps ran out, an answer may leave work undone.
        let spent = Ending::Exited(ANSWERED_OUT_OF_STEPS.into());
        assert_eq!(
            ask(&mut chat, answered_as(Some(spent)), &shown, 3, 3),
            Step::Answered
        );
        assert_eq!(chat.take_answered(), None);
        // Only that status says so. No other comes with an answer; were
        // one to, it wouldn't be read as this.
        let other = Ending::Exited(RunFailure::OutOfSteps.code().into());
        assert_eq!(
            ask(&mut chat, answered_as(Some(other)), &shown, 3, 3),
            Step::Answered
        );
        assert_eq!(chat.take_answered(), Some(3));
        // A run that failed answered nothing.
        assert_eq!(
            ask(&mut chat, failed_run(), &shown, 3, 3),
            Step::Answered,
            "the owner is told it failed"
        );
        assert_eq!(chat.take_answered(), None);
        // Nor did one whose answer the owner wasn't shown,
        let replaced = Answers::default();
        replaced.replace.set(true);
        assert_eq!(
            ask(&mut chat, answered_as(None), &replaced, 3, 3),
            Step::Answered
        );
        assert_eq!(chat.take_answered(), None);
        // or one whose answer reached no one,
        let refusing = Answers {
            refuse: true,
            ..Answers::default()
        };
        assert_eq!(
            ask(&mut chat, answered_as(None), &refusing, 3, 3),
            Step::Undelivered
        );
        assert_eq!(chat.take_answered(), None);
        // or one that answered after the owner started another.
        assert_eq!(
            ask(&mut chat, answered_as(None), &shown, 3, 4),
            Step::Answered
        );
        assert_eq!(chat.take_answered(), None);
        assert_eq!(
            ask(&mut chat, answered_as(None), &shown, 4, 4),
            Step::Answered
        );
        assert_eq!(chat.take_answered(), Some(4));
    }

    /// v29: an idea the owner started a conversation for is answered
    /// when a run answers them in it with steps to spare, as the chat's
    /// own turn finds it: with the hub that holds the conversation, and
    /// the page as it is kept.
    #[test]
    fn a_started_idea_is_answered_when_its_conversations_run_answers() {
        use hatchling_core::run_failure::ANSWERED_OUT_OF_STEPS;
        use hatchling_core::ui::{SuggestionOp, SuggestionResult, SuggestionState};
        use hatchling_scheduler::Ending;
        let dir = crate::suggestions::tests::scratch("chat-answered");
        let config = crate::suggestions::tests::config(&dir, None);
        let state = |id: &str| match crate::suggestions::handle(&config, &SuggestionOp::List) {
            SuggestionResult::Listed { suggestions } => suggestions
                .iter()
                .find(|kept| kept.suggestion_id == id)
                .map(|kept| kept.state),
            SuggestionResult::Changed { .. }
            | SuggestionResult::NotFound
            | SuggestionResult::Off
            | SuggestionResult::Failed => None,
        };
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let answers = Answers::default();
        let now = Timestamp::now();
        let mut chat = Chat::new(vec![send_local()]);
        // The owner sends a message, and its run ends as `run` does.
        let mut ask = |run: EndedAs| {
            let launches = InTurn(RefCell::new(vec![run]));
            assert_eq!(
                hub.inject(message("go")),
                hatchling_channel::IngressEvent::Queued
            );
            assert_eq!(
                chat_turn(&mut chat, &config, &hub, &launches, &answers, now),
                Step::Started
            );
            assert_eq!(
                chat_turn(&mut chat, &config, &hub, &launches, &answers, now),
                Step::Answered
            );
        };
        let plan = crate::suggestions::tests::started(&dir, "A run plan");
        // In a conversation started for nothing, an answer is no idea's.
        ask(answered_as(None));
        assert_eq!(state(&plan), Some(SuggestionState::Started));
        hub.new_conversation_for(plan.clone());
        // An answer given as the steps ran out leaves it started,
        ask(answered_as(Some(Ending::Exited(
            ANSWERED_OUT_OF_STEPS.into(),
        ))));
        assert_eq!(state(&plan), Some(SuggestionState::Started));
        // and so does a run that failed.
        ask(failed_run());
        assert_eq!(state(&plan), Some(SuggestionState::Started));
        // One with steps to spare answers it.
        ask(answered_as(None));
        assert_eq!(state(&plan), Some(SuggestionState::Answered));
        // An idea the owner set aside while its run worked stays so.
        let list = crate::suggestions::tests::started(&dir, "A reading list");
        hub.new_conversation_for(list.clone());
        let _ = crate::suggestions::handle(&config, &SuggestionOp::Dismiss { id: list.clone() });
        ask(answered_as(None));
        assert_eq!(state(&list), Some(SuggestionState::Dismissed));
        assert_eq!(state(&plan), Some(SuggestionState::Answered));
        // And once the owner starts a conversation for nothing, the one
        // before it is no idea's any more.
        let trip = crate::suggestions::tests::started(&dir, "A trip");
        hub.new_conversation_for(trip.clone());
        hub.new_conversation();
        ask(answered_as(None));
        assert_eq!(state(&trip), Some(SuggestionState::Started));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A run in the cell named that ends at once, with `outcome`, as
    /// `ending` says.
    #[derive(Debug)]
    struct Spent {
        outcome: Result<String, String>,
        ending: Option<hatchling_scheduler::Ending>,
        cell: &'static str,
    }

    impl Run for Spent {
        fn poll(&mut self) -> Option<Result<String, String>> {
            Some(self.outcome.clone())
        }

        fn ending(&self) -> Option<hatchling_scheduler::Ending> {
            self.ending
        }

        fn cell_id(&self) -> Option<String> {
            Some(self.cell.to_owned())
        }
    }

    /// Launches each [`Spent`] run in turn.
    struct SpentRuns(RefCell<Vec<Spent>>);

    impl Launcher for SpentRuns {
        type Run = Spent;

        fn launch(&self, _: RunRequest) -> Result<Spent, String> {
            let mut runs = self.0.borrow_mut();
            if runs.is_empty() {
                Err(String::new())
            } else {
                Ok(runs.remove(0))
            }
        }
    }

    /// The ending of a run whose agent answered as its steps ran out.
    fn ran_out() -> hatchling_scheduler::Ending {
        hatchling_scheduler::Ending::Exited(
            hatchling_core::run_failure::ANSWERED_OUT_OF_STEPS.into(),
        )
    }

    /// A run in `cell` that answered, ending as `ending` says.
    fn spent(ending: Option<hatchling_scheduler::Ending>, cell: &'static str) -> Spent {
        Spent {
            outcome: Ok(String::from("this far")),
            ending,
            cell,
        }
    }

    /// v30: an answer shown to the owner that ran out of steps is said
    /// so as it is delivered, and offers what they said in it, with its
    /// run and conversation, once. An answer with steps to spare is said
    /// not to have, and offers nothing; nor does a failed run, an answer
    /// not shown, one that reached no one, or one in a conversation the
    /// owner has left.
    #[test]
    fn an_answer_that_ran_out_of_steps_offers_what_was_asked() {
        let now = Timestamp::now();
        let ask =
            |chat: &mut Chat<Spent>, run: Spent, answers: &Answers, sent_in: u64, open: u64| {
                let launches = SpentRuns(RefCell::new(vec![run]));
                assert_eq!(
                    chat.step(
                        || Some((message("make the tools"), sent_in)),
                        sent_in,
                        &launches,
                        answers,
                        now
                    ),
                    Step::Started
                );
                chat.step(|| None, open, &launches, answers, now)
            };
        let shown = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        assert_eq!(chat.take_offer(), None, "nothing offered yet");
        assert_eq!(
            ask(&mut chat, spent(Some(ran_out()), "run-1"), &shown, 2, 2),
            Step::Answered
        );
        assert_eq!(
            chat.take_offer(),
            Some(Offer {
                run: String::from("run-1"),
                said: String::from("make the tools"),
                answer: String::from("this far"),
                conversation: 2,
            })
        );
        assert_eq!(chat.take_offer(), None, "offered once");
        assert_eq!(chat.take_answered(), None, "no answer with steps to spare");
        // With steps to spare there is nothing unfinished to keep.
        assert_eq!(
            ask(&mut chat, spent(None, "run-2"), &shown, 2, 2),
            Step::Answered
        );
        assert_eq!(chat.take_offer(), None);
        assert_eq!(chat.take_answered(), Some(2));
        // A run that failed answered nothing.
        let failed = Spent {
            outcome: Err(String::from("the run exited 1")),
            ending: Some(ran_out()),
            cell: "run-3",
        };
        assert_eq!(ask(&mut chat, failed, &shown, 2, 2), Step::Answered);
        assert_eq!(chat.take_offer(), None);
        // Nor did one whose answer the owner wasn't shown,
        let replaced = Answers::default();
        replaced.replace.set(true);
        assert_eq!(
            ask(&mut chat, spent(Some(ran_out()), "run-4"), &replaced, 2, 2),
            Step::Answered
        );
        assert_eq!(chat.take_offer(), None);
        // or one that reached no one,
        let refusing = Answers {
            refuse: true,
            ..Answers::default()
        };
        assert_eq!(
            ask(&mut chat, spent(Some(ran_out()), "run-5"), &refusing, 2, 2),
            Step::Undelivered
        );
        assert_eq!(chat.take_offer(), None);
        // or one that answered after the owner started another.
        assert_eq!(
            ask(&mut chat, spent(Some(ran_out()), "run-6"), &shown, 2, 3),
            Step::Answered
        );
        assert_eq!(chat.take_offer(), None);
        // Each answer was said to have run out of steps, or not, as it ended.
        assert_eq!(*shown.spent.borrow(), [true, false, true]);
        assert_eq!(*replaced.spent.borrow(), [true]);
    }

    /// v30: the chat's own turn hands the hub what the owner asked in a
    /// run that ran out of steps, as a kept card would hold it, and (v31)
    /// the run's answer, held the same way; a run with steps to spare
    /// offers nothing.
    #[test]
    fn the_chats_turn_offers_an_unfinished_ask_to_keep() {
        let dir = crate::suggestions::tests::scratch("chat-offer");
        let config = crate::suggestions::tests::config(&dir, None);
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let answers = Answers::default();
        let now = Timestamp::now();
        let mut chat = Chat::new(vec![send_local()]);
        let long = format!(
            "  {}  ",
            "x".repeat(crate::suggestions::MAX_TEXT_BYTES + 50)
        );
        let mut turn = |run: Spent, said: &str| {
            let launches = SpentRuns(RefCell::new(vec![run]));
            assert_eq!(
                hub.inject(message(said)),
                hatchling_channel::IngressEvent::Queued
            );
            assert_eq!(
                chat_turn(&mut chat, &config, &hub, &launches, &answers, now),
                Step::Started
            );
            assert_eq!(
                chat_turn(&mut chat, &config, &hub, &launches, &answers, now),
                Step::Answered
            );
        };
        let answer = format!("  {}  ", "y".repeat(crate::suggestions::MAX_TEXT_BYTES + 9));
        let long_answer = Spent {
            outcome: Ok(answer.clone()),
            ending: Some(ran_out()),
            cell: "run-7",
        };
        turn(long_answer, &long);
        turn(spent(None, "run-8"), "all done");
        assert_eq!(
            hub.offer_of("run-7"),
            Some((
                crate::suggestions::kept_words(&long),
                crate::suggestions::kept_words(&answer)
            ))
        );
        assert_eq!(hub.offered("run-8"), None);
        let _ = std::fs::remove_dir_all(&dir);
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

    /// A run that fails as its ending says, at once, in the cell named.
    #[derive(Debug)]
    struct EndedIn(hatchling_scheduler::Ending, &'static str);

    impl Run for EndedIn {
        fn poll(&mut self) -> Option<Result<String, String>> {
            Some(Err(String::from("the run exited")))
        }

        fn ending(&self) -> Option<hatchling_scheduler::Ending> {
            Some(self.0)
        }

        fn cell_id(&self) -> Option<String> {
            Some(self.1.to_owned())
        }
    }

    /// Launches one [`EndedIn`] run.
    struct OneEndedIn(RefCell<Option<EndedIn>>);

    impl Launcher for OneEndedIn {
        type Run = EndedIn;

        fn launch(&self, _: RunRequest) -> Result<EndedIn, String> {
            self.0.borrow_mut().take().ok_or_else(String::new)
        }
    }

    /// A run that answers "done" at once, in the cell named.
    #[derive(Debug)]
    struct AnswersIn(&'static str);

    impl Run for AnswersIn {
        fn poll(&mut self) -> Option<Result<String, String>> {
            Some(Ok(String::from("done")))
        }

        fn cell_id(&self) -> Option<String> {
            Some(self.0.to_owned())
        }
    }

    /// Launches one [`AnswersIn`] run.
    struct OneAnswersIn(RefCell<Option<AnswersIn>>);

    impl Launcher for OneAnswersIn {
        type Run = AnswersIn;

        fn launch(&self, _: RunRequest) -> Result<AnswersIn, String> {
            self.0.borrow_mut().take().ok_or_else(String::new)
        }
    }

    /// What the owner sends while a run works is that run's, once the chat
    /// knows where their messages wait: the run's session is given it, and
    /// the chat keeps it as part of what they said in that exchange. A run
    /// that has ended is given no more, and a chat that wasn't told where
    /// messages wait names no run.
    #[test]
    fn what_the_owner_sends_while_a_run_works_is_kept_with_its_exchange() {
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let answers = Answers::default();
        let now = Timestamp::now();
        let mut chat: Chat<AnswersIn> = Chat::new(vec![send_local()]).with_follow_ups(hub.clone());
        let launches = OneAnswersIn(RefCell::new(Some(AnswersIn("run-follow-chat"))));
        chat.step(
            || Some((message("Sort my notes"), 0)),
            0,
            &launches,
            &answers,
            now,
        );
        let _ = hub.inject(message("by date"));
        let _ = hub.inject(message("newest first"));
        // The run's session asks, between two of its steps.
        assert_eq!(
            crate::follow_ups::more("run-follow-chat"),
            ["by date", "newest first"]
        );
        assert_eq!(hub.queued(), 0, "no run is started for them");
        chat.step(|| hub.take_in_conversation(), 0, &launches, &answers, now);
        let kept = chat.history();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept.first()
                .map(|exchange| (exchange.user.as_str(), exchange.assistant.as_str())),
            Some(("Sort my notes\n\nby date\n\nnewest first", "done"))
        );
        let _ = hub.inject(message("thanks"));
        assert!(
            crate::follow_ups::more("run-follow-chat").is_empty(),
            "the run has ended"
        );
        assert_eq!(hub.queued(), 1, "it waits for a run of its own");

        let mut plain: Chat<AnswersIn> = Chat::new(vec![send_local()]);
        let launches = OneAnswersIn(RefCell::new(Some(AnswersIn("run-follow-plain"))));
        plain.step(|| Some((message("one"), 0)), 0, &launches, &answers, now);
        assert!(crate::follow_ups::more("run-follow-plain").is_empty());
        assert_eq!(hub.queued(), 1);
        plain.step(|| None, 0, &launches, &answers, now);
        assert_eq!(
            plain
                .history()
                .first()
                .map(|exchange| exchange.user.clone()),
            Some(String::from("one"))
        );
        assert_eq!(super::said(String::from("one"), &[]), "one");
    }

    /// A run that fails is given no more either: what it took is gone
    /// with it, and what the owner sends next waits for the next run.
    #[test]
    fn a_failed_run_is_given_no_more() {
        use hatchling_core::run_failure::RunFailure;
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let answers = Answers::default();
        let now = Timestamp::now();
        let mut chat: Chat<EndedIn> = Chat::new(vec![send_local()]).with_follow_ups(hub.clone());
        let ending =
            hatchling_scheduler::Ending::Exited(RunFailure::ModelUnavailable.code().into());
        let launches = OneEndedIn(RefCell::new(Some(EndedIn(ending, "run-follow-failed"))));
        chat.step(|| Some((message("one"), 0)), 0, &launches, &answers, now);
        let _ = hub.inject(message("two"));
        assert_eq!(crate::follow_ups::more("run-follow-failed"), ["two"]);
        chat.step(|| None, 0, &launches, &answers, now);
        assert!(
            chat.history().is_empty(),
            "a failed run's exchange isn't kept"
        );
        let _ = hub.inject(message("three"));
        assert!(crate::follow_ups::more("run-follow-failed").is_empty());
        assert_eq!(hub.queued(), 1);
    }

    /// A chat run whose model was still answering when the broker stopped
    /// waiting is told so, by its cell, and once: the next run in a cell of
    /// that name, and a run in any other cell, are told as they always are.
    #[test]
    fn a_chat_run_whose_model_was_still_answering_is_told_so_once() {
        use hatchling_core::run_failure::RunFailure;
        use hatchling_scheduler::Ending;
        let unreachable = Ending::Exited(RunFailure::ModelUnavailable.code().into());
        let failed = |cell: &'static str| {
            let mut chat: Chat<EndedIn> =
                Chat::new(vec![send_local()]).with_model(Some(String::from("m")));
            let launches = OneEndedIn(RefCell::new(Some(EndedIn(unreachable, cell))));
            let answers = Answers::default();
            let now = Timestamp::now();
            chat.step(|| Some((message("one"), 0)), 0, &launches, &answers, now);
            chat.step(|| None, 0, &launches, &answers, now);
            let delivered = answers.delivered.borrow();
            delivered
                .first()
                .map(|(_, told)| told.clone())
                .unwrap_or_default()
        };
        let usual = chat_failed(Some(unreachable), Some("m"));
        assert_eq!(failed("run-quiet-chat"), usual, "nothing known of it");
        crate::machine::model_was_quiet("run-quiet-chat");
        assert_eq!(failed("run-quiet-other"), usual, "another cell's");
        assert_eq!(failed("run-quiet-chat"), model_quiet(Some("m")));
        assert_eq!(failed("run-quiet-chat"), usual, "told once");
    }

    /// The person is told why a run failed in plain words, and what to do,
    /// from how it ended alone: nothing the run wrote reaches them.
    /// A model whose answers all went into its thinking is told as that,
    /// and not as one that said nothing.
    #[test]
    fn a_model_that_only_thought_is_told_as_that() {
        use hatchling_core::run_failure::RunFailure;
        let exited =
            |failure: RunFailure| Some(hatchling_scheduler::Ending::Exited(failure.code().into()));
        let thought = chat_failed(exited(RunFailure::ModelThoughtOnly), Some("qwen3.5:9b"));
        assert!(
            thought
                .starts_with("The model (qwen3.5:9b) thought about its answer and then gave none")
                && thought.contains("with its thinking turned off")
                && thought.contains("docs/model-thinking.md")
                && !thought.contains("answered with nothing"),
            "{thought}"
        );
        assert!(!chat_failed(exited(RunFailure::ModelThoughtOnly), None).contains('('));
        let silent = chat_failed(exited(RunFailure::ModelSilent), Some("qwen3.5:9b"));
        assert!(!silent.contains("thought about"), "{silent}");
    }

    /// A run out of steps that couldn't say where it got to is told as
    /// that, with what to do next.
    #[test]
    fn a_run_out_of_steps_is_told_to_go_on() {
        use hatchling_core::run_failure::RunFailure;
        let ending = Some(hatchling_scheduler::Ending::Exited(
            RunFailure::OutOfSteps.code().into(),
        ));
        let told = chat_failed(ending, Some("m"));
        assert!(
            told.starts_with("The agent used all its steps before it finished")
                && told.contains("didn't say where it got to")
                && told.contains("Ask it to go on"),
            "{told}"
        );
    }

    #[test]
    fn a_failed_run_is_explained_in_plain_words() {
        use hatchling_core::run_failure::RunFailure;
        use hatchling_scheduler::Ending;
        use hatchling_scheduler::SentTo;
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
        let incapable = chat_failed(exited(RunFailure::ModelIncapable), Some("llama3.2"));
        assert!(
            incapable.starts_with("The model (llama3.2) doesn't call tools")
                && incapable.contains("Choosing a model")
                && !incapable.contains("running and reachable"),
            "{incapable}"
        );
        let silent = chat_failed(exited(RunFailure::ModelSilent), Some("qwen3.5:9b"));
        assert!(
            silent.starts_with("The model (qwen3.5:9b) answered with nothing")
                && silent.contains("try again")
                && !silent.contains("running and reachable"),
            "{silent}"
        );
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
        assert_eq!(
            chat_failed(Some(Ending::OwnerStopped), Some("m")),
            "You stopped it."
        );
        let stopped = |to| chat_failed(Some(Ending::Stopped(to)), Some("m"));
        assert_eq!(
            stopped(SentTo::Website),
            "Hatchling stopped this run: it tried to send a saved secret to a website."
        );
        assert_eq!(
            stopped(SentTo::Search),
            "Hatchling stopped this run: it tried to send a saved secret to a search service."
        );
        assert_eq!(
            stopped(SentTo::Page),
            "Hatchling stopped this run: it tried to type a saved secret into a web page."
        );
        assert_eq!(
            stopped(SentTo::Harness),
            "Hatchling stopped this run: it tried to give a saved secret to a coding tool."
        );
        assert_eq!(
            stopped(SentTo::HarnessReturn),
            "Hatchling stopped this run: a coding tool it started handed back a saved secret."
        );
        let said: BTreeSet<String> = [
            model,
            incapable,
            silent,
            tools,
            steps,
            slow,
            room,
            String::from(CHAT_FAILED),
        ]
        .into_iter()
        .collect();
        assert_eq!(said.len(), 8, "each says something of its own");

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

    /// A failed run names the model the owner chose, over the operator's,
    /// as it stands when the run fails.
    #[test]
    fn a_failure_names_the_model_the_owner_chose() {
        use hatchling_core::run_failure::RunFailure;
        use hatchling_scheduler::Ending;
        let dir =
            std::env::temp_dir().join(format!("hatchling-chat-choice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let owner = dir.join("owner");
        std::fs::create_dir_all(&owner).unwrap();
        let failed = |chat: &mut Chat<Ended>| {
            let launches = OneEnded(RefCell::new(Some(Ended(Ending::Exited(
                RunFailure::ToolsMisused.code().into(),
            )))));
            let answers = Answers::default();
            let now = Timestamp::now();
            chat.step(|| Some((message("one"), 0)), 0, &launches, &answers, now);
            chat.step(|| None, 0, &launches, &answers, now);
            let delivered = answers.delivered.borrow();
            delivered
                .first()
                .map(|(_, told)| told.clone())
                .unwrap_or_default()
        };
        let mut chat = Chat::new(vec![send_local()])
            .with_model(Some(String::from("qwen3.5:9b")))
            .with_choice(Some(owner.clone()));
        assert!(
            failed(&mut chat).starts_with("The model (qwen3.5:9b) kept calling"),
            "the operator's, with no choice"
        );
        std::fs::write(
            owner.join(crate::model_choice::MODEL_FILE),
            r#"{"name":"gemma4:12b"}"#,
        )
        .unwrap();
        let told = failed(&mut chat);
        assert!(
            told.starts_with("The model (gemma4:12b) kept calling"),
            "{told}"
        );
        let _ = std::fs::remove_dir_all(&dir);
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

    /// Past a usage limit the owner set, a message isn't run, and a run
    /// the broker stopped for it is told as that limit: never as a model
    /// that couldn't be reached.
    #[test]
    fn a_reached_limit_is_told_and_no_run_starts() {
        use hatchling_core::run_failure::RunFailure;
        use hatchling_core::ui::UsageLimits;
        use hatchling_scheduler::Ending;
        let usage =
            std::env::temp_dir().join(format!("hatchling-chat-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&usage);
        let now = Timestamp::now();
        crate::usage::count(
            &usage,
            &hatchling_egress::EgressRequest {
                method: String::from("POST"),
                destination: String::from("localhost:11434"),
                path: String::from("/v1/chat/completions"),
                headers: Vec::new(),
                body: Vec::new(),
                surrogate_b64: String::new(),
            },
            &hatchling_egress::EgressResponse {
                status: 200,
                headers: Vec::new(),
                body: br#"{"model":"m","usage":{"prompt_tokens":1200,"completion_tokens":300}}"#
                    .to_vec(),
            },
            now.as_second(),
        );
        let launches = OneEnded(RefCell::new(Some(Ended(Ending::Exited(
            RunFailure::ModelUnavailable.code().into(),
        )))));
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()])
            .with_model(Some(String::from("m")))
            .with_usage(usage.clone());
        // No limit yet: the message runs.
        assert_eq!(
            chat.step(|| Some((message("one"), 0)), 0, &launches, &answers, now),
            Step::Started
        );
        crate::usage::set_limits(
            &usage,
            &UsageLimits {
                daily_tokens: Some(1000),
                monthly_tokens: None,
            },
        )
        .unwrap();
        assert_eq!(
            chat.step(|| None, 0, &launches, &answers, now),
            Step::Answered
        );
        // The next message isn't run: the one launch is spent, so a launch
        // would have been told as a run that couldn't start.
        assert_eq!(
            chat.step(|| Some((message("two"), 0)), 0, &launches, &answers, now),
            Step::Answered
        );
        let delivered = answers.delivered.borrow();
        let told: Vec<&str> = delivered.iter().map(|(_, text)| text.as_str()).collect();
        let limit = "You've reached the daily limit you set: 1,500 of 1,000 tokens today.";
        assert_eq!(told.len(), 2, "{told:?}");
        assert!(told.iter().all(|text| text.starts_with(limit)), "{told:?}");
        assert!(
            chat.history().is_empty(),
            "nothing is kept: {:?}",
            chat.history()
        );
        let _ = std::fs::remove_dir_all(&usage);
    }

    #[test]
    fn only_a_model_that_couldnt_be_reached_is_told_as_a_limit() {
        use hatchling_core::run_failure::RunFailure;
        use hatchling_scheduler::Ending;
        let reached = crate::usage::Reached {
            span: crate::usage::Span::Month,
            spent: 20,
            limit: 10,
        };
        let unreachable = Some(Ending::Exited(RunFailure::ModelUnavailable.code().into()));
        assert_eq!(
            failed_with(unreachable, Some("m"), Some(&reached), false),
            reached.told()
        );
        assert_eq!(
            failed_with(unreachable, Some("m"), None, false),
            chat_failed(unreachable, Some("m"))
        );
        for other in [
            Some(Ending::Exited(RunFailure::OutOfSteps.code().into())),
            Some(Ending::Exited(1)),
            Some(Ending::TimedOut),
            None,
        ] {
            assert_eq!(
                failed_with(other, Some("m"), Some(&reached), false),
                chat_failed(other, Some("m")),
                "{other:?}"
            );
        }
    }

    /// A run whose model was still answering when the broker stopped
    /// waiting is told that, and isn't sent to look for a model that isn't
    /// running. A limit reached is told first, and any other ending is told
    /// as it always is.
    #[test]
    fn a_model_still_answering_is_said_to_be_slow_and_not_missing() {
        use hatchling_core::run_failure::RunFailure;
        use hatchling_scheduler::Ending;
        let unreachable = Some(Ending::Exited(RunFailure::ModelUnavailable.code().into()));
        let told = failed_with(unreachable, Some("m"), None, true);
        assert_eq!(told, model_quiet(Some("m")));
        assert_ne!(told, chat_failed(unreachable, Some("m")));
        assert!(
            told.starts_with("The model (m) was still working"),
            "{told}"
        );
        assert!(!told.contains("running and reachable"), "{told}");
        assert!(!told.contains("free"), "{told}");
        assert!(model_quiet(None).starts_with("The model was still working"));
        // A model that costs nothing is said to be one, by its name's end.
        let free = model_quiet(Some("vendor/model:free"));
        assert!(free.contains("It is a free hosted model"), "{free}");
        assert!(!model_quiet(Some("free-model")).contains("free hosted"));
        assert!(!model_quiet(Some("vendor/model:free:other")).contains("free hosted"));
        // "More than five minutes" is the broker's own wait.
        assert!(crate::egress_authority::ANSWER_TIMEOUT.as_secs() > 5 * 60);
        assert!(crate::egress_authority::ANSWER_TIMEOUT.as_secs() < 6 * 60);
        // A limit reached is why its calls were refused: that is told.
        let reached = crate::usage::Reached {
            span: crate::usage::Span::Month,
            spent: 20,
            limit: 10,
        };
        assert_eq!(
            failed_with(unreachable, Some("m"), Some(&reached), true),
            reached.told()
        );
        for other in [
            Some(Ending::Exited(RunFailure::OutOfSteps.code().into())),
            Some(Ending::Exited(1)),
            Some(Ending::TimedOut),
            None,
        ] {
            assert_eq!(
                failed_with(other, Some("m"), None, true),
                chat_failed(other, Some("m")),
                "{other:?}"
            );
        }
    }

    /// An answer is delivered as coming from the run that wrote it; what
    /// Hatchling says itself goes as its own notice.
    #[test]
    fn an_answer_names_its_run_and_hatchlings_words_are_notices() {
        let launches = Launches::default();
        launches
            .outcomes
            .borrow_mut()
            .extend([Ok(answering("Done.")), Err(String::from("no cell"))]);
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        assert_eq!(
            exchange(&mut chat, "one", &launches, &answers),
            Step::Answered
        );
        let now = Timestamp::now();
        assert_eq!(
            chat.step(|| Some((message("two"), 0)), 0, &launches, &answers, now),
            Step::Answered
        );
        assert_eq!(*answers.runs.borrow(), [Some(String::from("run-scripted"))]);
        assert_eq!(*answers.notices.borrow(), [String::from(CHAT_FAILED)]);
    }

    /// The first time older answers stop being given whole, and the first
    /// time the owner's oldest words stop being given at all, the person
    /// is told, once each, after the answer that did it; a conversation
    /// resumed already compacted isn't told again.
    #[test]
    fn compaction_is_told_once_each_way() {
        let long = "x".repeat(4000);
        let run = |chat: &mut Chat<Scripted>, launches: &Launches, answers: &Answers, n: usize| {
            for turn in 0..n {
                launches
                    .outcomes
                    .borrow_mut()
                    .push(Ok(answering(&format!("a{turn}"))));
                assert_eq!(
                    exchange(chat, &format!("{turn}{long}"), launches, answers),
                    Step::Answered
                );
            }
        };
        let launches = Launches::default();
        let answers = Answers::default();
        let mut chat = Chat::new(vec![send_local()]);
        run(&mut chat, &launches, &answers, 6);
        assert!(answers.notices.borrow().is_empty(), "all still given whole");
        run(&mut chat, &launches, &answers, 1);
        assert_eq!(*answers.notices.borrow(), [String::from(COMPACTED)]);
        let delivered = answers.delivered.borrow().len();
        assert_eq!(
            answers
                .delivered
                .borrow()
                .get(delivered - 2)
                .map(|(_, text)| text.as_str()),
            Some("a0"),
            "told after the answer that did it"
        );
        run(&mut chat, &launches, &answers, 5);
        assert_eq!(
            *answers.notices.borrow(),
            [String::from(COMPACTED), String::from(FORGOTTEN)]
        );

        // Resumed with what the owner said earlier: already compacted.
        let resumed = Conversation {
            earlier: vec![String::from("before")],
            exchanges: Vec::new(),
        };
        let quiet = Answers::default();
        let mut chat = Chat::resuming(vec![send_local()], 0, resumed, ());
        run(&mut chat, &launches, &quiet, 7);
        assert!(
            quiet.notices.borrow().is_empty(),
            "{:?}",
            quiet.notices.borrow()
        );
    }
}
