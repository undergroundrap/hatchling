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
//! The page is one file in the broker's directory of the owner's records
//! (`<scheduler dir>.owner/ideas`), `0600`, rewritten whole through a
//! sibling `.next` file: the newest [`MAX_KEPT`].

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::ui::{Suggestion, SuggestionOp, SuggestionResult, SuggestionState};
use hatchling_core::{SuggestOp, SuggestResult};

pub use hatchling_core::ui::IDEAS_PAGE;

use crate::Config;

/// How many suggestions the page keeps: the newest.
pub const MAX_KEPT: usize = 100;

/// How many may wait for the owner at once.
pub const MAX_WAITING: usize = 20;

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
        "Suggest ideas for the owner's Ideas page. They asked for this:\n\n{prompt}\n\nUse your tools to read what you can about them first: their goals, and what they've asked you to remember. Then suggest up to three ideas with idea_suggest, one call each: things Hatchling could do for them with the tools and settings it has here, that they haven't asked for. Ground each in what you read, and say why in their terms. Don't suggest what Hatchling can't do here, and don't invent facts about them. When you're done, answer in one sentence."
    )
}

/// Puts `op` on the owner's Ideas page, if the session `config` serves
/// speaks to the owner.
///
/// A run's own config is scoped: it doesn't name the schedule store. The
/// run's follow-up scope does, as the broker set it when it started the
/// run, so the page is found from there.
#[must_use]
pub fn suggest(config: &Config, op: &SuggestOp) -> SuggestResult {
    let result = suggest_from(config, op);
    match &result {
        SuggestResult::Suggested => tracing::info!("an idea was suggested for the Ideas page"),
        SuggestResult::Unasked { reason } => {
            tracing::info!(%reason, "an idea wasn't kept on the Ideas page");
        }
    }
    result
}

fn suggest_from(config: &Config, op: &SuggestOp) -> SuggestResult {
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
    page.insert(
        0,
        Suggestion {
            suggestion_id,
            title: title.to_owned(),
            summary: summary.to_owned(),
            why: why.to_owned(),
            area: *area,
            suggested_at: hatchling_scheduler::Timestamp::now().to_string(),
            state: SuggestionState::Suggested,
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

/// Does `op`, the owner's, on their Ideas page.
#[must_use]
pub fn handle(config: &Config, op: &SuggestionOp) -> SuggestionResult {
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
        SuggestionOp::List => return SuggestionResult::Listed { suggestions: page },
        SuggestionOp::Start { id } => (id, SuggestionState::Started),
        SuggestionOp::Dismiss { id } => (id, SuggestionState::Dismissed),
        SuggestionOp::Restore { id } => (id, SuggestionState::Suggested),
    };
    let Some(kept) = page.iter_mut().find(|kept| &kept.suggestion_id == id) else {
        return SuggestionResult::NotFound;
    };
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
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::config::FollowupScope;
    use hatchling_core::SuggestionArea;

    fn scratch(name: &str) -> std::path::PathBuf {
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
    fn config(dir: &Path, channel: Option<&str>) -> Config {
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
        SuggestOp::Suggest {
            title: title.to_owned(),
            summary: String::from("A week of short runs, on your calendar."),
            why: String::from("You set a goal to run 5k."),
            area: SuggestionArea::Health,
        }
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
            };
            assert!(reason(&suggest(&chat, &op)).contains("needs a title"));
        }
        let long = |title: String, summary: String, why: String| SuggestOp::Suggest {
            title,
            summary,
            why,
            area: SuggestionArea::Career,
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
        // A page whose directory is gone can't be written.
        std::fs::remove_file(&path).unwrap();
        let gone = config(&dir.join("missing"), Some("local"));
        assert!(reason(&suggest(&gone, &idea("x"))).contains("couldn't keep it"));
        assert_eq!(
            handle(
                &gone,
                &SuggestionOp::Start {
                    id: String::from("idea-x")
                }
            ),
            SuggestionResult::NotFound
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_task_carries_the_prompt_and_asks_for_grounded_suggestions() {
        let task = task("Things for my running.");
        assert!(task.contains("\n\nThings for my running.\n\n"));
        assert!(task.contains("idea_suggest"));
        assert!(task.contains("up to three"));
        assert!(task.contains("don't invent facts"));
    }
}
