//! The owner's Ideas page: what the agent suggests it could do for them.
//!
//! A suggestion is the agent's words, all of it: a title, what it would do,
//! why it would help, and where in their life. Nothing runs from one. It
//! waits on the page until the owner starts it (the app fills their chat box
//! with it, for them to read and send) or sets it aside.
//!
//! Only a run that speaks to the owner may suggest: a chat turn, or a run
//! for the Ideas page ([`IDEAS_PAGE`], which only the owner can point a
//! schedule at). A scheduled run working for them elsewhere, a hook's or a
//! subagent can't, so a page or message that misleads one of those can't
//! fill the owner's page. At most [`MAX_WAITING`] wait at once.
//!
//! A suggestion is kept only if the broker can check two things of it.
//! What it says it needs is something this install has: the tool groups
//! it names are ones a chat here could be given. And where it says it
//! comes from is there: one of the owner's open goals, by its id, read
//! under the run's own grant to read them. The broker checks what the
//! suggestion says, not what doing it would turn out to take. A run
//! puts at most [`MAX_PER_RUN`] on the page, however it asks.
//!
//! A suggestion that waits doesn't wait for ever. One still waiting
//! [`STALE_SECS`] after it was suggested, or last brought back, is
//! stale; one whose goal has been closed or removed has lost its
//! source. Both are the broker's findings, made when the owner's app
//! lists the page (staleness also when a run suggests), kept on the
//! page with why, and no longer counted as waiting. The agent has no
//! way to retire, set aside or remove a suggestion. The owner brings a
//! stale one back; one whose goal is closed stays as it is.
//!
//! A suggestion the owner started is answered when the run in the
//! conversation they started for it answers them, with steps to spare
//! ([`conversation_answered`]). Which conversation that is, the owner's
//! app says as it starts one; the hub keeps it in memory alone, so
//! after a restart a started suggestion stays started. The broker
//! knows an answer was shown, not that the idea was carried out.
//!
//! The page is listed as it is kept, the newest first. Every idea kept
//! now has a source and goes on top, so those with a source come before
//! any kept before sources were, with nothing to sort.
//!
//! The page is one file in the broker's directory of the owner's records
//! (`<scheduler dir>.owner/ideas`), `0600`, rewritten whole through a
//! sibling `.next` file: the newest [`MAX_KEPT`].

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::run_tools::ToolGroup;
use hatchling_core::ui::{
    GoalOp, GoalResult, GoalState, Suggestion, SuggestionOp, SuggestionResult, SuggestionSource,
    SuggestionState,
};
use hatchling_core::{SuggestOp, SuggestResult, SuggestedFrom};
use hatchling_goals::{FeedBody, FeedQuery, GoalId, StatusFilter};
use hatchling_memory::MemoryPrincipal;

pub use hatchling_core::ui::IDEAS_PAGE;

use crate::Config;
use crate::config::GoalsAction;

/// How many suggestions the page keeps: the newest.
pub const MAX_KEPT: usize = 100;

/// How many may wait for the owner at once.
pub const MAX_WAITING: usize = 60;

/// How long a suggestion waits before it is stale: fourteen days.
pub const STALE_SECS: i64 = 14 * 24 * 60 * 60;

/// How many one run may put on the page.
pub const MAX_PER_RUN: usize = 3;

/// The longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 80;

/// The longest summary, and the longest reason, in bytes.
pub const MAX_TEXT_BYTES: usize = 400;

/// The largest page file read.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// The prefix of a suggestion's id.
const ID_PREFIX: &str = "idea-";

/// Serializes the broker's writers.
static WRITES: Mutex<()> = Mutex::new(());

/// What an Ideas page run is told: suggest ideas grounded in what the
/// agent can read about the owner.
#[must_use]
pub fn task(prompt: &str) -> String {
    format!(
        "Suggest ideas for the owner's Ideas page by calling idea_suggest, once per idea. Only ideas given to idea_suggest reach the page: anything you write in your answer is thrown away, unseen. They asked for this:\n\n{prompt}\n\nFirst read their open goals with your tools. Then call idea_suggest up to three times, one idea each: things Hatchling could do for them with the tools and settings it has here, that they haven't asked for. Every idea comes from one of their open goals: give that goal's id as from, exactly as it was shown to you, and say in needs which of your tools doing it would use. An idea with no open goal behind it, or that needs what this install doesn't have, isn't kept. If they have no open goals, or none of them suggests anything, call nothing: no ideas is a good answer. Say why in their terms, and don't invent facts about them. After the calls, answer in one short sentence."
    )
}

/// What this install has for an agent to work with, whoever asks: the
/// tool groups a chat here could be given. `room` says whether there is
/// a Room to offer.
///
/// It is what an idea's needs are held to. It is read from the
/// install's settings, as a run's session has them, and from no run's
/// own grants: the owner starts an idea in a chat of their own.
#[must_use]
pub(crate) fn here(config: &Config, browser: bool, room: bool) -> BTreeSet<ToolGroup> {
    let kept = config
        .scheduler
        .followup
        .as_ref()
        .is_some_and(|run| !run.root.as_os_str().is_empty());
    [
        (ToolGroup::Web, config.web.enabled),
        (ToolGroup::Search, config.web.search),
        (ToolGroup::Browser, browser),
        (ToolGroup::Skills, config.skills.is_enabled()),
        (ToolGroup::Goals, !config.goals.root.as_os_str().is_empty()),
        (
            ToolGroup::Memory,
            !config.memory.root.as_os_str().is_empty(),
        ),
        (ToolGroup::Schedule, kept || config.scheduler.enabled()),
        (ToolGroup::Suggest, kept || config.scheduler.enabled()),
        (ToolGroup::Room, room),
    ]
    .into_iter()
    .filter_map(|(group, on)| on.then_some(group))
    .collect()
}

/// What an install without `group` is said to lack, to the agent.
const fn lacking(group: ToolGroup) -> &'static str {
    match group {
        ToolGroup::Web => "no way to read web pages",
        ToolGroup::Search => "no web search",
        ToolGroup::Browser => "no browser",
        ToolGroup::Skills => "no skills",
        ToolGroup::Goals => "no goals list",
        ToolGroup::Memory => "no memory",
        ToolGroup::Schedule => "no scheduled work",
        ToolGroup::Suggest => "no Ideas page",
        ToolGroup::Room => "no computer of its own",
    }
}

/// One of the owner's goals, as a suggestion's source is looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GoalFound {
    /// It is there and open, with this title.
    Open(String),
    /// It is there, and closed.
    Closed,
    /// The owner has no such goal.
    Absent,
    /// The run asking may not read the owner's goals.
    Hidden,
    /// The goals couldn't be read.
    Unread,
}

/// The owner's goal `id`, as a run under `config` may see it: read only
/// under the run's own grant to read their goals.
pub(crate) fn goal_found(config: &Config, principal: &MemoryPrincipal, id: &str) -> GoalFound {
    if !config.goals.grants.contains(&GoalsAction::QueryGoals) {
        return GoalFound::Hidden;
    }
    let Ok(id) = id.parse::<GoalId>() else {
        return GoalFound::Absent;
    };
    let every = FeedQuery::Goals {
        status: StatusFilter::Any,
        updated_since_secs: None,
        after: None,
    };
    let Some(goals) = crate::goals_ops::read_feed(&config.goals, &config.memory, principal, &every)
    else {
        return GoalFound::Unread;
    };
    goals
        .iter()
        .find_map(|item| match item.body() {
            FeedBody::Goal(goal) if goal.id == id => Some(if goal.is_open() {
                GoalFound::Open(goal.title.clone())
            } else {
                GoalFound::Closed
            }),
            FeedBody::Goal(_) | FeedBody::Tracking(_) | FeedBody::Memory(_) => None,
        })
        .unwrap_or(GoalFound::Absent)
}

/// What a suggestion is checked against, as the broker finds it.
pub(crate) struct Ground<'a> {
    /// The run suggesting, as the owner's Activity names it.
    pub run: &'a str,
    /// What this install has ([`here`]).
    pub here: &'a BTreeSet<ToolGroup>,
    /// Looks up one of the owner's goals by its id ([`goal_found`]).
    pub goal: &'a dyn Fn(&str) -> GoalFound,
    /// The time now.
    pub now: hatchling_scheduler::Timestamp,
}

