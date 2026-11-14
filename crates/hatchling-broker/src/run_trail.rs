//! What an agent session did, as the broker served it: each tool it used
//! and how each use ended, and its model calls.
//!
//! The broker answers every request a cell makes, so it knows which tools
//! a run really used, whatever the agent says it did. A session's
//! [`Trail`] notes each request and its answer as they pass, and when the
//! session ends, however it ends, writes one `run_summary` event to the
//! activity log: the owner's Activity shows it as the run's trail, so an
//! answer that claims a tool did something the trail doesn't show stands
//! out.
//!
//! - **Named by what a tool does,** never by what it carried: no
//!   arguments, answers, pages, or secrets.
//! - **Model calls are counted apart:** how many, how many the broker
//!   refused at a usage limit, the tokens the largest one read (how full
//!   the model's context got), and the tokens written.
//! - **Plumbing isn't a tool:** minting a surrogate, staging a body or
//!   reading a held answer are part of the request they serve.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hatchling_core::{
    ActivityEvent, ArtifactResult, BrowserResult, ChannelOp, ChannelResult, EgressOp, EgressResult,
    GoalsResult, HookResult, MemoryOp, MemoryResult, QueryOp, QueryResult, RoomResult, ScheduleOp,
    ScheduleResult, SessionRequest, SessionResponse, SkillResult, SpaceResult, SubagentEvent,
    SubagentResult, SuggestResult, ToolTally, ToolUse, VaultResult, WebOp, WebResult,
};

/// How one use of a tool ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// It answered.
    Done,
    /// It was put to the owner, to approve or answer.
    Asked,
    /// It was refused: no grant, a limit, or the owner said no.
    Refused,
    /// It failed.
    Failed,
}

/// What a session did so far, and where its summary goes when it ends.
#[derive(Debug)]
pub struct Trail {
    log: Option<PathBuf>,
    run_id: String,
    cell_id: String,
    chat: bool,
    /// Where the owner's model usage is counted, and their limits kept.
    usage: PathBuf,
    /// The model's context window, in tokens: the declared one, until
    /// its server says its own.
    context_window: Option<u32>,
    /// The model's endpoint and name, to ask its server the window when
    /// the session ends.
    model: Option<(String, String)>,
    /// Bytes of the model on the CPU when the session ended, as its
    /// server says.
    model_on_cpu: Option<u64>,
    /// Whether the computer was low on memory when the session ended.
    memory_low: bool,
    /// Where the computer's memory is read: [`crate::model_window::memory`].
    memory: fn() -> Option<(u64, u64)>,
    tools: Vec<ToolTally>,
    model_calls: u32,
    model_calls_refused: u32,
    peak_input_tokens: u64,
    output_tokens: u64,
    /// The owner's app, told each step a chat run starts.
    app: Option<Arc<crate::app_stream::AppStream>>,
    /// The step last told.
    step: Option<Step>,
}

/// A step a chat run is on, as its owner's app is told it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// It waits for its model.
    Model,
    /// It is using a tool.
    Tool(ToolUse),
}

impl Trail {
    /// The trail of the session `run_id`, in cell `cell_id`, summarized
    /// in the activity log `log` when it ends; with no log, only in the
    /// broker's own.
    #[must_use]
    pub fn new(log: Option<PathBuf>, run_id: &str, cell_id: &str) -> Self {
        Self {
            log,
            run_id: run_id.to_owned(),
            cell_id: cell_id.to_owned(),
            chat: false,
            usage: PathBuf::new(),
            context_window: None,
            model: None,
            model_on_cpu: None,
            memory_low: false,
            memory: crate::model_window::memory,
            tools: Vec::new(),
            model_calls: 0,
            model_calls_refused: 0,
            peak_input_tokens: 0,
            output_tokens: 0,
            app: None,
            step: None,
        }
    }

    /// The trail, telling the owner's `app` each step a chat run starts
    /// ([`Self::starting`]).
    #[must_use]
    pub fn telling(mut self, app: Arc<crate::app_stream::AppStream>) -> Self {
        self.app = Some(app);
        self
    }

    /// Tells the owner's app that a chat run started a step: `tool`, or
    /// none for a call to its model. Only a change of step is told: a tool
    /// used again straight after itself is the same step going on. A run
    /// that isn't answering the owner tells nothing.
    ///
    /// The app is told what kind of thing is being done, as the summary
    /// says it afterwards, and nothing of what with.
    pub fn starting(&mut self, tool: Option<ToolUse>) {
        let step = tool.map_or(Step::Model, Step::Tool);
        if !self.chat || self.step == Some(step) {
            return;
        }
        if let Some(app) = &self.app {
            app.step(tool);
        }
        self.step = Some(step);
    }

    /// The trail, of a chat run when `chat` is set: answering the owner.
    #[must_use]
    pub const fn of_chat(mut self, chat: bool) -> Self {
        self.chat = chat;
        self
    }

    /// The trail, measuring prompts against the model's declared context
    /// window, `window`, when there is one.
    #[must_use]
    pub const fn with_window(mut self, window: Option<u32>) -> Self {
        self.context_window = window;
        self
    }

