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
//! blocked or replaced are never given back. The record is the broker's
//! and lives in its memory: a run can't write to it, and a broker
//! restart starts a new conversation.

use std::collections::BTreeSet;
use std::time::Duration;

use hatchling_core::conversation::{self, ChatExchange};

use hatchling_channel::{ChannelId, LocalHub, Message};
use hatchling_core::ScheduleGrant;
use hatchling_scheduler::{Delivery, Launcher, Run, RunRequest, Timestamp};

/// How often an idle chat looks for a message, and a running one for its
/// answer.
pub const CHAT_POLL: Duration = Duration::from_millis(200);

/// What the person is told when a chat run fails or can't start.
pub const CHAT_FAILED: &str = "The agent couldn't answer that message. The broker's log says why.";

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

/// The chat's one run in flight, and the conversation so far.
#[derive(Debug)]
pub struct Chat<R> {
    /// The run in flight, and the message it answers.
    running: Option<(R, String)>,
    grants: Vec<ScheduleGrant>,
    /// The exchanges kept, oldest first: no more than a run is given.
    exchanges: Vec<ChatExchange>,
}

impl<R: Run> Chat<R> {
    /// A chat whose runs hold `grants`.
    #[must_use]
    pub const fn new(grants: Vec<ScheduleGrant>) -> Self {
        Self {
            running: None,
            grants,
            exchanges: Vec::new(),
        }
    }

    /// The exchanges a run started now is given, oldest first.
    #[must_use]
    pub fn history(&self) -> Vec<ChatExchange> {
        conversation::recent(&self.exchanges)
    }

    /// Keeps the exchange of `user` and `answer`, the person having been
    /// shown `answer` as written; the oldest goes past the bound.
    fn keep(&mut self, user: String, answer: String) {
        self.exchanges.push(ChatExchange {
            user,
            assistant: answer,
        });
        let over = self
            .exchanges
            .len()
            .saturating_sub(conversation::MAX_EXCHANGES);
        self.exchanges.drain(..over);
    }

    /// One step at `now`: a running run is polled, and a finished one
    /// answered; with none running, the message `next` gives starts one.
    pub fn step<L, D>(
        &mut self,
        next: impl FnOnce() -> Option<Message>,
        launcher: &L,
        delivery: &D,
        now: Timestamp,
    ) -> Step
    where
        L: Launcher<Run = R>,
        D: Delivery,
    {
        if let Some((run, _)) = &mut self.running {
            let Some(outcome) = run.poll() else {
                return Step::Running;
            };
            let Some((_, user)) = self.running.take() else {
                return Step::Idle;
            };
            let answer = match outcome {
                Ok(answer) => answer,
                Err(reason) => {
                    tracing::warn!(reason = ?reason, "a chat run failed");
                    return answer_with(delivery, CHAT_FAILED);
                }
            };
            return match delivery.deliver_verbatim(&ChannelId::local().0, &answer) {
                Ok(shown) => {
                    if shown {
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
        let Some(message) = next() else {
            return Step::Idle;
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
            history: self.history(),
        };
        match launcher.launch(request) {
            Ok(run) => {
                self.running = Some((run, message.text));
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
    let mut chat = Chat::new(grants);
    loop {
        match chat.step(|| hub.take(), &launcher, &delivery, Timestamp::now()) {
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
    fn exchange(
        chat: &mut Chat<Scripted>,
        text: &str,
        launches: &Launches,
        answers: &Answers,
    ) -> Step {
        let now = Timestamp::now();
        assert_eq!(
            chat.step(|| Some(message(text)), launches, answers, now),
            Step::Started
        );
        chat.step(|| None, launches, answers, now)
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
            chat.step(|| Some(message("unstarted")), &launches, &answers, now),
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
        assert_eq!(chat.exchanges.len(), conversation::MAX_EXCHANGES);
        let history = chat.history();
        assert_eq!(history.len(), conversation::MAX_EXCHANGES);
        assert_eq!(history.first().map(|turn| turn.user.as_str()), Some("q3"));
        assert_eq!(
            history.last().map(|turn| turn.user.clone()),
            Some(format!("q{}", count - 1))
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
        assert_eq!(chat.step(|| None, &launches, &answers, now), Step::Idle);
        assert_eq!(
            chat.step(
                || Some(message("What is on today?")),
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
            Some(message("too soon"))
        };
        assert_eq!(
            chat.step(untouched, &launches, &answers, now),
            Step::Running
        );
        assert_eq!(
            chat.step(untouched, &launches, &answers, now),
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
        assert_eq!(chat.step(|| None, &launches, &answers, now), Step::Idle);
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
            chat.step(|| Some(message("one")), &launches, &answers, now),
            Step::Started
        );
        assert_eq!(chat.step(|| None, &launches, &answers, now), Step::Answered);
        assert_eq!(
            chat.step(|| Some(message("two")), &launches, &answers, now),
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
        assert_eq!(chat.step(|| None, &launches, &answers, now), Step::Idle);
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
            chat.step(|| Some(message("one")), &launches, &answers, now),
            Step::Started
        );
        assert_eq!(
            chat.step(|| None, &launches, &answers, now),
            Step::Undelivered
        );
        assert_eq!(
            chat.step(|| Some(message("two")), &launches, &answers, now),
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
