//! What the local API does with each action, and its audit trail
//! .
//!
//! Every decision is the broker's, made here against the caller's
//! grants, already narrowed to the owner's by [`crate::api_auth::admit`].
//! An action runs only where the caller can be shown to hold what it
//! runs with:
//!
//! - **Schedules** run with the grants they name, so creating one needs
//!   exactly those grants (a feed's are derived from its spec). The
//!   schedule handler then checks them against the owner's as for any
//!   creator.
//! - **Sending a message** reaches the agent, which acts with *all* of
//!   the owner's grants; so does a **skill**, with grants no schedule
//!   label names; and **cancelling** a schedule can undo one made with
//!   any of them. These need the caller to hold the owner's whole set.
//!   Anything less is the confused deputy: a narrow caller borrowing the
//!   agent's reach.
//! - **A caller with no grants** is refused every action, whatever the
//!   owner holds: "the owner's whole set" is no bar when the owner holds
//!   none, and a caller with nothing must not pass it.
//! - **Schedules the API makes are the owner's** ([`Creator::Owner`]): a
//!   cell can neither see nor cancel them.
//!
//! Every other action has no backend in this build and is refused as
//! unavailable. Every call, served or refused, is recorded ([`Audit`]).

use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use hatchling_channel::{ChannelId, LocalHub, MAX_INGRESS_TEXT_BYTES, Message, SenderId};
use hatchling_channel::{IngressEvent, SinkId};
use hatchling_core::CellId;
use hatchling_core::messages::{ScheduleGrant, ScheduleOp};
use hatchling_core::subagent::{ActivityEvent, SubagentEvent};
use hatchling_core::ui::{
    ActionRefusal, AgentState, ApiOutcome, ApiRequest, ChatAuthor, ChatMessage, Decision, UiAction,
    UiActionResult, UiEventBody,
};
use hatchling_scheduler::{Creator, Timestamp};
use hatchling_vault::MasterKey;

use crate::api_auth::Caller;
use crate::api_stream::Bus;
use crate::schedule_ops::{held_grants, list_schedules};
use crate::skills::Skills;
use crate::{Config, handle_schedule_op, handle_skill_op};

/// The one chat the API offers: the local channel's conversation.
pub const LOCAL_CHAT: &str = "local";

/// The most characters of a message its working status shows.
const STATUS_DETAIL_CHARS: usize = 80;

/// What the agent's working status says about `text`: its first line,
/// cut to [`STATUS_DETAIL_CHARS`] characters (an ellipsis marks a cut).
fn status_detail(text: &str) -> String {
    let mut lines = text.trim().lines();
    let mut chars = lines.next().unwrap_or_default().trim_end().chars();
    let mut detail: String = chars.by_ref().take(STATUS_DETAIL_CHARS).collect();
    if chars.next().is_some() || lines.next().is_some() {
        detail.push('\u{2026}');
    }
    detail
}

/// The activity thread every API call is recorded on.
pub const API_RUN_ID: &str = "api";

/// The cell id an API-invoked skill's log lines name.
const API_CELL: &str = "api";

/// What the API's actions reach: the broker's own configuration, key and
/// skills, the local channel's hub when there is one, and the stream
/// replies are pushed to.
pub struct Ops<'a> {
    config: &'a Config,
    key: &'a MasterKey,
    skills: &'a Skills,
    hub: Option<&'a LocalHub>,
    bus: Arc<Bus>,
}