    /// The trail, asking the server at `endpoint` the window it gives
    /// the model `name` when the session ends, after its model calls
    /// ([`crate::model_window`]): the window the server says is the one
    /// it keeps to, so it's used over the declared one.
    #[must_use]
    pub fn asking(mut self, endpoint: Option<&str>, name: Option<&str>) -> Self {
        self.model = endpoint
            .zip(name)
            .map(|(endpoint, name)| (endpoint.to_owned(), name.to_owned()));
        self
    }

    /// The trail, reading the computer's memory with `memory`.
    #[cfg(test)]
    pub(crate) const fn reading_memory(mut self, memory: fn() -> Option<(u64, u64)>) -> Self {
        self.memory = memory;
        self
    }

    /// Asks the model's server the window it gives the model, and how
    /// much of it is on the GPU, and reads the computer's memory, after
    /// the session's model calls: with none, the model may not be
    /// loaded, and the question isn't worth a connection.
    fn learn_window(&mut self) {
        if self.model_calls == 0 {
            return;
        }
        self.memory_low = (self.memory)()
            .is_some_and(|(available, total)| crate::model_window::memory_low(available, total));
        let Some((endpoint, name)) = &self.model else {
            return;
        };
        if let Some(loaded) = crate::model_window::learned(endpoint, name) {
            if let Some(window) = loaded.window {
                self.context_window = Some(window);
            }
            self.model_on_cpu = Some(loaded.on_cpu());
        }
    }

    /// The trail, counting model calls in the usage directory `usage`,
    /// and checking the owner's limits kept there ([`crate::usage`]).
    #[must_use]
    pub fn counting_in(mut self, usage: &Path) -> Self {
        usage.clone_into(&mut self.usage);
        self
    }

    /// The owner's usage limit reached at `now` (Unix seconds), if one
    /// is: a model call about to go is refused, and noted so.
    pub fn over_limit(&mut self, now: i64) -> Option<crate::usage::Reached> {
        let reached = crate::usage::reached(&self.usage, now)?;
        self.model_calls_refused = self.model_calls_refused.saturating_add(1);
        Some(reached)
    }

    /// Counts `request`'s answer `response`, at `at` (Unix seconds): a
    /// model call answered is the owner's usage, and the session's.
    pub fn count(
        &mut self,
        request: &hatchling_egress::EgressRequest,
        response: &hatchling_egress::EgressResponse,
        at: i64,
    ) {
        if let Some((input, output)) = crate::usage::count(&self.usage, request, response, at) {
            self.model_call(input, output);
        }
    }

    /// Notes a use of `tool`, the tool a request used ([`tool_of`]), if
    /// it used one, answered with `response`: by how it ended. A model
    /// call is noted apart ([`Self::model_call`]).
    pub fn note(&mut self, tool: Option<ToolUse>, response: &SessionResponse) {
        let Some(tool) = tool else {
            return;
        };
        let ending = ending_of(response);
        let at = if let Some(at) = self.tools.iter().position(|tally| tally.tool == tool) {
            at
        } else {
            self.tools.push(ToolTally {
                tool,
                done: 0,
                asked: 0,
                refused: 0,
                failed: 0,
            });
            self.tools.len() - 1
        };
        let Some(tally) = self.tools.get_mut(at) else {
            return;
        };
        let count = match ending {
            Ending::Done => &mut tally.done,
            Ending::Asked => &mut tally.asked,
            Ending::Refused => &mut tally.refused,
            Ending::Failed => &mut tally.failed,
        };
        *count = count.saturating_add(1);
    }

    /// Notes a model call answered, with the tokens it read and wrote as
    /// its server reported them.
    pub fn model_call(&mut self, input: Option<u64>, output: Option<u64>) {
        self.model_calls = self.model_calls.saturating_add(1);
        self.peak_input_tokens = self.peak_input_tokens.max(input.unwrap_or(0));
        self.output_tokens = self.output_tokens.saturating_add(output.unwrap_or(0));
    }

    /// The summary, if the session did anything to summarize.
    #[must_use]
    pub fn summary(&self) -> Option<SubagentEvent> {
        (!self.tools.is_empty() || self.model_calls > 0 || self.model_calls_refused > 0).then(
            || SubagentEvent::RunSummary {
                cell_id: self.cell_id.clone(),
                chat: self.chat,
                tools: self.tools.clone(),
                model_calls: self.model_calls,
                model_calls_refused: self.model_calls_refused,
                peak_input_tokens: self.peak_input_tokens,
                output_tokens: self.output_tokens,
                context_window: self.context_window,
                model_on_cpu: self.model_on_cpu,
                memory_low: self.memory_low,
            },
        )
    }
}

impl Drop for Trail {
    /// The session ended, however it ended: its summary is written.
    fn drop(&mut self) {
        self.learn_window();
        let Some(kind) = self.summary() else {
            return;
        };
        let event = ActivityEvent {
            event_id: format!("{}:summary", self.run_id),
            ts: hatchling_scheduler::Timestamp::now().to_string(),
            run_id: self.run_id.clone(),
            kind,
        };
        crate::subagent_ops::record(self.log.as_deref(), &event, "run summary");
    }
}