/// Retires what waits on `page` and shouldn't: a suggestion whose goal
/// isn't among the owner's `open` goals has lost its source, and one
/// that has waited more than [`STALE_SECS`] at `now` is stale. With no
/// `open` goals known (a run can't ask, or they couldn't be read) no
/// source is judged. Says whether anything changed.
fn retire(
    page: &mut [Suggestion],
    now: hatchling_scheduler::Timestamp,
    open: Option<&BTreeSet<String>>,
) -> bool {
    let mut changed = false;
    for kept in page
        .iter_mut()
        .filter(|kept| kept.state == SuggestionState::Suggested)
    {
        let gone = open.is_some_and(|open| {
            kept.source.as_ref().is_some_and(|source| {
                let SuggestionSource::Goal { id, .. } = source;
                !open.contains(id)
            })
        });
        let stale = kept
            .suggested_at
            .parse::<hatchling_scheduler::Timestamp>()
            .is_ok_and(|since| now.as_second().saturating_sub(since.as_second()) > STALE_SECS);
        if gone {
            kept.state = SuggestionState::SourceGone;
        } else if stale {
            kept.state = SuggestionState::Stale;
        } else {
            continue;
        }
        changed = true;
    }
    changed
}

/// The ids of the owner's open goals, as their own goals page reads
/// them; none when the broker keeps no goals or couldn't read them.
fn open_goals(config: &Config) -> Option<BTreeSet<String>> {
    match crate::goal_ops::handle_goal_op(config, &GoalOp::List) {
        GoalResult::Listed { goals } => Some(
            goals
                .into_iter()
                .filter(|goal| goal.state == GoalState::Open)
                .map(|goal| goal.id)
                .collect(),
        ),
        GoalResult::Saved { .. }
        | GoalResult::NotFound
        | GoalResult::AlreadyClosed
        | GoalResult::Rejected
        | GoalResult::LimitReached
        | GoalResult::Off
        | GoalResult::Failed => None,
    }
}

/// Puts `op` on the owner's Ideas page, if the session `config` serves
/// speaks to the owner, the install has what the idea needs, and the
/// goal it comes from is one the run `run` may read and is open.
///
/// A run's own config is scoped: it doesn't name the schedule store. The
/// run's follow-up scope does, as the broker set it when it started the
/// run, so the page is found from there.
#[must_use]
pub fn suggest(
    config: &Config,
    principal: &MemoryPrincipal,
    run: &str,
    op: &SuggestOp,
) -> SuggestResult {
    let here = here(
        config,
        crate::browser_ops::configured().is_some(),
        crate::room_ops::open(),
    );
    let ground = Ground {
        run,
        here: &here,
        goal: &|id| goal_found(config, principal, id),
        now: hatchling_scheduler::Timestamp::now(),
    };
    let result = suggest_from(config, &ground, op);
    match &result {
        SuggestResult::Suggested => tracing::info!("an idea was suggested for the Ideas page"),
        SuggestResult::Unasked { reason } => {
            tracing::info!(%reason, "an idea wasn't kept on the Ideas page");
        }
    }
    result
}

/// Why an idea that says it needs `needs` isn't kept on an install that
/// has `here`; none when it has all of them, each named once.
fn unmet(needs: &[ToolGroup], here: &BTreeSet<ToolGroup>) -> Option<String> {
    let named: BTreeSet<ToolGroup> = needs.iter().copied().collect();
    if named.len() != needs.len() {
        return Some(String::from("Name each thing the idea needs once."));
    }
    let lacks: Vec<&str> = named
        .iter()
        .filter(|group| !here.contains(group))
        .map(|group| lacking(*group))
        .collect();
    (!lacks.is_empty()).then(|| {
        format!(
            "Hatchling can't do that on this install: it has {}. Suggest only what it can do with what it has here.",
            lacks.join(" and ")
        )
    })
}