impl std::fmt::Debug for Ops<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ops")
            .field("hub", &self.hub.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> Ops<'a> {
    /// The actions' reach.
    #[must_use]
    pub const fn new(
        config: &'a Config,
        key: &'a MasterKey,
        skills: &'a Skills,
        hub: Option<&'a LocalHub>,
        bus: Arc<Bus>,
    ) -> Self {
        Self {
            config,
            key,
            skills,
            hub,
            bus,
        }
    }

    /// The stream replies and sent messages are pushed to.
    #[must_use]
    pub const fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }

    /// The owner's grants: the most any caller may hold.
    #[must_use]
    pub fn owner_grants(&self) -> BTreeSet<ScheduleGrant> {
        held_grants(self.config)
    }

    /// Handles `action` for `caller`, and says whether the API served it
    /// (passed it to its backend) or refused it.
    pub fn handle(
        &self,
        caller: &Caller,
        action: &UiAction,
        replies: &mut Replies<'a>,
    ) -> (UiActionResult, ApiOutcome) {
        let owner = held_grants(self.config);
        let refused = |reason| (UiActionResult::Refused { reason }, ApiOutcome::Refused);
        if caller.grants().is_empty() {
            return refused(ActionRefusal::NotGranted);
        }
        match action {
            UiAction::SendMessage { chat_id, text } => {
                if !caller.holds(&owner) {
                    return refused(ActionRefusal::NotGranted);
                }
                self.send(chat_id, text, replies)
            }
            UiAction::InvokeSkill { op } => {
                if !caller.holds(&owner) {
                    return refused(ActionRefusal::NotGranted);
                }
                let Ok(cell) = API_CELL.parse::<CellId>() else {
                    return refused(ActionRefusal::Unavailable);
                };
                let keys = crate::harness::vault_keys(&self.config.vault.root, self.key);
                (
                    UiActionResult::Skill {
                        outcome: handle_skill_op(self.skills, op, &cell, &keys),
                    },
                    ApiOutcome::Served,
                )
            }
            // A list shows what the caller's own grants cover.
            UiAction::ManageSchedule {
                op: ScheduleOp::List { after },
            } => (
                UiActionResult::Schedule {
                    outcome: list_schedules(
                        self.config,
                        self.key,
                        caller.grants(),
                        after.as_deref(),
                        Creator::Owner,
                    ),
                },
                ApiOutcome::Served,
            ),
            UiAction::ManageSchedule { op } => {
                if !caller.holds(&schedule_needs(op, &owner)) {
                    return refused(ActionRefusal::NotGranted);
                }
                (
                    UiActionResult::Schedule {
                        outcome: handle_schedule_op(
                            self.config,
                            self.key,
                            op,
                            Timestamp::now(),
                            Creator::Owner,
                        ),
                    },
                    ApiOutcome::Served,
                )
            }
            UiAction::Decide {
                confirmation_id,
                revision,
                digest,
                decision,
            } => {
                // Only the owner decides (WO-027): a caller holding less
                // than every grant the owner holds can't.
                if !caller.holds(&owner) {
                    return refused(ActionRefusal::NotGranted);
                }
                self.decide(confirmation_id, *revision, digest, *decision)
            }
            UiAction::OpenArtifact { .. }
            | UiAction::ShareArtifact { .. }
            | UiAction::AttachArtifact { .. }
            | UiAction::SaveFile { .. }
            | UiAction::TakeControl { .. }
            | UiAction::ReturnControl { .. }
            | UiAction::StopBrowser { .. } => refused(ActionRefusal::Unavailable),
        }
    }

    /// The owner's `decision` on a confirmation, at the `revision` and
    /// `digest` they saw (WO-027): recorded on the broker's board, which
    /// queues an approval for its worker and tells the stream.
    fn decide(
        &self,
        confirmation_id: &str,
        revision: u64,
        digest: &str,
        decision: Decision,
    ) -> (UiActionResult, ApiOutcome) {
        let board = crate::approval_board::board(self.config);
        match board.decide(
            confirmation_id,
            revision,
            digest,
            decision,
            Timestamp::now().as_second(),
        ) {
            Ok(_) => (
                UiActionResult::Decided {
                    confirmation_id: confirmation_id.to_owned(),
                    decision,
                },
                ApiOutcome::Served,
            ),
            Err(reason) => (
                UiActionResult::DecisionRejected {
                    confirmation_id: confirmation_id.to_owned(),
                    reason,
                },
                ApiOutcome::Refused,
            ),
        }
    }

    /// Queues `text` for the agent on the local channel, shows it on the
    /// stream with the agent working on it, and routes the agent's
    /// replies to the stream, each leaving it idle.
    fn send(
        &self,
        chat_id: &str,
        text: &str,
        replies: &mut Replies<'a>,
    ) -> (UiActionResult, ApiOutcome) {
        let refused = |reason| (UiActionResult::Refused { reason }, ApiOutcome::Refused);
        if chat_id != LOCAL_CHAT || text.len() > MAX_INGRESS_TEXT_BYTES {
            return refused(ActionRefusal::BadRequest);
        }
        let Some(hub) = self.hub else {
            return refused(ActionRefusal::Unavailable);
        };
        let Ok(message_id) = crate::approval::random_hex("msg-") else {
            return refused(ActionRefusal::Unavailable);
        };
        replies.route(hub, &self.bus);
        let message = Message {
            sender: SenderId(String::from("api")),
            channel: ChannelId::local(),
            text: text.to_owned(),
        };
        match hub.inject(message) {
            IngressEvent::Queued => {
                self.bus.push(
                    Timestamp::now().to_string(),
                    UiEventBody::ChatMessage(ChatMessage {
                        chat_id: String::from(LOCAL_CHAT),
                        message_id: message_id.clone(),
                        author: ChatAuthor::User,
                        text: text.to_owned(),
                    }),
                );
                self.bus.push(
                    Timestamp::now().to_string(),
                    UiEventBody::AgentStatus {
                        state: AgentState::Working,
                        detail: status_detail(text),
                    },
                );
                (
                    UiActionResult::MessageAccepted { message_id },
                    ApiOutcome::Served,
                )
            }
            IngressEvent::Refused | IngressEvent::Reply { .. } => {
                refused(ActionRefusal::Unavailable)
            }
        }
    }
}