/// Where a session's activity goes: the store's activity log when
/// scheduling is on, and a run's, through its follow-up scope, since a
/// run's own scheduling is off; none otherwise.
#[must_use]
pub fn activity_log(config: &crate::Config) -> Option<PathBuf> {
    if config.scheduler.enabled() {
        return Some(config.scheduler.activity_log());
    }
    config
        .scheduler
        .followup
        .as_ref()
        .map(|followup| followup.root.as_path())
        .filter(|root| !root.as_os_str().is_empty())
        .map(|root: &Path| root.with_extension("activity"))
}

/// The tool `request` uses, if it's one: a model call, and the egress's
/// plumbing, aren't.
#[must_use]
pub fn tool_of(request: &SessionRequest) -> Option<ToolUse> {
    Some(match request {
        SessionRequest::Vault(_) => ToolUse::Vault,
        SessionRequest::Egress(EgressOp::Egress { method, path, .. }) => {
            if crate::usage::is_model_call(method, path) {
                return None;
            }
            ToolUse::Network
        }
        SessionRequest::Egress(_) => return None,
        SessionRequest::Channel(ChannelOp::Send { .. }) => ToolUse::MessageSend,
        SessionRequest::Channel(_) => ToolUse::MessageRead,
        SessionRequest::Memory(MemoryOp::Propose { .. }) => ToolUse::MemoryPropose,
        SessionRequest::Memory(
            MemoryOp::Recall { .. } | MemoryOp::RecallByTag { .. } | MemoryOp::ContextRead { .. },
        )
        | SessionRequest::Query(QueryOp::Memory { .. }) => ToolUse::MemoryRecall,
        SessionRequest::Memory(_) => ToolUse::MemoryWrite,
        SessionRequest::Skill(_) => ToolUse::Skill,
        SessionRequest::Goals(_) => ToolUse::GoalsWrite,
        SessionRequest::Query(_) => ToolUse::GoalsRead,
        SessionRequest::Schedule(ScheduleOp::Propose { .. }) => ToolUse::SchedulePropose,
        SessionRequest::Schedule(ScheduleOp::List { .. } | ScheduleOp::ListParked { .. }) => {
            ToolUse::ScheduleRead
        }
        SessionRequest::Schedule(_) => ToolUse::ScheduleWrite,
        SessionRequest::Hook(_) => ToolUse::Hook,
        SessionRequest::Artifact(_) => ToolUse::Artifact,
        SessionRequest::Space(_) => ToolUse::Space,
        SessionRequest::Subagent(_) => ToolUse::Subagent,
        SessionRequest::Web(WebOp::Search { .. }) => ToolUse::WebSearch,
        SessionRequest::Web(_) => ToolUse::WebPage,
        SessionRequest::Browser(_) => ToolUse::Browser,
        SessionRequest::Suggest(_) => ToolUse::IdeaSuggest,
        SessionRequest::Room(_) => ToolUse::Room,
    })
}

/// How the use `response` answers ended.
const fn ending_of(response: &SessionResponse) -> Ending {
    match response {
        SessionResponse::Vault(result) => vault_ending(result),
        SessionResponse::Egress(result) => egress_ending(result),
        SessionResponse::Channel(result) => channel_ending(result),
        SessionResponse::Memory(result) => memory_ending(result),
        SessionResponse::Skill(result) => skill_ending(result),
        SessionResponse::Goals(result) => goals_ending(result),
        SessionResponse::Query(result) => query_ending(result),
        SessionResponse::Schedule(result) => schedule_ending(result),
        SessionResponse::Hook(result) => hook_ending(result),
        SessionResponse::Artifact(result) => artifact_ending(result),
        SessionResponse::Space(result) => space_ending(result),
        SessionResponse::Subagent(result) => subagent_ending(result),
        SessionResponse::Web(result) => web_ending(result),
        SessionResponse::Browser(result) => browser_ending(result),
        SessionResponse::Suggest(result) => suggest_ending(result),
        SessionResponse::Room(result) => room_ending(result),
    }
}

/// How a use answered with `result` ended.
const fn vault_ending(result: &VaultResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        VaultResult::Denied => Refused,
        VaultResult::VaultError => Failed,
        VaultResult::Stored
        | VaultResult::Fetched { .. }
        | VaultResult::Removed
        | VaultResult::NotFound => Done,
    }
}

/// How a use answered with `result` ended.
const fn egress_ending(result: &EgressResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        EgressResult::Denied => Refused,
        EgressResult::EgressError => Failed,
        EgressResult::Surrogate { .. }
        | EgressResult::Ok { .. }
        | EgressResult::Staged { .. }
        | EgressResult::Part { .. } => Done,
    }
}

/// How a use answered with `result` ended.
const fn channel_ending(result: &ChannelResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        ChannelResult::Denied | ChannelResult::Redacted { .. } => Refused,
        ChannelResult::ChannelError => Failed,
        ChannelResult::Message { .. }
        | ChannelResult::NoMessage
        | ChannelResult::Sent
        | ChannelResult::Accounts { .. }
        | ChannelResult::UnknownAccount => Done,
    }
}