/// The source of an idea that says it comes from `from`, as `ground`
/// finds it; or why it isn't kept.
fn sourced(from: &SuggestedFrom, ground: &Ground<'_>) -> Result<SuggestionSource, &'static str> {
    let SuggestedFrom::Goal(id) = from;
    match (ground.goal)(id) {
        GoalFound::Open(title) => Ok(SuggestionSource::Goal {
            id: id.clone(),
            title,
        }),
        GoalFound::Closed => {
            Err("That goal is closed: an idea comes from one of the owner's open goals.")
        }
        GoalFound::Absent => Err(
            "The owner has no goal with that id. Read their goals, and give one's id exactly as it is shown.",
        ),
        GoalFound::Hidden => {
            Err("This run may not read the owner's goals, so it can't suggest an idea from one.")
        }
        GoalFound::Unread => Err("Hatchling couldn't read the owner's goals; its log says why."),
    }
}

fn suggest_from(config: &Config, ground: &Ground<'_>, op: &SuggestOp) -> SuggestResult {
    let unasked = |reason: &str| SuggestResult::Unasked {
        reason: reason.to_owned(),
    };
    let Some(run) = config
        .scheduler
        .followup
        .as_ref()
        .filter(|run| run.channel == "local" || run.channel == IDEAS_PAGE)
    else {
        return unasked("Only a chat, or a run for the Ideas page, may suggest ideas.");
    };
    if run.root.as_os_str().is_empty() {
        return unasked("Hatchling keeps no Ideas page here: scheduled work is off.");
    }
    let SuggestOp::Suggest {
        title,
        summary,
        why,
        area,
        needs,
        from,
    } = op;
    let (title, summary, why) = (title.trim(), summary.trim(), why.trim());
    if title.is_empty() || summary.is_empty() || why.is_empty() {
        return unasked("A suggestion needs a title, what it would do, and why.");
    }
    if title.chars().count() > MAX_TITLE_CHARS
        || summary.len() > MAX_TEXT_BYTES
        || why.len() > MAX_TEXT_BYTES
    {
        return unasked(
            "Say it in fewer words: a title of 80 characters, and 400 bytes each for what and why.",
        );
    }
    if let Some(reason) = unmet(needs, ground.here) {
        return unasked(&reason);
    }
    let source = match sourced(from, ground) {
        Ok(source) => source,
        Err(reason) => return unasked(reason),
    };
    let Ok(suggestion_id) = crate::approval::random_hex(ID_PREFIX) else {
        tracing::error!("suggestion id entropy unavailable");
        return unasked("Hatchling couldn't keep it; its log says why.");
    };
    // Where `SchedulerConfig::ideas_log` puts it, for the broker's store.
    let path = crate::config::owner_dir_of(&run.root).join("ideas");
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut page = match read(&path) {
        Ok(page) => page,
        Err(error) => {
            tracing::warn!(%error, "the Ideas page couldn't be read");
            return unasked("Hatchling couldn't keep it; its log says why.");
        }
    };
    // What has waited too long no longer waits: a run can't ask after the
    // owner's goals, so no source is judged here.
    let _ = retire(&mut page, ground.now, None);
    let waiting = || {
        page.iter()
            .filter(|kept| kept.state == SuggestionState::Suggested)
    };
    if waiting().any(|kept| kept.title.to_lowercase() == title.to_lowercase()) {
        return unasked("It's already on the Ideas page.");
    }
    if waiting().count() >= MAX_WAITING {
        return unasked("The Ideas page is full of ideas the owner hasn't looked at yet.");
    }
    // Whatever became of them since: three is what one run may add.
    if page.iter().filter(|kept| kept.run == ground.run).count() >= MAX_PER_RUN {
        return unasked("This run has put three ideas on the page: that is all one run may.");
    }
    page.insert(
        0,
        Suggestion {
            suggestion_id,
            title: title.to_owned(),
            summary: summary.to_owned(),
            why: why.to_owned(),
            area: *area,
            suggested_at: ground.now.to_string(),
            state: SuggestionState::Suggested,
            needs: needs.clone(),
            source: Some(source),
            run: ground.run.to_owned(),
        },
    );
    trim(&mut page);
    match write(&path, &page) {
        Ok(()) => SuggestResult::Suggested,
        Err(error) => {
            tracing::warn!(%error, "the Ideas page couldn't be written");
            unasked("Hatchling couldn't keep it; its log says why.")
        }
    }
}

/// Keeps the page to [`MAX_KEPT`]: what the owner set aside or started
/// goes before what waits for them, the oldest first.
fn trim(page: &mut Vec<Suggestion>) {
    while page.len() > MAX_KEPT {
        let oldest_done = page
            .iter()
            .rposition(|kept| kept.state != SuggestionState::Suggested);
        let index = oldest_done.unwrap_or(page.len() - 1);
        page.remove(index);
    }
}

/// Does `op`, the owner's, on their Ideas page, retiring first what
/// waits there and shouldn't.
#[must_use]
pub fn handle(config: &Config, op: &SuggestionOp) -> SuggestionResult {
    handle_at(config, op, hatchling_scheduler::Timestamp::now(), &|| {
        open_goals(config)
    })
}

/// [`handle`] at `now`, with the owner's open goals as `open` reads
/// them: asked only when the page is listed, or a suggestion whose
/// source was gone is brought back.
pub(crate) fn handle_at(
    config: &Config,
    op: &SuggestionOp,
    now: hatchling_scheduler::Timestamp,
    open: &dyn Fn() -> Option<BTreeSet<String>>,
) -> SuggestionResult {
    if !config.scheduler.enabled() {
        return SuggestionResult::Off;
    }
    let path = config.scheduler.ideas_log();
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut page = match read(&path) {
        Ok(page) => page,
        Err(error) => {
            tracing::warn!(%error, "the Ideas page couldn't be read");
            return SuggestionResult::Failed;
        }
    };
    let (id, state) = match op {
        SuggestionOp::List => {
            // A page that can't be written back is still shown as it
            // stands: the next reading finds the same.
            if retire(&mut page, now, open().as_ref()) {
                if let Err(error) = write(&path, &page) {
                    tracing::warn!(%error, "the Ideas page couldn't be written");
                }
            }
            return SuggestionResult::Listed { suggestions: page };
        }
        SuggestionOp::Start { id } => (id, SuggestionState::Started),
        SuggestionOp::Dismiss { id } => (id, SuggestionState::Dismissed),
        SuggestionOp::Restore { id } => (id, SuggestionState::Suggested),
    };
    let Some(kept) = page.iter_mut().find(|kept| &kept.suggestion_id == id) else {
        return SuggestionResult::NotFound;
    };
    if state == SuggestionState::Suggested {
        // Brought back, it waits anew; but not while what it came from
        // is still gone: it is answered as it stands.
        if kept.state == SuggestionState::SourceGone && still_gone(kept, open().as_ref()) {
            return SuggestionResult::Changed {
                suggestion: kept.clone(),
            };
        }
        kept.suggested_at = now.to_string();
    }
    kept.state = state;
    let suggestion = kept.clone();
    match write(&path, &page) {
        Ok(()) => SuggestionResult::Changed { suggestion },
        Err(error) => {
            tracing::warn!(%error, "the Ideas page couldn't be written");
            SuggestionResult::Failed
        }
    }
}

/// Whether the owner has started the suggestion `id` and it stands as
/// they left it: what a new conversation may be started for.
#[must_use]
pub(crate) fn is_started(config: &Config, id: &str) -> bool {
    if !config.scheduler.enabled() {
        return false;
    }
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    read(&config.scheduler.ideas_log()).is_ok_and(|page| {
        page.iter()
            .any(|kept| kept.suggestion_id == id && kept.state == SuggestionState::Started)
    })
}

/// A run answered the owner in the local chat's `conversation`, was
/// shown, and had steps to spare: the suggestion that conversation was
/// started for, if it was for one and the owner left it started, is
/// answered. Says whether one was.
pub(crate) fn conversation_answered(
    config: &Config,
    hub: &hatchling_channel::LocalHub,
    conversation: u64,
) -> bool {
    let Some(id) = hub.started_for(conversation) else {
        return false;
    };
    if !config.scheduler.enabled() {
        return false;
    }
    let path = config.scheduler.ideas_log();
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    let Ok(mut page) = read(&path) else {
        return false;
    };
    let Some(kept) = page
        .iter_mut()
        .find(|kept| kept.suggestion_id == id && kept.state == SuggestionState::Started)
    else {
        return false;
    };
    kept.state = SuggestionState::Answered;
    match write(&path, &page) {
        Ok(()) => {
            tracing::info!("an idea the owner started was answered");
            true
        }
        Err(error) => {
            tracing::warn!(%error, "the Ideas page couldn't be written");
            false
        }
    }
}

/// Whether `kept`'s source is still gone, with the owner's open goals
/// as `open`: it is unless its goal is among them. A suggestion with no
/// source has none to be gone.
fn still_gone(kept: &Suggestion, open: Option<&BTreeSet<String>>) -> bool {
    kept.source.as_ref().is_some_and(|source| {
        let SuggestionSource::Goal { id, .. } = source;
        !open.is_some_and(|open| open.contains(id))
    })
}

/// The page at `path`: none when there's no file yet, or it's empty.
fn read(path: &Path) -> std::io::Result<Vec<Suggestion>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    if len > MAX_FILE_BYTES {
        return Err(std::io::Error::other("the Ideas page is too large to read"));
    }
    // The install makes the file empty, for the broker's user to write.
    if len == 0 {
        return Ok(Vec::new());
    }
    serde_json::from_reader(std::io::BufReader::new(file)).map_err(std::io::Error::other)
}

/// Writes `page` at `path`, whole, through a sibling `.next` file.
fn write(path: &Path, page: &[Suggestion]) -> std::io::Result<()> {
    crate::kept_log::ensure_parent(path)?;
    let next = crate::kept_log::next_path(path);
    let text = serde_json::to_vec(page)?;
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&next)?;
    file.write_all(&text)?;
    file.sync_all()?;
    std::fs::rename(&next, path)
}