/// The grants a schedule op needs the caller to hold: a create's own, a
/// feed's derived ones, and for a cancel or a follow-up the owner's
/// whole set (a cancel can undo any schedule; a follow-up is a run's). A
/// list never comes here: it is filtered by the caller's grants instead.
fn schedule_needs(op: &ScheduleOp, owner: &BTreeSet<ScheduleGrant>) -> BTreeSet<ScheduleGrant> {
    match op {
        ScheduleOp::Create { grants, .. } => grants.iter().cloned().collect(),
        ScheduleOp::CreateFeed { channel, feed, .. } => {
            let mut grants: BTreeSet<ScheduleGrant> = hatchling_feed::Spec::parse(feed)
                .map(|spec| spec.grants().into_iter().collect())
                .unwrap_or_default();
            grants.insert(ScheduleGrant::ChannelSend {
                channel: channel.clone(),
            });
            grants
        }
        ScheduleOp::Cancel { .. } | ScheduleOp::Followup { .. } | ScheduleOp::List { .. } => {
            owner.clone()
        }
    }
}

/// One connection's reply route: while attached, the agent's replies on
/// the local channel go to the stream. Detached when dropped, unless a
/// newer client has taken the route since.
#[derive(Default)]
pub struct Replies<'h> {
    attached: Option<(&'h LocalHub, SinkId)>,
}

impl std::fmt::Debug for Replies<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replies")
            .field("attached", &self.attached.is_some())
            .finish()
    }
}

impl<'h> Replies<'h> {
    /// Routes `hub`'s replies to `bus`, once per connection.
    fn route(&mut self, hub: &'h LocalHub, bus: &Arc<Bus>) {
        if self.attached.is_some() {
            return;
        }
        let bus = Arc::clone(bus);
        let sink = hub.attach(Box::new(move |cleared: &hatchling_channel::Cleared| {
            let reply = cleared.message();
            let message_id = crate::approval::random_hex("msg-")
                .map_err(|error| hatchling_channel::Error::Backend(error.to_string()))?;
            bus.push(
                Timestamp::now().to_string(),
                UiEventBody::ChatMessage(ChatMessage {
                    chat_id: String::from(LOCAL_CHAT),
                    message_id,
                    author: ChatAuthor::Agent,
                    text: reply.text.clone(),
                }),
            );
            bus.push(
                Timestamp::now().to_string(),
                UiEventBody::AgentStatus {
                    state: AgentState::Idle,
                    detail: String::new(),
                },
            );
            Ok(())
        }));
        self.attached = Some((hub, sink));
    }
}

impl Drop for Replies<'_> {
    fn drop(&mut self) {
        if let Some((hub, sink)) = self.attached.take() {
            hub.detach(sink);
        }
    }
}

/// The API's audit trail: one activity event per call, appended to the
/// activity log (owner-only), and so onto the stream and into the feed.
#[derive(Debug)]
pub struct Audit {
    log: PathBuf,
    connection: String,
    sequence: u64,
}

impl Audit {
    /// The trail for one connection, `connection` naming it in each event
    /// id, appending to `log`.
    #[must_use]
    pub const fn new(log: PathBuf, connection: String) -> Self {
        Self {
            log,
            connection,
            sequence: 0,
        }
    }