/// How a use answered with `result` ended.
const fn memory_ending(result: &MemoryResult) -> Ending {
    use Ending::{Asked, Done, Failed, Refused};
    match result {
        MemoryResult::Proposed => Asked,
        MemoryResult::Denied | MemoryResult::Unasked { .. } => Refused,
        MemoryResult::MemoryError => Failed,
        MemoryResult::Stored
        | MemoryResult::Item { .. }
        | MemoryResult::Items { .. }
        | MemoryResult::NotFound
        | MemoryResult::Removed { .. }
        | MemoryResult::ContextAppended
        | MemoryResult::ContextEntries { .. } => Done,
    }
}

/// How a use answered with `result` ended.
const fn skill_ending(result: &SkillResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        SkillResult::UnknownSkill | SkillResult::CapabilityDenied => Refused,
        SkillResult::SkillError => Failed,
        SkillResult::Output { .. } => Done,
    }
}

/// How a use answered with `result` ended.
const fn goals_ending(result: &GoalsResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        GoalsResult::Rejected | GoalsResult::LimitReached | GoalsResult::Denied => Refused,
        GoalsResult::GoalsError => Failed,
        GoalsResult::GoalCreated { .. }
        | GoalsResult::GoalChanged
        | GoalsResult::GoalNotFound
        | GoalsResult::GoalAlreadyClosed
        | GoalsResult::TrackingAppended { .. } => Done,
    }
}

/// How a use answered with `result` ended.
const fn query_ending(result: &QueryResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        QueryResult::Denied => Refused,
        QueryResult::QueryError => Failed,
        QueryResult::Items { .. } => Done,
    }
}

/// How a use answered with `result` ended.
const fn schedule_ending(result: &ScheduleResult) -> Ending {
    use Ending::{Asked, Done, Failed, Refused};
    match result {
        ScheduleResult::Proposed { .. } => Asked,
        ScheduleResult::Refused { .. }
        | ScheduleResult::Denied
        | ScheduleResult::Unasked { .. } => Refused,
        ScheduleResult::ScheduleError => Failed,
        ScheduleResult::Created { .. }
        | ScheduleResult::Cancelled
        | ScheduleResult::NotFound
        | ScheduleResult::Listed { .. }
        | ScheduleResult::Resolved
        | ScheduleResult::Retried
        | ScheduleResult::Parked { .. }
        | ScheduleResult::Dismissed => Done,
    }
}

/// How a use answered with `result` ended.
const fn hook_ending(result: &HookResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        HookResult::Refused { .. } | HookResult::Denied => Refused,
        HookResult::HookError => Failed,
        HookResult::Created { .. } | HookResult::Cancelled | HookResult::NotFound => Done,
    }
}

/// How a use answered with `result` ended.
const fn artifact_ending(result: &ArtifactResult) -> Ending {
    use Ending::{Asked, Done, Failed, Refused};
    match result {
        ArtifactResult::ActivationProposed { .. } => Asked,
        ArtifactResult::Refused { .. }
        | ArtifactResult::ApprovalRefused { .. }
        | ArtifactResult::IdeaRefused { .. }
        | ArtifactResult::Denied => Refused,
        ArtifactResult::BuildFailed { .. } | ArtifactResult::ArtifactError => Failed,
        ArtifactResult::Built { .. }
        | ArtifactResult::Delivered { .. }
        | ArtifactResult::Described { .. }
        | ArtifactResult::UnknownBuild
        | ArtifactResult::IdeaSaved { .. }
        | ArtifactResult::Ideas { .. } => Done,
    }
}

/// How a use answered with `result` ended.
const fn space_ending(result: &SpaceResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        SpaceResult::Refused { .. } | SpaceResult::Denied => Refused,
        SpaceResult::SpaceError => Failed,
        SpaceResult::Created { .. }
        | SpaceResult::Admitted { .. }
        | SpaceResult::Stored
        | SpaceResult::Value { .. }
        | SpaceResult::Removed
        | SpaceResult::TaskAdded { .. }
        | SpaceResult::TaskQueued { .. }
        | SpaceResult::PairingCode { .. }
        | SpaceResult::Changes { .. }
        | SpaceResult::Pushed { .. }
        | SpaceResult::ConflictLog { .. }
        | SpaceResult::MediaStored { .. }
        | SpaceResult::Media { .. } => Done,
    }
}

/// How a use answered with `result` ended.
const fn subagent_ending(result: &SubagentResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        SubagentResult::Refused { .. } => Refused,
        SubagentResult::SubagentError => Failed,
        SubagentResult::Spawned { .. } | SubagentResult::Report(_) => Done,
    }
}

/// How a use answered with `result` ended.
const fn web_ending(result: &WebResult) -> Ending {
    use Ending::{Asked, Done, Refused};
    match result {
        WebResult::Waiting { .. } => Asked,
        WebResult::Denied { .. }
        | WebResult::Expired { .. }
        | WebResult::Unasked { .. }
        | WebResult::Refused { .. }
        | WebResult::Off => Refused,
        WebResult::Page { .. } | WebResult::Results { .. } | WebResult::Redirect { .. } => Done,
    }
}

