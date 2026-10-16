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

use std::collections::BTreeSet;
use std::time::Duration;

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

/// The chat's one run in flight.
#[derive(Debug)]
pub struct Chat<R> {
    running: Option<R>,
    grants: Vec<ScheduleGrant>,
}

impl<R: Run> Chat<R> {
    /// A chat whose runs hold `grants`.
    #[must_use]
    pub const fn new(grants: Vec<ScheduleGrant>) -> Self {
        Self {
            running: None,
            grants,
        }
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
        if let Some(run) = &mut self.running {
            let Some(outcome) = run.poll() else {
                return Step::Running;
            };
            self.running = None;
            let answer = outcome.unwrap_or_else(|reason| {
                tracing::warn!(reason = ?reason, "a chat run failed");
                String::from(CHAT_FAILED)
            });
            return answer_with(delivery, &answer);
        }
        let Some(message) = next() else {
            return Step::Idle;
        };
        let request = RunRequest {
            schedule_id: String::from(CHAT_LABEL),
            task: message.text,
            grants: self.grants.clone(),
            due: now,
            missed: false,
            channel: ChannelId::local().0,
            depth: 0,
            event: None,
            space: None,
            feed: None,
        };
        match launcher.launch(request) {
            Ok(run) => {
                self.running = Some(run);
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
    key: &'env hatchling_vault::MasterKey,
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

    /// Records each delivery; refuses them all when `refuse` is set.
    #[derive(Default)]
    struct Answers {
        delivered: RefCell<Vec<(String, String)>>,
        refuse: bool,
    }

    impl Delivery for Answers {
        fn deliver(&self, channel: &str, text: &str) -> Result<(), String> {
            self.delivered
                .borrow_mut()
                .push((channel.to_owned(), text.to_owned()));
            if self.refuse {
                Err(String::from("no client"))
            } else {
                Ok(())
            }
        }
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