#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "crate-visible on purpose: the chat's and the API's tests put ideas on a page"
)]
pub(crate) mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::config::FollowupScope;
    use hatchling_core::SuggestionArea;

    pub(crate) fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-ideas-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A config whose scheduler keeps its store in `dir`, for a run
    /// delivering to `channel`, or for no run. A run's own config doesn't
    /// name the store, as [`crate::run_launcher::scoped_config`] makes it:
    /// only its follow-up scope does.
    pub(crate) fn config(dir: &Path, channel: Option<&str>) -> Config {
        let mut config =
            crate::api_auth::tests::owner_config(Some(&dir.join("schedules").to_string_lossy()));
        config.scheduler.followup = channel.map(|channel| FollowupScope {
            root: config.scheduler.root.clone(),
            run: String::from("run-1"),
            grants: Vec::new(),
            channel: channel.to_owned(),
            depth: 0,
        });
        config
    }

    fn idea(title: &str) -> SuggestOp {
        idea_of(title, &[], "g-open")
    }

    /// An idea that says it needs `needs` and comes from the goal `goal`.
    fn idea_of(title: &str, needs: &[ToolGroup], goal: &str) -> SuggestOp {
        SuggestOp::Suggest {
            title: title.to_owned(),
            summary: String::from("A week of short runs, on your calendar."),
            why: String::from("You set a goal to run 5k."),
            area: SuggestionArea::Health,
            needs: needs.to_vec(),
            from: SuggestedFrom::Goal(goal.to_owned()),
        }
    }

    /// An install that has every tool group.
    fn everything() -> BTreeSet<ToolGroup> {
        ToolGroup::ALL.into_iter().collect()
    }

    /// The owner's goals, as these tests have them.
    fn goals(id: &str) -> GoalFound {
        match id {
            "g-open" => GoalFound::Open(String::from("Run 5k by June")),
            "g-closed" => GoalFound::Closed,
            "g-hidden" => GoalFound::Hidden,
            "g-unread" => GoalFound::Unread,
            _ => GoalFound::Absent,
        }
    }

    /// A suggestion by the run `run`, on an install that has `here`.
    fn suggest_as(
        config: &Config,
        run: &str,
        here: &BTreeSet<ToolGroup>,
        op: &SuggestOp,
    ) -> SuggestResult {
        suggest_at(config, run, here, hatchling_scheduler::Timestamp::now(), op)
    }

    /// A suggestion by the run `run`, at `now`.
    fn suggest_at(
        config: &Config,
        run: &str,
        here: &BTreeSet<ToolGroup>,
        now: hatchling_scheduler::Timestamp,
        op: &SuggestOp,
    ) -> SuggestResult {
        let ground = Ground {
            run,
            here,
            goal: &goals,
            now,
        };
        suggest_from(config, &ground, op)
    }

    /// A suggestion by a run of its own, on an install with everything:
    /// what the tests of the page itself ask of it.
    fn suggest(config: &Config, op: &SuggestOp) -> SuggestResult {
        use std::sync::atomic::{AtomicU32, Ordering};
        static RUNS: AtomicU32 = AtomicU32::new(0);
        let run = format!("run-{}", RUNS.fetch_add(1, Ordering::Relaxed));
        suggest_as(config, &run, &everything(), op)
    }

    fn reason(result: &SuggestResult) -> &str {
        if let SuggestResult::Unasked { reason } = result {
            reason
        } else {
            ""
        }
    }

    fn listed(config: &Config) -> Vec<Suggestion> {
        if let SuggestionResult::Listed { suggestions } = handle(config, &SuggestionOp::List) {
            suggestions
        } else {
            Vec::new()
        }
    }

    /// An idea put on the page kept under `dir`, where it waits: its id.
    pub(crate) fn waiting(dir: &Path, title: &str) -> String {
        let chat = config(dir, Some("local"));
        assert_eq!(suggest(&chat, &idea(title)), SuggestResult::Suggested);
        listed(&chat)
            .iter()
            .find(|kept| kept.title == title)
            .map(|kept| kept.suggestion_id.clone())
            .unwrap_or_default()
    }

    /// An idea on the page kept under `dir` that the owner started: its
    /// id.
    pub(crate) fn started(dir: &Path, title: &str) -> String {
        let id = waiting(dir, title);
        let _ = handle(&config(dir, None), &SuggestionOp::Start { id: id.clone() });
        id
    }

    #[test]
    fn a_chat_or_an_ideas_run_suggests_and_the_owner_reads_it_newest_first() {
        let dir = scratch("kept");
        let chat = config(&dir, Some("local"));
        assert!(listed(&chat).is_empty(), "no page yet: none");
        assert_eq!(
            suggest(&chat, &idea("  A run plan  ")),
            SuggestResult::Suggested
        );
        // A run's config names no store but in its follow-up scope, as the
        // run launcher scopes it: the page is found from there.
        let mut run = config(&dir, Some(IDEAS_PAGE));
        run.scheduler.root = std::path::PathBuf::new();
        assert_eq!(
            suggest(&run, &idea("A reading list")),
            SuggestResult::Suggested
        );
        let page = listed(&chat);
        assert_eq!(
            page.iter()
                .map(|kept| kept.title.as_str())
                .collect::<Vec<_>>(),
            ["A reading list", "A run plan"],
            "trimmed, the newest first"
        );
        let first = page.last().unwrap();
        assert!(first.suggestion_id.starts_with(ID_PREFIX));
        assert_eq!(first.state, SuggestionState::Suggested);
        assert_eq!(first.area, SuggestionArea::Health);
        assert_eq!(first.why, "You set a goal to run 5k.");
        assert!(
            first
                .suggested_at
                .parse::<hatchling_scheduler::Timestamp>()
                .is_ok()
        );
        let path = chat.scheduler.ideas_log();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!crate::kept_log::next_path(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A run that doesn't speak to the owner can't fill their page: a
    /// scheduled run for the chat's feed page, a hook's, or no run at all.
    #[test]
    fn only_a_run_that_speaks_to_the_owner_may_suggest() {
        let dir = scratch("speaks");
        for channel in [None, Some(crate::feed_page::FEED_PAGE), Some("signal")] {
            let result = suggest(&config(&dir, channel), &idea("A run plan"));
            assert!(
                reason(&result).starts_with("Only a chat"),
                "{channel:?}: {result:?}"
            );
        }
        assert!(listed(&config(&dir, None)).is_empty());
        let mut off = config(&dir, Some("local"));
        off.scheduler.root = std::path::PathBuf::new();
        assert_eq!(handle(&off, &SuggestionOp::List), SuggestionResult::Off);
        if let Some(run) = off.scheduler.followup.as_mut() {
            run.root = std::path::PathBuf::new();
        }
        assert!(reason(&suggest(&off, &idea("x"))).contains("scheduled work is off"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_long_or_repeated_suggestion_is_told_why() {
        let dir = scratch("refused");
        let chat = config(&dir, Some("local"));
        for (title, summary, why) in [("", "s", "w"), ("t", " ", "w"), ("t", "s", "")] {
            let op = SuggestOp::Suggest {
                title: title.to_owned(),
                summary: summary.to_owned(),
                why: why.to_owned(),
                area: SuggestionArea::Career,
                needs: Vec::new(),
                from: SuggestedFrom::Goal(String::from("g-open")),
            };
            assert!(reason(&suggest(&chat, &op)).contains("needs a title"));
        }
        let long = |title: String, summary: String, why: String| SuggestOp::Suggest {
            title,
            summary,
            why,
            area: SuggestionArea::Career,
            needs: Vec::new(),
            from: SuggestedFrom::Goal(String::from("g-open")),
        };
        let fits = |n| "x".repeat(n);
        assert_eq!(
            suggest(
                &chat,
                &long(
                    "é".repeat(MAX_TITLE_CHARS),
                    fits(MAX_TEXT_BYTES),
                    fits(MAX_TEXT_BYTES)
                )
            ),
            SuggestResult::Suggested,
            "at the bounds: kept"
        );
        for op in [
            long("t".repeat(MAX_TITLE_CHARS + 1), fits(1), fits(1)),
            long(fits(1), fits(MAX_TEXT_BYTES + 1), fits(1)),
            long(fits(1), fits(1), fits(MAX_TEXT_BYTES + 1)),
        ] {
            assert!(reason(&suggest(&chat, &op)).contains("fewer words"));
        }
        assert_eq!(
            suggest(&chat, &idea("A run plan")),
            SuggestResult::Suggested
        );
        assert!(reason(&suggest(&chat, &idea("a RUN plan"))).contains("already"));
        // Set aside, it may be suggested again.
        let id = listed(&chat).first().unwrap().suggestion_id.clone();
        assert!(matches!(
            handle(&chat, &SuggestionOp::Dismiss { id }),
            SuggestionResult::Changed { .. }
        ));
        assert_eq!(
            suggest(&chat, &idea("A run plan")),
            SuggestResult::Suggested
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_owner_starts_sets_aside_and_brings_back_a_suggestion() {
        let dir = scratch("owner");
        let chat = config(&dir, Some("local"));
        assert_eq!(
            suggest(&chat, &idea("A run plan")),
            SuggestResult::Suggested
        );
        let id = listed(&chat).first().unwrap().suggestion_id.clone();
        for (op, state) in [
            (
                SuggestionOp::Start { id: id.clone() },
                SuggestionState::Started,
            ),
            (
                SuggestionOp::Dismiss { id: id.clone() },
                SuggestionState::Dismissed,
            ),
            (
                SuggestionOp::Restore { id: id.clone() },
                SuggestionState::Suggested,
            ),
        ] {
            let SuggestionResult::Changed { suggestion } = handle(&chat, &op) else {
                return;
            };
            assert_eq!(
                (suggestion.suggestion_id.as_str(), suggestion.state),
                (id.as_str(), state)
            );
            assert_eq!(listed(&chat).first().unwrap().state, state, "kept");
        }
        assert_eq!(
            handle(
                &chat,
                &SuggestionOp::Start {
                    id: String::from("idea-none")
                }
            ),
            SuggestionResult::NotFound
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v29: a suggestion the owner started is answered when the
    /// conversation they started for it is, while they leave it
    /// started, and only a started one may have a conversation.
    #[test]
    fn a_started_suggestion_is_answered_with_the_conversation_started_for_it() {
        let dir = scratch("answered");
        let chat = config(&dir, Some("local"));
        let hub = hatchling_channel::LocalHub::new(hatchling_channel::OutputGate::disabled());
        let plan = waiting(&dir, "A run plan");
        let list = waiting(&dir, "A reading list");
        let trip = waiting(&dir, "A trip");
        let state = |id: &str| {
            listed(&chat)
                .iter()
                .find(|kept| kept.suggestion_id == id)
                .map(|kept| kept.state)
        };
        let act = |op: SuggestionOp| {
            let _ = handle(&chat, &op);
        };
        // Only a started suggestion may have a conversation of its own.
        assert!(!is_started(&chat, &plan), "it waits");
        assert!(!is_started(&chat, "idea-none"), "not on the page");
        act(SuggestionOp::Start { id: plan.clone() });
        assert!(is_started(&chat, &plan));
        assert!(!is_started(&chat, &list), "one started isn't another");
        // No conversation was started for it yet.
        assert!(!conversation_answered(&chat, &hub, 0));
        // Another the owner started, later, isn't the one this
        // conversation is for.
        act(SuggestionOp::Start { id: trip.clone() });
        hub.new_conversation_for(plan.clone());
        assert!(
            !conversation_answered(&chat, &hub, 0),
            "the conversation before it"
        );
        assert_eq!(state(&plan), Some(SuggestionState::Started));
        assert!(conversation_answered(&chat, &hub, 1));
        assert_eq!(
            (state(&plan), state(&list)),
            (
                Some(SuggestionState::Answered),
                Some(SuggestionState::Suggested)
            )
        );
        assert_eq!(state(&trip), Some(SuggestionState::Started));
        assert!(!is_started(&chat, &plan), "answered, it isn't started");
        assert!(!conversation_answered(&chat, &hub, 1), "answered once");
        // One the owner didn't start isn't answered, nor one they set
        // aside while its run worked.
        hub.new_conversation_for(list.clone());
        assert!(!conversation_answered(&chat, &hub, 2));
        assert_eq!(state(&list), Some(SuggestionState::Suggested));
        act(SuggestionOp::Start { id: list.clone() });
        act(SuggestionOp::Dismiss { id: list.clone() });
        assert!(!conversation_answered(&chat, &hub, 2));
        assert_eq!(state(&list), Some(SuggestionState::Dismissed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v29: an answered suggestion is still the owner's to start again,
    /// set aside or bring back. And where there is no page to ask, or
    /// one that can't be read, nothing is started and nothing answered.
    #[test]
    fn an_answered_suggestion_is_the_owners_and_no_page_answers_nothing() {
        let dir = scratch("answered-owner");
        let chat = config(&dir, Some("local"));
        let hub = hatchling_channel::LocalHub::new(hatchling_channel::OutputGate::disabled());
        let plan = started(&dir, "A run plan");
        let state = |id: &str| {
            listed(&chat)
                .iter()
                .find(|kept| kept.suggestion_id == id)
                .map(|kept| kept.state)
        };
        let act = |op: SuggestionOp| {
            let _ = handle(&chat, &op);
        };
        hub.new_conversation_for(plan.clone());
        assert!(conversation_answered(&chat, &hub, 1));
        assert_eq!(state(&plan), Some(SuggestionState::Answered));
        for (op, then) in [
            (
                SuggestionOp::Start { id: plan.clone() },
                SuggestionState::Started,
            ),
            (
                SuggestionOp::Dismiss { id: plan.clone() },
                SuggestionState::Dismissed,
            ),
            (
                SuggestionOp::Restore { id: plan.clone() },
                SuggestionState::Suggested,
            ),
            (
                SuggestionOp::Start { id: plan.clone() },
                SuggestionState::Started,
            ),
        ] {
            act(op);
            assert_eq!(state(&plan), Some(then));
        }
        hub.new_conversation_for(plan.clone());
        // With scheduled work off there is no page to ask.
        let mut off = config(&dir, Some("local"));
        off.scheduler.root = std::path::PathBuf::new();
        assert!(!is_started(&off, &plan));
        assert!(!conversation_answered(&off, &hub, 2));
        // A page that can't be read answers nothing, and is left alone.
        let path = chat.scheduler.ideas_log();
        let kept = std::fs::read(&path).unwrap();
        std::fs::write(&path, "not a page").unwrap();
        assert!(!is_started(&chat, &plan));
        assert!(!conversation_answered(&chat, &hub, 2));
        assert_eq!(std::fs::read(&path).unwrap(), b"not a page");
        std::fs::write(&path, kept).unwrap();
        assert!(conversation_answered(&chat, &hub, 2));
        assert_eq!(state(&plan), Some(SuggestionState::Answered));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_page_holds_so_many_waiting_and_keeps_the_newest() {
        let dir = scratch("bounded");
        let chat = config(&dir, Some("local"));
        for n in 0..MAX_WAITING {
            assert_eq!(
                suggest(&chat, &idea(&format!("Idea {n}"))),
                SuggestResult::Suggested
            );
        }
        assert!(reason(&suggest(&chat, &idea("One more"))).contains("full"));
        // Each set aside makes room; past the bound, what was set aside
        // goes first, the oldest first.
        for round in 0..MAX_KEPT {
            let id = listed(&chat)
                .iter()
                .find(|kept| kept.state == SuggestionState::Suggested)
                .unwrap()
                .suggestion_id
                .clone();
            let _ = handle(&chat, &SuggestionOp::Dismiss { id });
            assert_eq!(
                suggest(&chat, &idea(&format!("More {round}"))),
                SuggestResult::Suggested
            );
        }
        let page = listed(&chat);
        assert_eq!(page.len(), MAX_KEPT);
        assert_eq!(
            page.iter()
                .filter(|kept| kept.state == SuggestionState::Suggested)
                .count(),
            MAX_WAITING,
            "nothing waiting was dropped"
        );
        assert_eq!(
            page.first().map(|kept| kept.title.as_str()),
            Some(format!("More {}", MAX_KEPT - 1).as_str())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_page_that_cant_be_read_or_written_keeps_nothing() {
        let dir = scratch("broken");
        let chat = config(&dir, Some("local"));
        let path = chat.scheduler.ideas_log();
        // The install makes it empty: an empty page, which takes ideas.
        crate::kept_log::ensure_parent(&path).unwrap();
        std::fs::write(&path, "").unwrap();
        assert!(listed(&chat).is_empty());
        assert_eq!(
            handle(&chat, &SuggestionOp::List),
            SuggestionResult::Listed {
                suggestions: Vec::new()
            }
        );
        assert_eq!(suggest(&chat, &idea("x")), SuggestResult::Suggested);
        std::fs::write(&path, "not a page").unwrap();
        assert!(reason(&suggest(&chat, &idea("x"))).contains("couldn't keep it"));
        assert_eq!(handle(&chat, &SuggestionOp::List), SuggestionResult::Failed);
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert_eq!(handle(&chat, &SuggestionOp::List), SuggestionResult::Failed);
        // A page whose directory can't be made can't be kept, or read.
        std::fs::write(dir.join("missing"), "").unwrap();
        let gone = config(&dir.join("missing"), Some("local"));
        assert!(reason(&suggest(&gone, &idea("x"))).contains("couldn't keep it"));
        assert_eq!(
            handle(
                &gone,
                &SuggestionOp::Start {
                    id: String::from("idea-x")
                }
            ),
            SuggestionResult::Failed
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_task_carries_the_prompt_and_asks_for_grounded_suggestions() {
        let task = task("Things for my running.");
        assert!(task.contains("\n\nThings for my running.\n\n"));
        assert!(task.contains("idea_suggest"));
        assert!(task.contains("up to three"));
        assert!(task.contains("thrown away, unseen"));
        assert!(task.contains("don't invent facts"));
        // Every idea comes from an open goal, and none is a good answer.
        assert!(task.contains("First read their open goals"));
        assert!(task.contains("give that goal's id as from, exactly as it was shown to you"));
        assert!(task.contains("say in needs which of your tools doing it would use"));
        assert!(task.contains("isn't kept"));
        assert!(task.contains("call nothing: no ideas is a good answer"));
        assert!(
            !task.contains("asked you to remember"),
            "memory is no source yet"
        );
    }

    /// An idea is kept only if this install has each thing it says it
    /// needs, named once; the card keeps what it said.
    #[test]
    fn an_idea_is_kept_only_if_this_install_has_what_it_needs() {
        use ToolGroup::{Browser, Goals, Room, Search, Web};
        let dir = scratch("needs");
        let chat = config(&dir, Some("local"));
        let here = BTreeSet::from([Goals, Room]);
        let asked = |title: &str, needs: &[ToolGroup]| {
            suggest_as(&chat, title, &here, &idea_of(title, needs, "g-open"))
        };
        assert_eq!(asked("Needs nothing", &[]), SuggestResult::Suggested);
        assert_eq!(
            asked("Needs both", &[Room, Goals]),
            SuggestResult::Suggested
        );
        assert_eq!(
            reason(&asked("Needs search", &[Search])),
            "Hatchling can't do that on this install: it has no web search. Suggest only what it can do with what it has here."
        );
        // Each thing it lacks is named, in the groups' own order, whatever
        // else the idea needs.
        let lacks = asked("Needs much", &[Room, Browser, Search, Web]);
        assert!(
            reason(&lacks).contains(
                "it has no way to read web pages and no web search and no browser. Suggest"
            ),
            "{lacks:?}"
        );
        assert_eq!(
            reason(&asked("Twice", &[Room, Room])),
            "Name each thing the idea needs once."
        );
        let page = listed(&chat);
        assert_eq!(
            page.iter()
                .map(|kept| (kept.title.as_str(), kept.needs.clone()))
                .collect::<Vec<_>>(),
            [("Needs both", vec![Room, Goals]), ("Needs nothing", vec![])],
            "what wasn't kept isn't there"
        );
        // Every group has its own words for an install without it.
        let named: BTreeSet<&str> = ToolGroup::ALL.into_iter().map(lacking).collect();
        assert_eq!(named.len(), ToolGroup::ALL.len());
        assert!(named.iter().all(|words| words.starts_with("no ")));
        for (group, words) in [
            (Room, "no computer of its own"),
            (Goals, "no goals list"),
            (ToolGroup::Memory, "no memory"),
            (ToolGroup::Skills, "no skills"),
            (ToolGroup::Schedule, "no scheduled work"),
            (ToolGroup::Suggest, "no Ideas page"),
        ] {
            assert_eq!(lacking(group), words);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An idea is kept only if the goal it says it comes from is one the
    /// broker finds, open; the card keeps the goal as the broker found it.
    #[test]
    fn an_idea_is_kept_only_if_the_goal_it_comes_from_is_open() {
        let dir = scratch("from");
        let chat = config(&dir, Some("local"));
        let asked = |title: &str, goal: &str| {
            suggest_as(&chat, title, &everything(), &idea_of(title, &[], goal))
        };
        for (goal, why) in [
            ("g-closed", "That goal is closed"),
            ("g-none", "The owner has no goal with that id"),
            ("", "The owner has no goal with that id"),
            ("g-hidden", "This run may not read the owner's goals"),
            ("g-unread", "couldn't read the owner's goals"),
        ] {
            let result = asked("From it", goal);
            assert!(
                reason(&result).starts_with(why) || reason(&result).contains(why),
                "{goal}: {result:?}"
            );
        }
        assert!(
            listed(&chat).is_empty(),
            "an idea with no goal behind it was kept"
        );
        assert_eq!(asked("From it", "g-open"), SuggestResult::Suggested);
        let page = listed(&chat);
        assert_eq!(page.len(), 1);
        assert_eq!(
            page.first().and_then(|kept| kept.source.clone()),
            Some(SuggestionSource::Goal {
                id: String::from("g-open"),
                title: String::from("Run 5k by June"),
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A run puts three ideas on the page and no more, whatever became of
    /// them; another run has three of its own. Each card keeps its run.
    #[test]
    fn a_run_puts_three_ideas_on_the_page_and_no_more() {
        let dir = scratch("cap");
        let chat = config(&dir, Some("local"));
        let asked = |run: &str, title: &str| suggest_as(&chat, run, &everything(), &idea(title));
        for title in ["One", "Two", "Three"] {
            assert_eq!(asked("run-a", title), SuggestResult::Suggested);
        }
        assert_eq!(
            reason(&asked("run-a", "Four")),
            "This run has put three ideas on the page: that is all one run may."
        );
        // Set aside by the owner, one of them still counts for its run.
        let id = listed(&chat).first().unwrap().suggestion_id.clone();
        assert!(matches!(
            handle(&chat, &SuggestionOp::Dismiss { id }),
            SuggestionResult::Changed { .. }
        ));
        assert!(reason(&asked("run-a", "Four")).contains("three ideas"));
        assert_eq!(asked("run-b", "Four"), SuggestResult::Suggested);
        let page = listed(&chat);
        assert_eq!(
            page.iter()
                .map(|kept| (kept.title.as_str(), kept.run.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Four", "run-b"),
                ("Three", "run-a"),
                ("Two", "run-a"),
                ("One", "run-a")
            ]
        );
        assert_eq!(MAX_PER_RUN, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The owner is shown ideas with a source before those kept before
    /// sources were, the newest first among each: the order the page is
    /// kept in. A page kept before sources were is read, and its ideas
    /// have none.
    #[test]
    fn the_owner_is_shown_ideas_with_a_source_first() {
        let dir = scratch("ranked");
        let chat = config(&dir, Some("local"));
        let path = chat.scheduler.ideas_log();
        crate::kept_log::ensure_parent(&path).unwrap();
        let old = |id: &str, title: &str| {
            format!(
                r#"{{"suggestion_id":"{id}","title":"{title}","summary":"s","why":"w","area":"career","suggested_at":"2026-11-01T08:00:00Z","state":"suggested"}}"#
            )
        };
        std::fs::write(
            &path,
            format!(
                "[{},{}]",
                old("idea-b", "Old, newer"),
                old("idea-a", "Old, older")
            ),
        )
        .unwrap();
        assert_eq!(
            listed(&chat)
                .iter()
                .map(|kept| (
                    kept.title.as_str(),
                    kept.source.is_none(),
                    kept.run.as_str()
                ))
                .collect::<Vec<_>>(),
            [("Old, newer", true, ""), ("Old, older", true, "")]
        );
        assert_eq!(
            suggest(&chat, &idea("New, older")),
            SuggestResult::Suggested
        );
        assert_eq!(
            suggest(&chat, &idea("New, newer")),
            SuggestResult::Suggested
        );
        assert_eq!(
            listed(&chat)
                .iter()
                .map(|kept| (kept.title.as_str(), kept.source.is_some()))
                .collect::<Vec<_>>(),
            [
                ("New, newer", true),
                ("New, older", true),
                ("Old, newer", false),
                ("Old, older", false)
            ]
        );
        // An idea with no source that the owner sets aside and brings
        // back stays where it was, behind the others.
        let _ = handle(
            &chat,
            &SuggestionOp::Dismiss {
                id: String::from("idea-a"),
            },
        );
        let _ = handle(
            &chat,
            &SuggestionOp::Restore {
                id: String::from("idea-a"),
            },
        );
        assert_eq!(
            listed(&chat).last().map(|kept| kept.title.clone()),
            Some(String::from("Old, older"))
        );
        // A run with no name of its own is not every older idea's run.
        assert_eq!(
            suggest_as(&chat, "", &everything(), &idea("By no run")),
            SuggestResult::Suggested
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What this install has is read from its settings, group by group,
    /// and from nothing a run was granted.
    #[test]
    fn what_this_install_has_is_read_from_its_settings() {
        use ToolGroup::{Browser, Goals, Memory, Room, Schedule, Search, Skills, Suggest, Web};
        let dir = scratch("here");
        let mut bare = config(&dir, Some("local"));
        bare.web.enabled = false;
        bare.web.search = false;
        bare.skills.skills_dir = std::path::PathBuf::new();
        bare.goals.root = std::path::PathBuf::new();
        bare.memory.root = std::path::PathBuf::new();
        assert_eq!(
            here(&bare, false, false),
            BTreeSet::from([Schedule, Suggest])
        );
        let with = |change: &dyn Fn(&mut Config), browser: bool, room: bool| {
            let mut config = bare.clone();
            change(&mut config);
            here(&config, browser, room)
                .into_iter()
                .filter(|group| ![Schedule, Suggest].contains(group))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            with(&|config| config.web.enabled = true, false, false),
            [Web]
        );
        assert_eq!(
            with(&|config| config.web.search = true, false, false),
            [Search]
        );
        assert_eq!(with(&|_| (), true, false), [Browser]);
        assert_eq!(with(&|_| (), false, true), [Room]);
        assert_eq!(
            with(
                &|config| config.skills.skills_dir = dir.join("skills"),
                false,
                false
            ),
            [Skills]
        );
        assert_eq!(
            with(
                &|config| config.goals.root = dir.join("goals"),
                false,
                false
            ),
            [Goals]
        );
        assert_eq!(
            with(
                &|config| config.memory.root = dir.join("memory"),
                false,
                false
            ),
            [Memory]
        );
        // A run's own session names the schedule store only in its
        // follow-up scope; with neither there is no scheduled work, and
        // no page.
        let mut run = bare.clone();
        run.scheduler.root = std::path::PathBuf::new();
        assert_eq!(
            here(&run, false, false),
            BTreeSet::from([Schedule, Suggest])
        );
        if let Some(scope) = run.scheduler.followup.as_mut() {
            scope.root = std::path::PathBuf::new();
        }
        assert_eq!(here(&run, false, false), BTreeSet::new());
        run.scheduler.followup = None;
        assert_eq!(here(&run, false, false), BTreeSet::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A goal is found only by a run that may read the owner's goals, by
    /// its own id, as open or closed; and an idea asked for through the
    /// broker's own door is held to the store and to this install.
    #[test]
    fn a_goal_is_found_in_the_owners_store_by_a_run_that_may_read_it() {
        use std::str::FromStr as _;

        use hatchling_goals::{GoalOutcome, GoalsStore};
        use hatchling_memory::SessionId;
        let dir = scratch("store");
        let principal = MemoryPrincipal {
            peer_uid: rustix::process::getuid().as_raw(),
            session_id: SessionId::from_str("sess-ideas").unwrap(),
        };
        // A chat whose goals are kept at `root`, read under `grants`.
        let goals_at = |root: &str, grants: Vec<GoalsAction>| {
            let mut chat = config(&dir, Some("local"));
            chat.goals.root = dir.join(root);
            chat.goals.grants = grants;
            chat
        };
        let chat = goals_at("goals", vec![GoalsAction::QueryGoals]);
        let store = GoalsStore::open(&chat.goals.root).unwrap();
        let open = store
            .create_goal(&principal, "Run 5k by June", "", 10)
            .unwrap();
        let done = store.create_goal(&principal, "Read more", "", 11).unwrap();
        let _ = store
            .close_goal(&principal, &done.id, GoalOutcome::Achieved, 12)
            .unwrap();
        assert_eq!(
            goal_found(&chat, &principal, open.id.as_str()),
            GoalFound::Open(String::from("Run 5k by June"))
        );
        assert_eq!(
            goal_found(&chat, &principal, done.id.as_str()),
            GoalFound::Closed
        );
        assert_eq!(goal_found(&chat, &principal, "0a1b"), GoalFound::Absent);
        assert_eq!(
            goal_found(&chat, &principal, "not an id!"),
            GoalFound::Absent
        );
        // Another person's goals aren't this one's.
        let other = MemoryPrincipal {
            peer_uid: principal.peer_uid + 1,
            session_id: principal.session_id.clone(),
        };
        assert_eq!(
            goal_found(&chat, &other, open.id.as_str()),
            GoalFound::Absent
        );
        // Without the grant nothing is read, there or not.
        let blind = goals_at("goals", Vec::new());
        assert_eq!(
            goal_found(&blind, &principal, open.id.as_str()),
            GoalFound::Hidden
        );
        assert_eq!(goal_found(&blind, &principal, "0a1b"), GoalFound::Hidden);
        // A store that can't be read is said as that.
        let broken = goals_at("a-file", vec![GoalsAction::QueryGoals]);
        std::fs::write(&broken.goals.root, "").unwrap();
        assert_eq!(
            goal_found(&broken, &principal, open.id.as_str()),
            GoalFound::Unread
        );
        // The broker's own door: the goal is looked up in the store, the
        // card keeps it and the run, and this process has no Room and no
        // browser to need.
        let through = |title: &str, needs: &[ToolGroup], goal: &str| {
            super::suggest(&chat, &principal, "run-door", &idea_of(title, needs, goal))
        };
        assert_eq!(
            through("A run plan", &[ToolGroup::Goals], open.id.as_str()),
            SuggestResult::Suggested
        );
        assert!(reason(&through("Old news", &[], done.id.as_str())).contains("closed"));
        assert!(
            reason(&through(
                "In my computer",
                &[ToolGroup::Room],
                open.id.as_str()
            ))
            .contains("no computer of its own")
        );
        assert!(
            reason(&through(
                "In a browser",
                &[ToolGroup::Browser],
                open.id.as_str()
            ))
            .contains("no browser")
        );
        let page = listed(&chat);
        assert_eq!(page.len(), 1);
        let kept = page.first().unwrap();
        assert_eq!(kept.run, "run-door");
        assert_eq!(kept.needs, [ToolGroup::Goals]);
        assert_eq!(
            kept.source,
            Some(SuggestionSource::Goal {
                id: open.id.as_str().to_owned(),
                title: String::from("Run 5k by June"),
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A time `secs` after a fixed one.
    fn at(secs: i64) -> hatchling_scheduler::Timestamp {
        hatchling_scheduler::Timestamp::from_second(1_800_000_000 + secs).unwrap()
    }

    /// These ids, as a set of the owner's open goals.
    fn ids(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// The page as the owner's app lists it at `now`, with their open
    /// goals as `open` reads them: each idea's title and state.
    fn listed_at(
        config: &Config,
        now: hatchling_scheduler::Timestamp,
        open: &dyn Fn() -> Option<BTreeSet<String>>,
    ) -> Vec<(String, SuggestionState)> {
        if let SuggestionResult::Listed { suggestions } =
            handle_at(config, &SuggestionOp::List, now, open)
        {
            suggestions
                .into_iter()
                .map(|kept| (kept.title, kept.state))
                .collect()
        } else {
            Vec::new()
        }
    }

    /// An idea that has waited more than fourteen days is stale when the
    /// owner's app lists the page, and no sooner; one the owner started
    /// or set aside isn't judged. What is found is written back.
    #[test]
    fn an_idea_that_waited_fourteen_days_goes_stale_when_the_page_is_listed() {
        use SuggestionState::{Dismissed, Stale, Started, Suggested};
        let dir = scratch("stale");
        let chat = config(&dir, Some("local"));
        let all = everything();
        for (run, title) in [("r1", "Waits"), ("r2", "Was started"), ("r3", "Set aside")] {
            assert_eq!(
                suggest_at(&chat, run, &all, at(0), &idea(title)),
                SuggestResult::Suggested
            );
        }
        let id = |title: &str| {
            listed(&chat)
                .into_iter()
                .find(|kept| kept.title == title)
                .unwrap()
                .suggestion_id
        };
        let _ = handle_at(
            &chat,
            &SuggestionOp::Start {
                id: id("Was started"),
            },
            at(1),
            &|| None,
        );
        let _ = handle_at(
            &chat,
            &SuggestionOp::Dismiss {
                id: id("Set aside"),
            },
            at(1),
            &|| None,
        );
        let none = || Some(ids(&["g-open"]));
        let states = |now| {
            listed_at(&chat, now, &none)
                .into_iter()
                .map(|(_, state)| state)
                .collect::<Vec<_>>()
        };
        // Newest first: set aside, started, waiting.
        assert_eq!(
            states(at(STALE_SECS)),
            [Dismissed, Started, Suggested],
            "at fourteen days it still waits"
        );
        assert_eq!(states(at(STALE_SECS + 1)), [Dismissed, Started, Stale]);
        // Written back: read with no clock and no goals, it is still stale.
        assert_eq!(
            listed(&chat)
                .iter()
                .map(|kept| kept.state)
                .collect::<Vec<_>>(),
            [Dismissed, Started, Stale]
        );
        assert_eq!(STALE_SECS, 1_209_600);
        // A date that can't be read is no reason to retire an idea.
        let path = chat.scheduler.ideas_log();
        let mut page = read(&path).unwrap();
        for kept in &mut page {
            kept.state = Suggested;
            kept.suggested_at = String::from("not a date");
        }
        write(&path, &page).unwrap();
        assert_eq!(
            states(at(10 * STALE_SECS)),
            [Suggested, Suggested, Suggested]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An idea whose goal is closed or gone has lost its source, whatever
    /// its age; with the owner's goals unreadable no source is judged, and
    /// an idea kept before sources were has none to lose.
    #[test]
    fn an_idea_whose_goal_is_closed_has_lost_its_source() {
        use SuggestionState::{SourceGone, Stale, Suggested};
        let dir = scratch("gone");
        let chat = config(&dir, Some("local"));
        let path = chat.scheduler.ideas_log();
        crate::kept_log::ensure_parent(&path).unwrap();
        // One kept before sources were, as old as the page.
        std::fs::write(
            &path,
            format!(
                r#"[{{"suggestion_id":"idea-old","title":"No source","summary":"s","why":"w","area":"career","suggested_at":"{}","state":"suggested"}}]"#,
                at(0)
            ),
        )
        .unwrap();
        assert_eq!(
            suggest_at(&chat, "r1", &everything(), at(0), &idea("From a goal")),
            SuggestResult::Suggested
        );
        let titles =
            |now, goals: &dyn Fn() -> Option<BTreeSet<String>>| listed_at(&chat, now, goals);
        let both = |first, second| {
            vec![
                (String::from("From a goal"), first),
                (String::from("No source"), second),
            ]
        };
        // The goals couldn't be read: nothing is judged by its source.
        assert_eq!(titles(at(1), &|| None), both(Suggested, Suggested));
        // Its goal is open: it waits.
        assert_eq!(
            titles(at(1), &|| Some(ids(&["g-open", "g-other"]))),
            both(Suggested, Suggested)
        );
        // Its goal is no longer open: gone, at once; the one with no source waits.
        assert_eq!(
            titles(at(1), &|| Some(ids(&["g-other"]))),
            both(SourceGone, Suggested)
        );
        // Later the other goes stale; the first stays gone, and isn't judged again.
        assert_eq!(
            titles(at(STALE_SECS + 1), &|| Some(ids(&["g-open"]))),
            both(SourceGone, Stale)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stale and gone at once, an idea is said to have lost its source:
    /// that is what bringing it back depends on.
    #[test]
    fn an_idea_both_stale_and_without_its_goal_has_lost_its_source() {
        let dir = scratch("both");
        let chat = config(&dir, Some("local"));
        assert_eq!(
            suggest_at(&chat, "r1", &everything(), at(0), &idea("From a goal")),
            SuggestResult::Suggested
        );
        assert_eq!(
            listed_at(&chat, at(STALE_SECS + 1), &|| Some(ids(&[]))),
            [(String::from("From a goal"), SuggestionState::SourceGone)]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Brought back, a stale idea waits anew from then; one whose goal is
    /// still closed stays as it is, and comes back once the goal is open
    /// again. The owner's goals are asked after only when they bear on it.
    #[test]
    fn the_owner_brings_back_a_stale_idea_and_not_one_whose_goal_is_closed() {
        use SuggestionState::{SourceGone, Stale, Suggested};
        let dir = scratch("back");
        let chat = config(&dir, Some("local"));
        let all = everything();
        assert_eq!(
            suggest_at(&chat, "r1", &all, at(0), &idea("Stale")),
            SuggestResult::Suggested
        );
        let later = STALE_SECS + 1;
        assert_eq!(
            listed_at(&chat, at(later), &|| Some(ids(&["g-open"]))),
            [(String::from("Stale"), Stale)]
        );
        let id = listed(&chat).first().unwrap().suggestion_id.clone();
        let restore = SuggestionOp::Restore { id: id.clone() };
        let asked = std::cell::Cell::new(0_u32);
        let unasked = || {
            asked.set(asked.get() + 1);
            None
        };
        let SuggestionResult::Changed { suggestion } =
            handle_at(&chat, &restore, at(later), &unasked)
        else {
            unreachable!("a stale idea wasn't brought back");
        };
        assert_eq!(suggestion.state, Suggested);
        assert_eq!(
            suggestion.suggested_at,
            at(later).to_string(),
            "it waits from now"
        );
        assert_eq!(asked.get(), 0, "a stale idea's goal was asked after");
        // It has a new fourteen days.
        assert_eq!(
            listed_at(&chat, at(later + STALE_SECS), &|| Some(ids(&["g-open"]))),
            [(String::from("Stale"), Suggested)]
        );
        // Its goal closes: gone; and it can't be brought back while it is.
        assert_eq!(
            listed_at(&chat, at(later + 5), &|| Some(ids(&[]))),
            [(String::from("Stale"), SourceGone)]
        );
        for closed in [Some(ids(&[])), Some(ids(&["g-other"])), None] {
            let SuggestionResult::Changed { suggestion } =
                handle_at(&chat, &restore, at(later + 6), &|| closed.clone())
            else {
                unreachable!("the idea wasn't answered as it stands");
            };
            assert_eq!(suggestion.state, SourceGone, "{closed:?}");
        }
        assert_eq!(
            listed(&chat).first().unwrap().state,
            SourceGone,
            "nothing was written"
        );
        // The goal is open again: it comes back, and waits from then.
        let SuggestionResult::Changed { suggestion } =
            handle_at(&chat, &restore, at(later + 7), &|| Some(ids(&["g-open"])))
        else {
            unreachable!("the idea wasn't brought back");
        };
        assert_eq!(suggestion.state, Suggested);
        assert_eq!(suggestion.suggested_at, at(later + 7).to_string());
        // The owner may still start one that is gone, or set it aside.
        let _ = listed_at(&chat, at(later + 8), &|| Some(ids(&[])));
        assert!(matches!(
            handle_at(&chat, &SuggestionOp::Start { id }, at(later + 9), &|| None),
            SuggestionResult::Changed { suggestion } if suggestion.state == SuggestionState::Started
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stale idea no longer counts as waiting: a run suggesting after
    /// it finds room, and the page keeps it, as stale. Sixty may wait.
    #[test]
    fn a_stale_idea_makes_room_for_a_run_that_suggests() {
        let dir = scratch("room");
        let chat = config(&dir, Some("local"));
        let all = everything();
        for n in 0..MAX_WAITING {
            assert_eq!(
                suggest_at(
                    &chat,
                    &format!("r{n}"),
                    &all,
                    at(0),
                    &idea(&format!("Idea {n}"))
                ),
                SuggestResult::Suggested
            );
        }
        assert!(
            reason(&suggest_at(
                &chat,
                "late",
                &all,
                at(STALE_SECS),
                &idea("No room")
            ))
            .contains("full")
        );
        assert_eq!(
            suggest_at(&chat, "late", &all, at(STALE_SECS + 1), &idea("Room now")),
            SuggestResult::Suggested
        );
        let page = listed(&chat);
        assert_eq!(page.len(), MAX_WAITING + 1);
        assert_eq!(
            page.iter()
                .filter(|kept| kept.state == SuggestionState::Stale)
                .count(),
            MAX_WAITING
        );
        assert_eq!(
            page.first().map(|kept| kept.state),
            Some(SuggestionState::Suggested)
        );
        assert_eq!(MAX_WAITING, 60);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The owner's open goals are read from their own goals store, as
    /// their goals page reads them; with no store, or one that can't be
    /// read, there are none to judge by. Listed through the broker's own
    /// door, an idea whose goal was closed has lost its source.
    #[test]
    fn the_owners_open_goals_are_read_from_their_store() {
        use std::str::FromStr as _;

        use hatchling_goals::{GoalOutcome, GoalsStore};
        use hatchling_memory::SessionId;
        let dir = scratch("open-goals");
        let mut chat = config(&dir, Some("local"));
        assert_eq!(open_goals(&chat), None, "no store: none to judge by");
        chat.goals.root = dir.join("goals");
        let principal = MemoryPrincipal {
            peer_uid: chat.agent_uid.as_u32(),
            session_id: SessionId::from_str("sess-ideas").unwrap(),
        };
        let store = GoalsStore::open(&chat.goals.root).unwrap();
        let kept_open = store
            .create_goal(&principal, "Run 5k by June", "", 10)
            .unwrap();
        let closed = store.create_goal(&principal, "Read more", "", 11).unwrap();
        let _ = store
            .close_goal(&principal, &closed.id, GoalOutcome::Abandoned, 12)
            .unwrap();
        assert_eq!(open_goals(&chat), Some(ids(&[kept_open.id.as_str()])));
        // An idea from each, kept while both were open.
        let ground = |goal: &str| {
            let goal = goal.to_owned();
            move |id: &str| {
                if id == goal {
                    GoalFound::Open(String::from("a goal"))
                } else {
                    GoalFound::Absent
                }
            }
        };
        for (run, title, goal) in [
            ("r1", "From the open one", kept_open.id.as_str()),
            ("r2", "From the closed one", closed.id.as_str()),
        ] {
            let found = ground(goal);
            let kept = suggest_from(
                &chat,
                &Ground {
                    run,
                    here: &everything(),
                    goal: &found,
                    now: hatchling_scheduler::Timestamp::now(),
                },
                &idea_of(title, &[], goal),
            );
            assert_eq!(kept, SuggestResult::Suggested);
        }
        let SuggestionResult::Listed { suggestions } = handle(&chat, &SuggestionOp::List) else {
            unreachable!("the page wasn't listed");
        };
        assert_eq!(
            suggestions
                .iter()
                .map(|kept| (kept.title.as_str(), kept.state))
                .collect::<Vec<_>>(),
            [
                ("From the closed one", SuggestionState::SourceGone),
                ("From the open one", SuggestionState::Suggested)
            ]
        );
        chat.goals.root = dir.join("a-file");
        std::fs::write(&chat.goals.root, "").unwrap();
        assert_eq!(open_goals(&chat), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