/// How a use answered with `result` ended.
const fn browser_ending(result: &BrowserResult) -> Ending {
    use Ending::{Asked, Done, Failed, Refused};
    match result {
        BrowserResult::Waiting { .. } | BrowserResult::CodeWaiting { .. } => Asked,
        BrowserResult::Denied { .. }
        | BrowserResult::Expired { .. }
        | BrowserResult::Unasked { .. }
        | BrowserResult::Refused { .. }
        | BrowserResult::Off
        | BrowserResult::CodeDeclined
        | BrowserResult::CodeExpired => Refused,
        BrowserResult::Failed { .. } => Failed,
        BrowserResult::Page { .. }
        | BrowserResult::Allowed { .. }
        | BrowserResult::Closed
        | BrowserResult::Stopped
        | BrowserResult::OwnerDriving
        | BrowserResult::HandedBack { .. } => Done,
    }
}

/// How a use answered with `result` ended.
const fn suggest_ending(result: &SuggestResult) -> Ending {
    use Ending::{Done, Refused};
    match result {
        SuggestResult::Unasked { .. } => Refused,
        SuggestResult::Suggested => Done,
    }
}

/// How a use answered with `result` ended. A command that ran is done,
/// however it came out: its exit is the agent's to read. So is a page of
/// a file that was read; a file that couldn't be read is a failure.
const fn room_ending(result: &RoomResult) -> Ending {
    use Ending::{Done, Failed, Refused};
    match result {
        RoomResult::Ended { .. } | RoomResult::Running { .. } | RoomResult::Page { .. } => Done,
        RoomResult::Refused { .. } | RoomResult::Unknown { .. } => Refused,
        RoomResult::Lost { .. } | RoomResult::Failed { .. } | RoomResult::NoPage { .. } => Failed,
    }
}

#[cfg(test)]
mod tests {
    use hatchling_core::{GoalsOp, RoomOp, SuggestOp, SuggestionArea, WebOp};

    use super::*;

    fn s(text: &str) -> String {
        String::from(text)
    }

    fn egress(method: &str, path: &str) -> SessionRequest {
        SessionRequest::Egress(EgressOp::Egress {
            method: s(method),
            destination: s("localhost:11434"),
            path: s(path),
            headers: Vec::new(),
            body_b64: String::new(),
            surrogate_b64: String::new(),
            staged: 0,
        })
    }

    /// A tool is named by what it does, whatever it carried; a model call
    /// and the egress's plumbing aren't tools.
    #[test]
    fn each_request_is_the_tool_it_uses() {
        let cases = [
            (
                SessionRequest::Query(QueryOp::Goals {
                    status: s("open"),
                    updated_since_secs: None,
                    after: None,
                }),
                Some(ToolUse::GoalsRead),
            ),
            (
                SessionRequest::Goals(GoalsOp::GoalClose {
                    id: s("g1"),
                    outcome: s("reached"),
                }),
                Some(ToolUse::GoalsWrite),
            ),
            (
                SessionRequest::Memory(MemoryOp::Recall {
                    scope: s("user"),
                    id: s("m1"),
                }),
                Some(ToolUse::MemoryRecall),
            ),
            (
                SessionRequest::Memory(MemoryOp::Propose {
                    content: s("likes tea"),
                }),
                Some(ToolUse::MemoryPropose),
            ),
            (
                SessionRequest::Schedule(ScheduleOp::List { after: None }),
                Some(ToolUse::ScheduleRead),
            ),
            (
                SessionRequest::Web(WebOp::Search { query: s("q") }),
                Some(ToolUse::WebSearch),
            ),
            (
                SessionRequest::Web(WebOp::Fetch {
                    url: s("https://example.com"),
                    reason: s("r"),
                }),
                Some(ToolUse::WebPage),
            ),
            (
                SessionRequest::Channel(ChannelOp::Send {
                    channel: s("local"),
                    text: s("hi"),
                    state_changing: false,
                    approval: None,
                    account: None,
                }),
                Some(ToolUse::MessageSend),
            ),
            (
                SessionRequest::Channel(ChannelOp::Receive {
                    channel: s("local"),
                    account: None,
                }),
                Some(ToolUse::MessageRead),
            ),
            (
                SessionRequest::Suggest(SuggestOp::Suggest {
                    title: s("t"),
                    summary: s("s"),
                    why: s("w"),
                    area: SuggestionArea::Learning,
                }),
                Some(ToolUse::IdeaSuggest),
            ),
            (
                SessionRequest::Room(RoomOp::Stop { job: s("j") }),
                Some(ToolUse::Room),
            ),
            (egress("GET", "/v1/models"), Some(ToolUse::Network)),
            (egress("POST", "/v1/chat/completions"), None),
            (SessionRequest::Egress(EgressOp::Read {}), None),
        ];
        for (request, tool) in cases {
            assert_eq!(tool_of(&request), tool, "{request:?}");
        }
    }

    /// The steps told on `bus`, in order: the tool, or none for the model.
    fn steps_told(bus: &crate::api_stream::Bus) -> Vec<Option<ToolUse>> {
        bus.after(None)
            .into_iter()
            .filter_map(|event| {
                if let hatchling_core::ui::UiEventBody::AgentStep { tool } = event.body {
                    Some(tool)
                } else {
                    None
                }
            })
            .collect()
    }

    /// A stream with an app attached, and what that app is told.
    fn attached() -> (
        Arc<crate::app_stream::AppStream>,
        Arc<crate::api_stream::Bus>,
    ) {
        let app = Arc::new(crate::app_stream::AppStream::default());
        let bus = Arc::new(crate::api_stream::Bus::starting_at(1));
        app.attach(Arc::clone(&bus));
        (app, bus)
    }