    /// Records one call by `caller` (or an unidentified one), and
    /// returns the event recorded.
    pub fn record(
        &mut self,
        caller: Option<&Caller>,
        request: ApiRequest,
        outcome: ApiOutcome,
    ) -> ActivityEvent {
        self.sequence += 1;
        let event = ActivityEvent {
            event_id: format!("{}:{}", self.connection, self.sequence),
            ts: Timestamp::now().to_string(),
            run_id: String::from(API_RUN_ID),
            kind: SubagentEvent::ApiCall {
                caller: caller.map_or_else(
                    || String::from("unidentified"),
                    |caller| caller.label().to_owned(),
                ),
                capabilities: caller.map(Caller::grant_labels).unwrap_or_default(),
                request,
                outcome,
            },
        };
        match serde_json::to_string(&event) {
            Ok(line) => {
                tracing::info!(target: "activity", event = %line, "API call");
                let written = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(&self.log)
                    .and_then(|mut file| writeln!(file, "{line}"));
                if let Err(error) = written {
                    tracing::error!(%error, log = %self.log.display(), "cannot append an API call to the activity log");
                }
            }
            Err(error) => tracing::error!(%error, "cannot encode an API call's activity event"),
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use hatchling_core::messages::{FeedSpec, ScheduleResult, ScheduleSpec, SkillOp, SkillResult};
    use hatchling_core::ui::{DecisionRejection, FileContent, FileFormat, UiEvent};

    use super::*;
    use crate::api_auth::tests::{owner_config, scratch};
    use crate::config::{GoalsAction, SkillConfig};

    fn goals(action: &str) -> ScheduleGrant {
        ScheduleGrant::Goals {
            action: action.to_owned(),
        }
    }

    fn local_send() -> ScheduleGrant {
        ScheduleGrant::ChannelSend {
            channel: String::from("local"),
        }
    }

    /// A caller holding `grants`.
    fn caller(grants: &[ScheduleGrant]) -> Caller {
        Caller::new(String::from("uid 1000"), grants.iter().cloned().collect())
    }

    fn create(grants: Vec<ScheduleGrant>) -> UiAction {
        UiAction::ManageSchedule {
            op: ScheduleOp::Create {
                when: ScheduleSpec::Cron {
                    expr: String::from("0 8 * * *"),
                    tz: String::from("UTC"),
                },
                task: String::from("Summarize my day."),
                grants,
                channel: String::from("local"),
            },
        }
    }

    fn feed() -> ScheduleOp {
        ScheduleOp::CreateFeed {
            when: ScheduleSpec::Cron {
                expr: String::from("0 8 * * *"),
                tz: String::from("UTC"),
            },
            channel: String::from("local"),
            feed: FeedSpec {
                goals: Some(String::from("open")),
                tracking: false,
                tracking_source: None,
                memory_tags: Vec::new(),
                lookback_hours: 24,
                max_items: 20,
            },
        }
    }

    fn send(text: &str) -> UiAction {
        UiAction::SendMessage {
            chat_id: String::from(LOCAL_CHAT),
            text: text.to_owned(),
        }
    }

    fn chats(bus: &Bus) -> Vec<(ChatAuthor, String)> {
        bus.after(None)
            .into_iter()
            .filter_map(|event: UiEvent| {
                if let UiEventBody::ChatMessage(chat) = event.body {
                    Some((chat.author, chat.text))
                } else {
                    None
                }
            })
            .collect()
    }

    fn statuses(bus: &Bus) -> Vec<(AgentState, String)> {
        bus.after(None)
            .into_iter()
            .filter_map(|event: UiEvent| {
                if let UiEventBody::AgentStatus { state, detail } = event.body {
                    Some((state, detail))
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn a_status_shows_the_first_line_cut_on_a_character() {
        assert_eq!(status_detail("What is on today?"), "What is on today?");
        assert_eq!(status_detail("  padded  "), "padded");
        assert_eq!(
            status_detail("Plan the week.\nThen book it."),
            "Plan the week.\u{2026}"
        );
        assert_eq!(status_detail("trailing line\n"), "trailing line");
        assert_eq!(status_detail(""), "");
        let exact = "a".repeat(STATUS_DETAIL_CHARS);
        assert_eq!(status_detail(&exact), exact);
        // Two-byte characters past the limit are cut whole, not mid-byte.
        let long = "\u{e9}".repeat(STATUS_DETAIL_CHARS + 1);
        let shown = status_detail(&long);
        assert_eq!(shown.chars().count(), STATUS_DETAIL_CHARS + 1);
        assert_eq!(
            shown,
            format!("{}\u{2026}", "\u{e9}".repeat(STATUS_DETAIL_CHARS))
        );
    }

    #[test]
    fn a_schedule_needs_the_grants_it_runs_with_and_a_cancel_the_owners() {
        let owner = BTreeSet::from([goals("query-goals"), local_send()]);
        let UiAction::ManageSchedule { op } = create(vec![goals("query-goals")]) else {
            return;
        };
        assert_eq!(
            schedule_needs(&op, &owner),
            BTreeSet::from([goals("query-goals")])
        );
        assert_eq!(
            schedule_needs(&feed(), &owner),
            BTreeSet::from([goals("query-goals"), local_send()]),
            "a feed's derived grants and its delivery"
        );
        let broken = ScheduleOp::CreateFeed {
            when: ScheduleSpec::Once {
                at: String::from("2026-10-08T09:00:00Z"),
                tz: String::new(),
            },
            channel: String::from("local"),
            feed: FeedSpec {
                goals: None,
                tracking: false,
                tracking_source: None,
                memory_tags: Vec::new(),
                lookback_hours: 24,
                max_items: 20,
            },
        };
        assert_eq!(
            schedule_needs(&broken, &owner),
            BTreeSet::from([local_send()]),
            "an unreadable feed still needs its delivery; the handler refuses it"
        );
        assert_eq!(
            schedule_needs(
                &ScheduleOp::Cancel {
                    id: String::from("s")
                },
                &owner
            ),
            owner
        );
        let followup = ScheduleOp::Followup {
            task: String::from("t"),
            delay_secs: 60,
            grants: Vec::new(),
        };
        assert_eq!(schedule_needs(&followup, &owner), owner);
        assert_eq!(
            schedule_needs(&ScheduleOp::List { after: None }, &owner),
            owner,
            "a list is filtered by the caller's grants before it gets here"
        );
    }

    /// The ids a schedule list shows.
    fn listed(result: &ScheduleResult) -> Vec<String> {
        if let ScheduleResult::Listed { schedules, .. } = result {
            schedules.iter().map(|shown| shown.id.clone()).collect()
        } else {
            vec![format!("not listed: {result:?}")]
        }
    }

    #[test]
    fn a_schedule_the_api_makes_is_the_owners_and_a_cell_can_neither_see_nor_cancel_it() {
        let dir = scratch("creator");
        let mut config = owner_config(Some(&dir.0.join("schedules").to_string_lossy()));
        config.goals.grants = vec![GoalsAction::QueryGoals];
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let ops = Ops::new(&config, &key, &skills, None, Arc::new(Bus::starting_at(1)));
        let owner = caller(&ops.owner_grants().into_iter().collect::<Vec<_>>());
        let created = |result: ScheduleResult| {
            if let ScheduleResult::Created { id } = result {
                id
            } else {
                format!("not created: {result:?}")
            }
        };
        let owners = match ops.handle(&owner, &create(vec![local_send()]), &mut Replies::default())
        {
            (UiActionResult::Schedule { outcome }, ApiOutcome::Served) => created(outcome),
            other => format!("not served: {other:?}"),
        };
        let UiAction::ManageSchedule { op: create_op } = create(vec![local_send()]) else {
            return;
        };
        let now = Timestamp::now();
        let agent = |op: &ScheduleOp| handle_schedule_op(&config, &key, op, now, Creator::Agent);
        let agents = created(agent(&create_op));
        let list = ScheduleOp::List { after: None };
        // The cell sees its own; the owner sees both.
        assert_eq!(listed(&agent(&list)), std::slice::from_ref(&agents));
        let mut both = vec![owners.clone(), agents.clone()];
        both.sort();
        assert_eq!(
            listed(&handle_schedule_op(
                &config,
                &key,
                &list,
                now,
                Creator::Owner
            )),
            both
        );
        // The cell's cancel of the owner's is answered as for no such id,
        // and the schedule stays.
        let cancel = |id: &str| ScheduleOp::Cancel { id: id.to_owned() };
        assert_eq!(agent(&cancel(&owners)), ScheduleResult::NotFound);
        assert_eq!(agent(&cancel("sch-none")), ScheduleResult::NotFound);
        assert_eq!(
            listed(&handle_schedule_op(
                &config,
                &key,
                &list,
                now,
                Creator::Owner
            )),
            both
        );
        // Each cancels its own; the owner may cancel the agent's.
        assert_eq!(agent(&cancel(&agents)), ScheduleResult::Cancelled);
        let again = created(agent(&create_op));
        assert_eq!(
            handle_schedule_op(&config, &key, &cancel(&again), now, Creator::Owner),
            ScheduleResult::Cancelled
        );
        let by_api = UiAction::ManageSchedule {
            op: cancel(&owners),
        };
        assert_eq!(
            ops.handle(&owner, &by_api, &mut Replies::default()),
            (
                UiActionResult::Schedule {
                    outcome: ScheduleResult::Cancelled
                },
                ApiOutcome::Served
            )
        );
        assert_eq!(listed(&agent(&list)), Vec::<String>::new());
    }

    #[test]
    fn a_caller_with_no_grants_is_refused_everything_even_when_the_owner_holds_none() {
        let dir = scratch("none");
        let mut config = owner_config(Some(&dir.0.join("schedules").to_string_lossy()));
        config.goals.grants = Vec::new();
        config.channels = Vec::new();
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let ops = Ops::new(&config, &key, &skills, None, Arc::new(Bus::starting_at(1)));
        assert!(ops.owner_grants().is_empty(), "{:?}", ops.owner_grants());
        let not_granted = (
            UiActionResult::Refused {
                reason: ActionRefusal::NotGranted,
            },
            ApiOutcome::Refused,
        );
        let skill = UiAction::InvokeSkill {
            op: SkillOp::Invoke {
                skill: String::from("notes@1.0.0"),
                capability: "read".parse().unwrap(),
                input: String::new(),
            },
        };
        let cancel = UiAction::ManageSchedule {
            op: ScheduleOp::Cancel {
                id: String::from("sch-1"),
            },
        };
        for action in [send("hi"), skill, cancel, create(Vec::new())] {
            assert_eq!(
                ops.handle(&caller(&[]), &action, &mut Replies::default()),
                not_granted,
                "{action:?}"
            );
        }
    }

    #[test]
    fn a_message_of_exactly_the_ingress_bound_passes_the_length_check() {
        let dir = scratch("bound");
        let config = owner_config(Some(&dir.0.join("schedules").to_string_lossy()));
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let ops = Ops::new(&config, &key, &skills, None, Arc::new(Bus::starting_at(1)));
        let owner = caller(&ops.owner_grants().into_iter().collect::<Vec<_>>());
        let refused = |reason| (UiActionResult::Refused { reason }, ApiOutcome::Refused);
        let at = "x".repeat(MAX_INGRESS_TEXT_BYTES);
        // No hub here: a message the length check passes ends at
        // "unavailable"; one byte more is a bad request.
        assert_eq!(
            ops.handle(&owner, &send(&at), &mut Replies::default()),
            refused(ActionRefusal::Unavailable)
        );
        assert_eq!(
            ops.handle(&owner, &send(&format!("{at}x")), &mut Replies::default()),
            refused(ActionRefusal::BadRequest)
        );
    }

    #[test]
    fn a_list_shows_only_what_the_callers_grants_cover() {
        let dir = scratch("list");
        let mut config = owner_config(Some(&dir.0.join("schedules").to_string_lossy()));
        config.goals.grants = vec![GoalsAction::QueryGoals, GoalsAction::GoalCreate];
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let ops = Ops::new(&config, &key, &skills, None, Arc::new(Bus::starting_at(1)));
        let all = [goals("query-goals"), goals("goal-create"), local_send()];
        let owner = caller(&all);
        let narrow = caller(&[goals("query-goals"), local_send()]);
        let mut replies = Replies::default();
        let id = |grants: Vec<ScheduleGrant>| match ops.handle(
            &owner,
            &create(grants),
            &mut Replies::default(),
        ) {
            (
                UiActionResult::Schedule {
                    outcome: ScheduleResult::Created { id },
                },
                ApiOutcome::Served,
            ) => id,
            other => format!("not created: {other:?}"),
        };
        let readable = id(vec![goals("query-goals"), local_send()]);
        let writing = id(vec![goals("goal-create"), local_send()]);
        let list = UiAction::ManageSchedule {
            op: ScheduleOp::List { after: None },
        };
        let ids = |caller: &Caller| match ops.handle(caller, &list, &mut Replies::default()) {
            (
                UiActionResult::Schedule {
                    outcome: ScheduleResult::Listed { schedules, next },
                },
                ApiOutcome::Served,
            ) => {
                assert_eq!(next, None);
                schedules
                    .into_iter()
                    .map(|shown| shown.id)
                    .collect::<Vec<_>>()
            }
            other => vec![format!("not listed: {other:?}")],
        };
        let mut both = vec![readable.clone(), writing];
        both.sort();
        assert_eq!(ids(&owner), both);
        assert_eq!(ids(&narrow), [readable], "only what its grants cover");
        assert_eq!(
            ops.handle(&caller(&[]), &list, &mut replies),
            (
                UiActionResult::Refused {
                    reason: ActionRefusal::NotGranted,
                },
                ApiOutcome::Refused
            ),
            "no grant, nothing to list"
        );
    }

    #[test]
    fn each_action_runs_only_within_the_callers_grants() {
        let dir = scratch("grants");
        let mut config = owner_config(Some(&dir.0.join("schedules").to_string_lossy()));
        config.goals.grants = vec![GoalsAction::QueryGoals, GoalsAction::GoalCreate];
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let ops = Ops::new(&config, &key, &skills, None, Arc::new(Bus::starting_at(1)));
        let all = [goals("query-goals"), goals("goal-create"), local_send()];
        assert_eq!(ops.owner_grants(), BTreeSet::from(all.clone()));
        let shown = format!("{ops:?}");
        assert!(shown.contains("hub: false"));
        let narrow = caller(&[goals("query-goals"), local_send()]);
        let owner = caller(&all);
        let mut replies = Replies::default();
        let not_granted = (
            UiActionResult::Refused {
                reason: ActionRefusal::NotGranted,
            },
            ApiOutcome::Refused,
        );
        // The confused deputy: the agent and skills act with everything.
        assert_eq!(ops.handle(&narrow, &send("hi"), &mut replies), not_granted);
        let skill = UiAction::InvokeSkill {
            op: SkillOp::Invoke {
                skill: String::from("notes@1.0.0"),
                capability: "read".parse().unwrap(),
                input: String::new(),
            },
        };
        assert_eq!(ops.handle(&narrow, &skill, &mut replies), not_granted);
        assert_eq!(
            ops.handle(&owner, &skill, &mut replies),
            (
                UiActionResult::Skill {
                    outcome: SkillResult::UnknownSkill
                },
                ApiOutcome::Served
            )
        );
        // Schedules: exactly the grants they run with.
        assert_eq!(
            ops.handle(
                &narrow,
                &create(vec![goals("goal-create"), local_send()]),
                &mut replies
            ),
            not_granted
        );
        let (made, outcome) = ops.handle(
            &narrow,
            &create(vec![goals("query-goals"), local_send()]),
            &mut replies,
        );
        assert_eq!(outcome, ApiOutcome::Served);
        assert!(
            matches!(
                made,
                UiActionResult::Schedule {
                    outcome: ScheduleResult::Created { .. }
                }
            ),
            "{made:?}"
        );
        let cancel = UiAction::ManageSchedule {
            op: ScheduleOp::Cancel {
                id: String::from("sched-none"),
            },
        };
        assert_eq!(ops.handle(&narrow, &cancel, &mut replies), not_granted);
        assert_eq!(
            ops.handle(&owner, &cancel, &mut replies),
            (
                UiActionResult::Schedule {
                    outcome: ScheduleResult::NotFound
                },
                ApiOutcome::Served
            )
        );
    }

    /// WO-027: only the owner decides, and a decision on a confirmation
    /// the broker doesn't have is rejected as unknown.
    #[test]
    fn only_the_owner_decides_and_an_unknown_confirmation_is_rejected() {
        let config = owner_config(Some("/srv/hatchling/schedules"));
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let ops = Ops::new(&config, &key, &skills, None, Arc::new(Bus::starting_at(1)));
        let held: Vec<ScheduleGrant> = ops.owner_grants().into_iter().collect();
        assert!(held.len() > 1, "{held:?}");
        let owner = caller(&held);
        let partial = caller(held.get(..1).unwrap_or_default());
        let mut replies = Replies::default();
        let decide = UiAction::Decide {
            confirmation_id: String::from("approval-none"),
            revision: 1,
            digest: String::from("00"),
            decision: Decision::Approve,
        };
        assert_eq!(
            ops.handle(&partial, &decide, &mut replies),
            (
                UiActionResult::Refused {
                    reason: ActionRefusal::NotGranted
                },
                ApiOutcome::Refused
            )
        );
        assert_eq!(
            ops.handle(&owner, &decide, &mut replies),
            (
                UiActionResult::DecisionRejected {
                    confirmation_id: String::from("approval-none"),
                    reason: DecisionRejection::Unknown,
                },
                ApiOutcome::Refused
            )
        );
    }

    #[test]
    fn actions_without_a_backend_are_refused_as_unavailable() {
        let config = owner_config(Some("/srv/hatchling/schedules"));
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let ops = Ops::new(&config, &key, &skills, None, Arc::new(Bus::starting_at(1)));
        let owner = caller(&ops.owner_grants().into_iter().collect::<Vec<_>>());
        let mut replies = Replies::default();
        // No backend in this build.
        let unavailable = (
            UiActionResult::Refused {
                reason: ActionRefusal::Unavailable,
            },
            ApiOutcome::Refused,
        );
        let id = || String::from("x");
        for action in [
            UiAction::OpenArtifact {
                artifact_id: id(),
                revision: 1,
            },
            UiAction::ShareArtifact {
                artifact_id: id(),
                revision: 1,
            },
            UiAction::AttachArtifact {
                chat_id: id(),
                artifact_id: id(),
                revision: 1,
            },
            UiAction::SaveFile {
                chat_id: None,
                name: id(),
                format: FileFormat::Text,
                content: FileContent::Text { text: id() },
            },
            UiAction::TakeControl { session_id: id() },
            UiAction::ReturnControl {
                session_id: id(),
                frame_sequence: 1,
            },
            UiAction::StopBrowser { session_id: id() },
        ] {
            assert_eq!(
                ops.handle(&owner, &action, &mut replies),
                unavailable,
                "{action:?}"
            );
        }
        // Without the local channel's hub, a message has nowhere to go.
        assert_eq!(ops.handle(&owner, &send("hi"), &mut replies), unavailable);
    }

    #[test]
    fn a_message_reaches_the_agent_and_its_replies_reach_the_stream_until_the_connection_ends() {
        let mut config = owner_config(Some("/srv/hatchling/schedules"));
        config.goals.grants = Vec::new();
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let bus = Arc::new(Bus::starting_at(1));
        let ops = Ops::new(&config, &key, &skills, Some(&hub), Arc::clone(&bus));
        assert!(Arc::ptr_eq(ops.bus(), &bus));
        let owner = caller(&[local_send()]);
        let bad = (
            UiActionResult::Refused {
                reason: ActionRefusal::BadRequest,
            },
            ApiOutcome::Refused,
        );
        let mut replies = Replies::default();
        let elsewhere = UiAction::SendMessage {
            chat_id: String::from("main"),
            text: String::from("hi"),
        };
        assert_eq!(ops.handle(&owner, &elsewhere, &mut replies), bad);
        assert_eq!(
            ops.handle(
                &owner,
                &send(&"x".repeat(MAX_INGRESS_TEXT_BYTES + 1)),
                &mut replies
            ),
            bad
        );
        assert_eq!(hub.queued(), 0, "nothing reached the agent");
        let (accepted, outcome) = ops.handle(&owner, &send("What is on today?"), &mut replies);
        assert_eq!(outcome, ApiOutcome::Served);
        let accepted_id = if let UiActionResult::MessageAccepted { message_id } = &accepted {
            message_id.as_str()
        } else {
            ""
        };
        assert!(accepted_id.starts_with("msg-"), "{accepted:?}");
        let queued = hub.take().unwrap();
        assert_eq!(
            (queued.channel, queued.text.as_str()),
            (ChannelId::local(), "What is on today?")
        );
        let shown = format!("{replies:?}");
        assert!(shown.contains("attached: true"));
        hub.deliver(
            &Message {
                sender: SenderId(String::from("agent")),
                channel: ChannelId::local(),
                text: String::from("Two meetings."),
            },
            "s",
        )
        .unwrap();
        assert_eq!(
            chats(&bus),
            [
                (ChatAuthor::User, String::from("What is on today?")),
                (ChatAuthor::Agent, String::from("Two meetings."))
            ]
        );
        // A second message keeps the one route.
        assert_eq!(
            ops.handle(&owner, &send("Thanks."), &mut replies).1,
            ApiOutcome::Served
        );
        assert_eq!(
            hub.take().map(|msg| msg.text),
            Some(String::from("Thanks."))
        );
        // A full queue refuses, and the agent never sees the message.
        for _ in 0..hatchling_channel::MAX_QUEUED_MESSAGES {
            let _ = hub.inject(Message {
                sender: SenderId(String::new()),
                channel: ChannelId::local(),
                text: String::new(),
            });
        }
        assert_eq!(
            ops.handle(&owner, &send("one more"), &mut replies),
            (
                UiActionResult::Refused {
                    reason: ActionRefusal::Unavailable
                },
                ApiOutcome::Refused
            )
        );
        // The connection ends: its route is gone.
        drop(replies);
        assert!(
            hub.deliver(
                &Message {
                    sender: SenderId(String::new()),
                    channel: ChannelId::local(),
                    text: String::from("late")
                },
                "s"
            )
            .is_err()
        );
        let shown = format!("{:?}", Replies::default());
        assert!(shown.contains("attached: false"));
    }

    #[test]
    fn the_agent_works_on_a_message_until_its_reply_arrives() {
        let mut config = owner_config(Some("/srv/hatchling/schedules"));
        config.goals.grants = Vec::new();
        let key = MasterKey::generate().unwrap();
        let skills = Skills::load(&SkillConfig::disabled()).unwrap();
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let bus = Arc::new(Bus::starting_at(1));
        let ops = Ops::new(&config, &key, &skills, Some(&hub), Arc::clone(&bus));
        let mut replies = Replies::default();
        let owner = caller(&[local_send()]);
        let sent = ops.handle(&owner, &send("Plan the week.\nThen book it."), &mut replies);
        assert_eq!(sent.1, ApiOutcome::Served);
        let working = (AgentState::Working, String::from("Plan the week.\u{2026}"));
        assert_eq!(statuses(&bus), std::slice::from_ref(&working));
        hub.deliver(
            &Message {
                sender: SenderId(String::from("agent")),
                channel: ChannelId::local(),
                text: String::from("Booked."),
            },
            "s",
        )
        .unwrap();
        assert_eq!(statuses(&bus), [working, (AgentState::Idle, String::new())]);
        // Each status follows the message it is about.
        let order: Vec<bool> = bus
            .after(None)
            .into_iter()
            .filter(|event| {
                matches!(
                    event.body,
                    UiEventBody::ChatMessage(_) | UiEventBody::AgentStatus { .. }
                )
            })
            .map(|event| matches!(event.body, UiEventBody::AgentStatus { .. }))
            .collect();
        assert_eq!(order, [false, true, false, true]);
    }

    #[test]
    fn every_call_is_recorded_owner_only_with_its_kind_and_never_its_content() {
        let dir = scratch("audit");
        let log = dir.0.join("schedules.activity");
        let mut audit = Audit::new(log.clone(), String::from("api-conn"));
        let named = caller(&[goals("query-goals")]);
        let first = audit.record(Some(&named), ApiRequest::SendMessage, ApiOutcome::Served);
        let second = audit.record(None, ApiRequest::Hello, ApiOutcome::Refused);
        assert_eq!(
            (first.event_id.as_str(), first.run_id.as_str()),
            ("api-conn:1", API_RUN_ID)
        );
        assert_eq!(second.event_id, "api-conn:2");
        assert_eq!(
            second.kind,
            SubagentEvent::ApiCall {
                caller: String::from("unidentified"),
                capabilities: Vec::new(),
                request: ApiRequest::Hello,
                outcome: ApiOutcome::Refused
            }
        );
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<ActivityEvent> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines, [first, second]);
        assert!(text.contains("\"caller\":\"uid 1000\"") && text.contains("goals:query-goals"));
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // A log that cannot be written is logged, never a failure of the call.
        let mut lost = Audit::new(
            dir.0.join("missing").join("schedules.activity"),
            String::from("api-lost"),
        );
        assert_eq!(
            lost.record(None, ApiRequest::Subscribe, ApiOutcome::Served)
                .event_id,
            "api-lost:1"
        );
    }

    #[test]
    fn a_route_is_taken_once_and_given_back_when_dropped() {
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let bus = Arc::new(Bus::starting_at(1));
        let mut replies = Replies::default();
        replies.route(&hub, &bus);
        replies.route(&hub, &bus);
        let reply = |text: &str| Message {
            sender: SenderId(String::from("agent")),
            channel: ChannelId::local(),
            text: text.to_owned(),
        };
        hub.deliver(&reply("routed"), "s").unwrap();
        assert_eq!(chats(&bus), [(ChatAuthor::Agent, String::from("routed"))]);
        drop(replies);
        assert!(
            hub.deliver(&reply("after"), "s").is_err(),
            "the route is given back"
        );
    }
}
