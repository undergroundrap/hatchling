//! What became of the ideas on the owner's Ideas page, kept for the
//! ranking that will learn from it.
//!
//! The page itself keeps only the newest [`crate::suggestions::MAX_KEPT`]
//! cards, and what the owner fired on, set aside or let go stale is the
//! first it lets go. This record keeps what happened to them: a line per
//! event, when, which idea, where it came from and its area. Never a
//! card's words, a goal's id or title, or what was said in a chat.
//!
//! Nothing reads it yet. It is kept now because the history it holds
//! can't be made later.
//!
//! One file beside the page (`<scheduler dir>.owner/idea-signals`),
//! `0600`, kept as a [`crate::kept_log`] with a bound of its own: past
//! [`MAX_LINES`] it keeps the newest [`MAX_KEPT`]. An event that can't be
//! kept is logged; what the owner did stands.

use std::path::{Path, PathBuf};

use hatchling_core::SuggestionArea;
use hatchling_core::ui::{Suggestion, SuggestionSource};
use serde::{Deserialize, Serialize};

/// The record's file, beside the Ideas page.
pub const SIGNALS_FILE: &str = "idea-signals";

/// How many events are kept: the newest.
pub const MAX_KEPT: usize = 10_000;

/// How many lines the file may grow to before it's cut back to
/// [`MAX_KEPT`].
pub const MAX_LINES: usize = 2 * MAX_KEPT;

/// What happened to an idea.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// A run put it on the page.
    Suggested,
    /// The owner kept it for later, from a chat that ran out of steps.
    Kept,
    /// The owner started it.
    Fired,
    /// The owner set it aside.
    Dismissed,
    /// The owner brought it back.
    Restored,
    /// It waited too long, and the broker found it stale.
    Stale,
    /// The goal it came from was closed or removed.
    SourceGone,
    /// The run in the conversation the owner started for it answered.
    Answered,
}

/// Where an idea came from, by kind alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// One of the owner's goals.
    Goal,
    /// A chat the owner kept for later.
    Chat,
    /// Kept before ideas had a source.
    None,
}

impl Source {
    /// The kind of `source`.
    #[must_use]
    pub const fn of(source: Option<&SuggestionSource>) -> Self {
        match source {
            Some(SuggestionSource::Goal { .. }) => Self::Goal,
            Some(SuggestionSource::Chat) => Self::Chat,
            None => Self::None,
        }
    }
}

/// One event, as the record keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    /// When it happened.
    pub at: String,
    /// The idea's id.
    pub idea: String,
    /// What happened.
    pub event: Event,
    /// Where the idea came from.
    pub from: Source,
    /// Its area.
    pub area: SuggestionArea,
}

/// The record beside the Ideas page at `page`.
#[must_use]
pub fn beside(page: &Path) -> PathBuf {
    page.with_file_name(SIGNALS_FILE)
}

/// Keeps that `event` happened to `idea` at `at`, in the record beside
/// the Ideas page at `page`.
pub fn record(page: &Path, idea: &Suggestion, event: Event, at: hatchling_scheduler::Timestamp) {
    let signal = Signal {
        at: at.to_string(),
        idea: idea.suggestion_id.clone(),
        event,
        from: Source::of(idea.source.as_ref()),
        area: idea.area,
    };
    if let Err(error) = crate::kept_log::append_within(&beside(page), &signal, MAX_LINES, MAX_KEPT)
    {
        tracing::warn!(%error, "what became of an idea wasn't kept");
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use hatchling_core::ui::SuggestionState;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-idea-signals-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn idea(source: Option<SuggestionSource>) -> Suggestion {
        Suggestion {
            suggestion_id: String::from("idea-1"),
            title: String::from("Plan the garden"),
            summary: String::from("a secret summary"),
            why: String::from("a secret reason"),
            area: SuggestionArea::Family,
            suggested_at: String::from("2026-11-16T00:00:00Z"),
            state: SuggestionState::Suggested,
            needs: Vec::new(),
            source,
            run: String::from("run-1"),
        }
    }

    fn at(second: i64) -> hatchling_scheduler::Timestamp {
        hatchling_scheduler::Timestamp::from_second(second).unwrap()
    }

    fn kept(page: &Path) -> Vec<Signal> {
        std::fs::read_to_string(beside(page))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn an_event_is_kept_beside_the_page_by_kind_never_by_words() {
        let dir = scratch("kept");
        let page = dir.join("ideas");
        let goal = SuggestionSource::Goal {
            id: String::from("g-garden-plot"),
            title: String::from("Grow tomatoes"),
        };
        record(&page, &idea(Some(goal)), Event::Fired, at(1_800_000_000));
        record(
            &page,
            &idea(Some(SuggestionSource::Chat)),
            Event::Kept,
            at(1_800_000_001),
        );
        record(&page, &idea(None), Event::Stale, at(1_800_000_002));
        assert_eq!(beside(&page), dir.join("idea-signals"));
        let signal = |second, event, from| Signal {
            at: at(second).to_string(),
            idea: String::from("idea-1"),
            event,
            from,
            area: SuggestionArea::Family,
        };
        assert_eq!(
            kept(&page),
            [
                signal(1_800_000_000, Event::Fired, Source::Goal),
                signal(1_800_000_001, Event::Kept, Source::Chat),
                signal(1_800_000_002, Event::Stale, Source::None),
            ]
        );
        let text = std::fs::read_to_string(beside(&page)).unwrap();
        for word in ["garden", "tomatoes", "secret", "run-1"] {
            assert!(!text.contains(word), "{word} kept: {text}");
        }
        assert!(
            text.lines()
                .next()
                .unwrap()
                .contains(r#""event":"fired","from":"goal","area":"family""#),
            "{text}"
        );
        assert_eq!(
            std::fs::metadata(beside(&page))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The record has its own bound, not the decisions' and the Feed's:
    /// it grows to 20,000 lines, and past them keeps the newest 10,000.
    #[test]
    fn the_record_is_bounded_to_the_newest_ten_thousand() {
        let dir = scratch("bounded");
        let page = dir.join("ideas");
        let mut text = String::from("older\n").repeat(19_998);
        text.push_str("newest before\n");
        std::fs::write(beside(&page), text).unwrap();
        let lines = || {
            std::fs::read_to_string(beside(&page))
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        record(&page, &idea(None), Event::Fired, at(1_800_000_000));
        assert_eq!(lines().len(), 20_000, "at its bound, not past it");
        record(&page, &idea(None), Event::Dismissed, at(1_800_000_001));
        let kept = lines();
        assert_eq!(kept.len(), 10_000, "cut back to the newest");
        assert!(kept.last().unwrap().contains(r#""event":"dismissed""#));
        assert!(
            kept.iter()
                .rev()
                .nth(1)
                .unwrap()
                .contains(r#""event":"fired""#)
        );
        assert_eq!(
            kept.iter().filter(|line| *line == "newest before").count(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_event_that_cant_be_kept_leaves_nothing_and_panics_not() {
        let dir = scratch("unkept");
        // The page's directory is a file: the record can't be made.
        let blocked = dir.join("owner");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let page = blocked.join("ideas");
        record(&page, &idea(None), Event::Dismissed, at(1_800_000_000));
        assert!(!beside(&page).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