    /// A chat run's steps are told to the owner's app as they change, and
    /// only then: a tool used again straight after itself is one step. A
    /// run that isn't answering the owner tells nothing, and a trail with
    /// no app to tell goes on as it did.
    #[test]
    fn a_chat_runs_steps_are_told_as_they_change() {
        let (app, bus) = attached();
        let mut trail = Trail::new(None, "run", "cell").of_chat(true).telling(app);
        for step in [
            None,
            None,
            Some(ToolUse::Room),
            Some(ToolUse::Room),
            None,
            Some(ToolUse::MemoryRecall),
            Some(ToolUse::Room),
        ] {
            trail.starting(step);
        }
        assert_eq!(
            steps_told(&bus),
            [
                None,
                Some(ToolUse::Room),
                None,
                Some(ToolUse::MemoryRecall),
                Some(ToolUse::Room)
            ]
        );

        let (app, bus) = attached();
        let mut background = Trail::new(None, "run", "cell").telling(app);
        background.starting(None);
        background.starting(Some(ToolUse::Room));
        assert_eq!(steps_told(&bus), [], "not a chat run");

        let mut alone = Trail::new(None, "run", "cell").of_chat(true);
        alone.starting(None);
        alone.starting(Some(ToolUse::Room));
    }

    /// Each use is counted by how it ended, under its tool, in the order
    /// tools were first used; model calls apart, the largest prompt kept.
    #[test]
    fn a_trail_tallies_each_use_by_how_it_ended() {
        let mut trail = Trail::new(None, "session-1", "run-1");
        assert_eq!(trail.summary(), None, "nothing done, nothing to say");
        let goals = Some(ToolUse::GoalsRead);
        let memory = Some(ToolUse::MemoryPropose);
        trail.note(
            goals,
            &SessionResponse::Query(QueryResult::Items {
                items_json: Vec::new(),
                next: None,
            }),
        );
        trail.note(memory, &SessionResponse::Memory(MemoryResult::Proposed));
        trail.note(goals, &SessionResponse::Query(QueryResult::Denied));
        trail.note(goals, &SessionResponse::Query(QueryResult::QueryError));
        trail.note(
            memory,
            &SessionResponse::Memory(MemoryResult::Unasked {
                reason: s("paused"),
            }),
        );
        trail.note(None, &SessionResponse::Egress(EgressResult::Denied));
        trail.model_call(Some(5400), Some(300));
        trail.model_call(Some(900), None);
        trail.model_call(None, Some(20));
        trail.model_calls_refused = 1;
        let tally = |tool, done, asked, refused, failed| ToolTally {
            tool,
            done,
            asked,
            refused,
            failed,
        };
        assert_eq!(
            trail.summary(),
            Some(SubagentEvent::RunSummary {
                cell_id: s("run-1"),
                chat: false,
                tools: vec![
                    tally(ToolUse::GoalsRead, 1, 0, 1, 1),
                    tally(ToolUse::MemoryPropose, 0, 1, 1, 0),
                ],
                model_calls: 3,
                model_calls_refused: 1,
                peak_input_tokens: 5400,
                output_tokens: 320,
                context_window: None,
                model_on_cpu: None,
                memory_low: false,
            })
        );
    }

    /// A model call answered is counted in the usage directory and the
    /// trail; past the owner's limit, the next is refused and noted.
    #[test]
    fn a_trail_counts_model_calls_and_refuses_past_a_limit() {
        let usage =
            std::env::temp_dir().join(format!("hatchling-trail-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&usage);
        let mut trail = Trail::new(None, "session-1", "run-1").counting_in(&usage);
        let now = hatchling_scheduler::Timestamp::now().as_second();
        assert_eq!(trail.over_limit(now), None, "no limit set");
        let request = hatchling_egress::EgressRequest {
            method: s("POST"),
            destination: s("localhost:11434"),
            path: s("/v1/chat/completions"),
            headers: Vec::new(),
            body: Vec::new(),
            surrogate_b64: String::new(),
        };
        let reply = |status| hatchling_egress::EgressResponse {
            status,
            headers: Vec::new(),
            body: br#"{"model":"m","usage":{"prompt_tokens":40,"completion_tokens":2}}"#.to_vec(),
        };
        trail.count(&request, &reply(500), now);
        trail.count(&request, &reply(200), now);
        crate::usage::set_limits(
            &usage,
            &hatchling_core::ui::UsageLimits {
                daily_tokens: Some(42),
                monthly_tokens: None,
            },
        )
        .unwrap();
        assert_eq!(trail.over_limit(now).map(|reached| reached.spent), Some(42));
        assert_eq!(
            trail.summary(),
            Some(SubagentEvent::RunSummary {
                cell_id: s("run-1"),
                chat: false,
                tools: Vec::new(),
                model_calls: 1,
                model_calls_refused: 1,
                peak_input_tokens: 40,
                output_tokens: 2,
                context_window: None,
                model_on_cpu: None,
                memory_low: false,
            })
        );
        let _ = std::fs::remove_dir_all(&usage);
    }

    /// When the session ends, its summary is written to the activity log
    /// once, as the run's event; a session that did nothing writes none.
    #[test]
    fn an_ended_session_writes_its_summary() {
        let log = std::env::temp_dir().join(format!("hatchling-trail-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        drop(Trail::new(Some(log.clone()), "quiet", "run-0"));
        assert!(!log.exists(), "nothing to say");
        let mut trail = Trail::new(Some(log.clone()), "session-1", "run-1");
        trail.note(
            Some(ToolUse::IdeaSuggest),
            &SessionResponse::Suggest(SuggestResult::Suggested),
        );
        drop(trail);
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let event: ActivityEvent = serde_json::from_str(lines.first().unwrap()).unwrap();
        assert_eq!(
            (event.event_id.as_str(), event.run_id.as_str()),
            ("session-1:summary", "session-1")
        );
        assert!(matches!(
            event.kind,
            SubagentEvent::RunSummary { ref tools, .. } if tools.len() == 1
        ));
        let _ = std::fs::remove_file(&log);
    }

    /// The window in each summary written to `log`, in order, and how
    /// many events it holds in all.
    fn windows_in(log: &Path) -> (Vec<Option<u32>>, usize) {
        let text = std::fs::read_to_string(log).unwrap();
        let windows = text
            .lines()
            .filter_map(|line| {
                let event: ActivityEvent = serde_json::from_str(line).unwrap();
                let SubagentEvent::RunSummary { context_window, .. } = event.kind else {
                    return None;
                };
                Some(context_window)
            })
            .collect();
        (windows, text.lines().count())
    }

    /// A session that called the model asks its server the window when
    /// it ends, and the server's is used over the declared one; one that
    /// didn't call it doesn't ask, and when the server doesn't say, the
    /// declared window stands.
    #[test]
    fn an_ended_session_learns_the_window_from_the_models_server() {
        let log =
            std::env::temp_dir().join(format!("hatchling-trail-window-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let (address, served) = crate::model_window::answering(vec![crate::model_window::loaded(
            r#"{"name":"m","context_length":16384}"#,
        )]);
        let endpoint = format!("http://{address}/v1");
        let trail = |id: &str, endpoint: &str| {
            Trail::new(Some(log.clone()), id, "run-1")
                .with_window(Some(32768))
                .asking(Some(endpoint), Some("m"))
        };
        let mut quiet = trail("no-calls", &endpoint);
        quiet.note(
            Some(ToolUse::IdeaSuggest),
            &SessionResponse::Suggest(SuggestResult::Suggested),
        );
        drop(quiet);
        let mut called = trail("called", &endpoint);
        called.model_call(Some(40), Some(2));
        drop(called);
        let heads = served.join().unwrap();
        assert_eq!(
            heads.len(),
            1,
            "only the session that called the model asked"
        );
        assert!(
            heads
                .first()
                .is_some_and(|head| head.starts_with("GET /api/ps ")),
            "{heads:?}"
        );
        let refusing = crate::test_port::RefusingPort::new();
        let mut unsaid = trail("unsaid", &format!("http://{}/v1", refusing.addr()));
        unsaid.model_call(Some(40), Some(2));
        drop(unsaid);
        let mut unasked =
            Trail::new(Some(log.clone()), "unasked", "run-1").with_window(Some(32768));
        unasked.model_call(Some(40), Some(2));
        drop(unasked);
        assert_eq!(
            windows_in(&log),
            (vec![Some(32768), Some(16384), Some(32768), Some(32768)], 4)
        );
        let _ = std::fs::remove_file(&log);
    }

    /// A session that called the model records how it fit, as the
    /// model's server says, and whether the computer was low on memory;
    /// one that didn't call it reads neither.
    #[test]
    fn an_ended_session_records_how_the_model_fit() {
        let (address, served) = crate::model_window::answering(vec![crate::model_window::loaded(
            r#"{"name":"m","context_length":131072,"size":10700000000,"size_vram":9800000000}"#,
        )]);
        let endpoint = format!("http://{address}/v1");
        let mut called = Trail::new(None, "called", "run-1")
            .asking(Some(&endpoint), Some("m"))
            .reading_memory(|| Some((1, 100)));
        called.model_call(Some(40), Some(2));
        called.learn_window();
        served.join().unwrap();
        assert!(matches!(
            called.summary(),
            Some(SubagentEvent::RunSummary {
                context_window: Some(131_072),
                model_on_cpu: Some(900_000_000),
                memory_low: true,
                ..
            })
        ));
        let mut roomy = Trail::new(None, "roomy", "run-1").reading_memory(|| Some((50, 100)));
        roomy.model_call(Some(40), Some(2));
        roomy.learn_window();
        assert!(matches!(
            roomy.summary(),
            Some(SubagentEvent::RunSummary {
                model_on_cpu: None,
                memory_low: false,
                ..
            })
        ));
        let mut quiet = Trail::new(None, "quiet", "run-1").reading_memory(|| Some((1, 100)));
        quiet.note(
            Some(ToolUse::IdeaSuggest),
            &SessionResponse::Suggest(SuggestResult::Suggested),
        );
        quiet.learn_window();
        assert!(matches!(
            quiet.summary(),
            Some(SubagentEvent::RunSummary {
                memory_low: false,
                ..
            })
        ));
    }

    /// Every answer is done, put to the owner, refused, or failed.
    #[test]
    fn each_answer_ends_one_way() {
        use Ending::{Asked, Done, Failed, Refused};
        let cases = [
            (SessionResponse::Vault(VaultResult::Stored), Done),
            (SessionResponse::Vault(VaultResult::Denied), Refused),
            (SessionResponse::Vault(VaultResult::VaultError), Failed),
            (SessionResponse::Egress(EgressResult::EgressError), Failed),
            (SessionResponse::Channel(ChannelResult::Sent), Done),
            (
                SessionResponse::Channel(ChannelResult::ChannelError),
                Failed,
            ),
            (SessionResponse::Memory(MemoryResult::Proposed), Asked),
            (SessionResponse::Memory(MemoryResult::MemoryError), Failed),
            (
                SessionResponse::Skill(SkillResult::CapabilityDenied),
                Refused,
            ),
            (SessionResponse::Skill(SkillResult::SkillError), Failed),
            (SessionResponse::Goals(GoalsResult::GoalChanged), Done),
            (SessionResponse::Goals(GoalsResult::LimitReached), Refused),
            (SessionResponse::Goals(GoalsResult::GoalsError), Failed),
            (SessionResponse::Schedule(ScheduleResult::Dismissed), Done),
            (SessionResponse::Schedule(ScheduleResult::Denied), Refused),
            (
                SessionResponse::Schedule(ScheduleResult::ScheduleError),
                Failed,
            ),
            (SessionResponse::Hook(HookResult::Cancelled), Done),
            (SessionResponse::Hook(HookResult::Denied), Refused),
            (SessionResponse::Hook(HookResult::HookError), Failed),
            (
                SessionResponse::Artifact(ArtifactResult::UnknownBuild),
                Done,
            ),
            (SessionResponse::Artifact(ArtifactResult::Denied), Refused),
            (
                SessionResponse::Artifact(ArtifactResult::ArtifactError),
                Failed,
            ),
            (SessionResponse::Space(SpaceResult::Removed), Done),
            (SessionResponse::Space(SpaceResult::Denied), Refused),
            (SessionResponse::Space(SpaceResult::SpaceError), Failed),
            (
                SessionResponse::Subagent(SubagentResult::SubagentError),
                Failed,
            ),
            (SessionResponse::Web(WebResult::Off), Refused),
            (SessionResponse::Browser(BrowserResult::Closed), Done),
            (
                SessionResponse::Browser(BrowserResult::CodeDeclined),
                Refused,
            ),
            (SessionResponse::Suggest(SuggestResult::Suggested), Done),
            (
                SessionResponse::Suggest(SuggestResult::Unasked { reason: s("full") }),
                Refused,
            ),
        ];
        for (response, ending) in cases {
            assert_eq!(ending_of(&response), ending, "{response:?}");
        }
    }

    /// A command that ran and a page that was read are done; what the
    /// session may not ask is refused; a Room that couldn't answer, and
    /// a file that couldn't be read, are failures.
    #[test]
    fn each_answer_about_the_room_ends_one_way() {
        use Ending::{Done, Failed, Refused};
        let cases = [
            (
                SessionResponse::Room(RoomResult::Running {
                    job: s("j"),
                    ran_ms: 1,
                }),
                Done,
            ),
            (
                SessionResponse::Room(RoomResult::Page {
                    path: s("/a"),
                    from: 1,
                    text: s("x"),
                    more: false,
                    cut: false,
                    size: 1,
                }),
                Done,
            ),
            (
                SessionResponse::Room(RoomResult::Refused { reason: s("no") }),
                Refused,
            ),
            (
                SessionResponse::Room(RoomResult::Unknown { job: s("j") }),
                Refused,
            ),
            (
                SessionResponse::Room(RoomResult::NoPage { reason: s("none") }),
                Failed,
            ),
            (
                SessionResponse::Room(RoomResult::Lost {
                    job: s("j"),
                    why: s("gone"),
                }),
                Failed,
            ),
            (
                SessionResponse::Room(RoomResult::Failed {
                    reason: s("why"),
                    job: None,
                }),
                Failed,
            ),
        ];
        for (response, ending) in cases {
            assert_eq!(ending_of(&response), ending, "{response:?}");
        }
    }

    /// A run's session finds the store's activity log through its
    /// follow-up; with neither, there's none.
    #[test]
    fn a_runs_activity_goes_to_the_stores_log() {
        let mut config = crate::api_auth::tests::owner_config(None);
        assert_eq!(activity_log(&config), None);
        config.scheduler.followup = Some(crate::FollowupScope {
            root: PathBuf::from("/srv/h/schedules"),
            run: s("t"),
            grants: Vec::new(),
            channel: s("local"),
            depth: 0,
        });
        assert_eq!(
            activity_log(&config),
            Some(PathBuf::from("/srv/h/schedules.activity"))
        );
        if let Some(followup) = config.scheduler.followup.as_mut() {
            followup.root = PathBuf::new();
        }
        assert_eq!(activity_log(&config), None);
        config.scheduler.root = PathBuf::from("/srv/h/s");
        assert_eq!(
            activity_log(&config),
            Some(PathBuf::from("/srv/h/s.activity"))
        );
    }
}
