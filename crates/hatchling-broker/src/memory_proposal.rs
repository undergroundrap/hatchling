//! What the agent asks to keep, checked before the owner is asked
//! (WO-048, v35).
//!
//! A proposal says what kind of thing it is. Guidance and facts about
//! ongoing work also say why and when they apply, so a kept rule can be
//! weighed at its edges and not only followed. A proposal may name a day
//! after which the owner is asked to confirm it.
//!
//! **Dates are dates** (the maintainer's decision of 2026-11-26). A kept
//! memory is read in later weeks: "next Thursday" would mean a different
//! day each week. So:
//! - a day the proposal names is `YYYY-MM-DD`, a real one, after today;
//! - its words, reason and when-it-applies may not use a relative date
//!   ("tomorrow", "next Thursday", "in two weeks").
//!
//! The agent is told which, and to name the calendar date instead.

use hatchling_memory::MemoryKind;

use crate::memory_keep::MAX_PROPOSAL_BYTES;

/// The longest a reason, or when one applies, is, in bytes.
pub const MAX_REASON_BYTES: usize = 500;

/// Seconds in a day.
const DAY_SECS: u64 = 86_400;

/// What the agent asks to keep, as it said it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asked<'a> {
    /// The words to keep.
    pub content: &'a str,
    /// What kind of thing it is.
    pub kind: Option<&'a str>,
    /// Why.
    pub why: Option<&'a str>,
    /// When it applies.
    pub how_to_apply: Option<&'a str>,
    /// The day after which the owner is asked to confirm it.
    pub check_by: Option<&'a str>,
    /// Whether it records something known not to be so.
    pub absence: bool,
}

impl<'a> Asked<'a> {
    /// A fact about the owner, in `content`, with nothing else said.
    #[must_use]
    pub const fn about_owner(content: &'a str) -> Self {
        Self {
            content,
            kind: Some("owner"),
            why: None,
            how_to_apply: None,
            check_by: None,
            absence: false,
        }
    }
}

/// A proposal the broker checked: what's put to the owner, and what's kept
/// once they approve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The words, trimmed.
    pub content: String,
    /// What kind of thing it is.
    pub kind: MemoryKind,
    /// Why, when given.
    pub why: Option<String>,
    /// When it applies, when given.
    pub how_to_apply: Option<String>,
    /// The day to check it again, `YYYY-MM-DD`, when given.
    pub check_by: Option<String>,
    /// Whether it records something known not to be so.
    pub absence: bool,
}

impl Checked {
    /// A fact about the owner, in `content`, with nothing else said.
    #[cfg(test)]
    pub(crate) fn owner(content: &str) -> Self {
        Self {
            content: content.to_owned(),
            kind: MemoryKind::Owner,
            why: None,
            how_to_apply: None,
            check_by: None,
            absence: false,
        }
    }
}

/// Why a proposal isn't put to the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfit {
    /// Its words are empty, too long, or hold control characters.
    Words,
    /// What the agent is told to change.
    Said(String),
}

/// Checks `asked`, today being the day of `now_secs`.
///
/// # Errors
///
/// [`Unfit`] when it isn't to be put to the owner.
pub fn check(asked: &Asked<'_>, now_secs: u64) -> Result<Checked, Unfit> {
    let content = asked.content.trim();
    if !fit_text(content, MAX_PROPOSAL_BYTES) {
        return Err(Unfit::Words);
    }
    let kind = match asked.kind.map(str::trim) {
        Some("owner") => MemoryKind::Owner,
        Some("guidance") => MemoryKind::Guidance,
        Some("work") => MemoryKind::Work,
        Some("pointer") => MemoryKind::Pointer,
        _ => {
            return Err(said(
                "say what kind of thing it is: owner, guidance, work or pointer",
            ));
        }
    };
    let why = reason(asked.why, "why")?;
    let how_to_apply = reason(asked.how_to_apply, "how_to_apply")?;
    if matches!(kind, MemoryKind::Guidance | MemoryKind::Work)
        && (why.is_none() || how_to_apply.is_none())
    {
        return Err(said(
            "guidance and facts about work say why and how_to_apply, in the owner's terms",
        ));
    }
    for text in [Some(content), why.as_deref(), how_to_apply.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Some(phrase) = relative_date(text) {
            return Err(Unfit::Said(format!(
                "“{phrase}” means a different day next week: name the calendar date instead, as YYYY-MM-DD"
            )));
        }
    }
    let check_by = match asked.check_by.map(str::trim) {
        None | Some("") => None,
        Some(raw) => match calendar_day(raw) {
            Some(day) if day > now_secs / DAY_SECS => Some(raw.to_owned()),
            _ => {
                return Err(said(
                    "check_by is a calendar day after today, as YYYY-MM-DD",
                ));
            }
        },
    };
    Ok(Checked {
        content: content.to_owned(),
        kind,
        why,
        how_to_apply,
        check_by,
        absence: asked.absence,
    })
}

/// What the agent is told.
fn said(why: &str) -> Unfit {
    Unfit::Said(why.to_owned())
}

/// Whether `text` is something to keep: not empty, at most `max` bytes,
/// and no control characters but line breaks.
fn fit_text(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && text.chars().all(|c| c == '\n' || !c.is_control())
}

/// A reason given as `name`, trimmed, or none when it's empty.
fn reason(given: Option<&str>, name: &str) -> Result<Option<String>, Unfit> {
    let Some(text) = given.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if fit_text(text, MAX_REASON_BYTES) {
        Ok(Some(text.to_owned()))
    } else {
        Err(Unfit::Said(format!(
            "{name} is at most {MAX_REASON_BYTES} bytes, without control characters"
        )))
    }
}

/// The day number since 1970-01-01 of `raw`, when it's a real calendar
/// day written `YYYY-MM-DD`.
fn calendar_day(raw: &str) -> Option<u64> {
    let midnight = format!("{raw}T00:00:00Z");
    let at: hatchling_scheduler::Timestamp = midnight.parse().ok()?;
    // Written back exactly as given: no other shape the parser also takes.
    if at.to_string() != midnight {
        return None;
    }
    u64::try_from(at.as_second())
        .ok()
        .map(|secs| secs / DAY_SECS)
}

const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// Words that name a day only relative to when they're read.
const ALONE: [&str; 4] = ["today", "tonight", "tomorrow", "yesterday"];

/// Words that, before a weekday or a span, make it relative.
const LEADS: [&str; 4] = ["next", "last", "this", "coming"];

/// The spans a count is of.
const SPANS: [&str; 8] = [
    "day", "days", "week", "weeks", "month", "months", "year", "years",
];

/// Counts, as words.
const COUNTS: [&str; 15] = [
    "a", "an", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    "few", "couple", "several",
];

/// Whether `word` is a count: a number or a word for one.
fn is_count(word: &str) -> bool {
    // `word` is never empty: the words are split and the empty dropped.
    COUNTS.contains(&word) || word.bytes().all(|byte| byte.is_ascii_digit())
}

/// The first relative date `text` uses, as it says it.
fn relative_date(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let at = |index: usize| words.get(index).copied().unwrap_or_default();
    for index in 0..words.len() {
        let word = at(index);
        if ALONE.contains(&word) {
            return Some(word.to_owned());
        }
        let next = at(index + 1);
        if LEADS.contains(&word)
            && (WEEKDAYS.contains(&next) || SPANS.contains(&next) || next == "weekend")
        {
            return Some(format!("{word} {next}"));
        }
        // "in two weeks", "in 3 days".
        if word == "in" && is_count(next) && SPANS.contains(&at(index + 2)) {
            return Some(format!("in {next} {}", at(index + 2)));
        }
        // "two weeks ago", "3 days from now".
        if is_count(word) && SPANS.contains(&next) {
            let after = at(index + 2);
            if after == "ago" || (after == "from" && at(index + 3) == "now") {
                let tail = if after == "ago" { "ago" } else { "from now" };
                return Some(format!("{word} {next} {tail}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Noon on 2026-11-26, UTC.
    const NOW: u64 = 1_795_694_400;

    const fn asked<'a>(content: &'a str, kind: &'a str) -> Asked<'a> {
        Asked {
            kind: Some(kind),
            ..Asked::about_owner(content)
        }
    }

    #[test]
    fn each_kind_is_checked_and_guidance_and_work_say_why_and_when() {
        let owner = check(&Asked::about_owner("  Their cat is Biscuit.  "), NOW).unwrap();
        assert_eq!(
            owner,
            Checked {
                content: String::from("Their cat is Biscuit."),
                kind: MemoryKind::Owner,
                why: None,
                how_to_apply: None,
                check_by: None,
                absence: false,
            }
        );
        let absent = check(
            &Asked {
                absence: true,
                ..Asked::about_owner("They have no car.")
            },
            NOW,
        )
        .unwrap();
        assert!(absent.absence, "a settled negative is kept as one");
        assert_eq!(
            check(&asked("The plan is in docs/.", "pointer"), NOW)
                .unwrap()
                .kind,
            MemoryKind::Pointer
        );
        for kind in [None, Some(""), Some("fact"), Some("Owner")] {
            assert_eq!(
                check(
                    &Asked {
                        kind,
                        ..Asked::about_owner("x")
                    },
                    NOW
                ),
                Err(said(
                    "say what kind of thing it is: owner, guidance, work or pointer"
                )),
                "{kind:?}"
            );
        }
        for kind in ["guidance", "work"] {
            let bare = asked("Use metric units.", kind);
            let missing = Err(said(
                "guidance and facts about work say why and how_to_apply, in the owner's terms",
            ));
            assert_eq!(check(&bare, NOW), missing);
            assert_eq!(
                check(
                    &Asked {
                        why: Some("They're in Europe."),
                        ..bare
                    },
                    NOW
                ),
                missing
            );
            assert_eq!(
                check(
                    &Asked {
                        how_to_apply: Some("Any measurement."),
                        ..bare
                    },
                    NOW
                ),
                missing
            );
            assert_eq!(
                check(
                    &Asked {
                        why: Some(" "),
                        how_to_apply: Some("Any measurement."),
                        ..bare
                    },
                    NOW
                ),
                missing
            );
            let whole = check(
                &Asked {
                    why: Some(" They're in Europe. "),
                    how_to_apply: Some("Any measurement."),
                    ..bare
                },
                NOW,
            )
            .unwrap();
            assert_eq!(whole.why.as_deref(), Some("They're in Europe."));
            assert_eq!(whole.how_to_apply.as_deref(), Some("Any measurement."));
        }
    }

    #[test]
    fn words_and_reasons_are_bounded() {
        for bad in ["", "   ", "a\u{7}b"] {
            assert_eq!(
                check(&Asked::about_owner(bad), NOW),
                Err(Unfit::Words),
                "{bad:?}"
            );
        }
        let at_bound = "x".repeat(MAX_PROPOSAL_BYTES);
        assert!(check(&Asked::about_owner(&at_bound), NOW).is_ok());
        let over = "x".repeat(MAX_PROPOSAL_BYTES + 1);
        assert_eq!(check(&Asked::about_owner(&over), NOW), Err(Unfit::Words));
        assert!(check(&Asked::about_owner("two\nlines"), NOW).is_ok());
        let reason_at = "y".repeat(MAX_REASON_BYTES);
        let reason_over = "y".repeat(MAX_REASON_BYTES + 1);
        assert!(
            check(
                &Asked {
                    why: Some(&reason_at),
                    ..Asked::about_owner("x")
                },
                NOW
            )
            .is_ok()
        );
        assert_eq!(
            check(
                &Asked {
                    why: Some(&reason_over),
                    ..Asked::about_owner("x")
                },
                NOW
            ),
            Err(Unfit::Said(String::from(
                "why is at most 500 bytes, without control characters"
            )))
        );
        assert_eq!(
            check(
                &Asked {
                    how_to_apply: Some("a\u{1}b"),
                    ..Asked::about_owner("x")
                },
                NOW
            ),
            Err(Unfit::Said(String::from(
                "how_to_apply is at most 500 bytes, without control characters"
            )))
        );
    }

    #[test]
    fn a_relative_date_is_refused_wherever_it_is_said() {
        for (text, phrase) in [
            ("Their dentist is next Thursday.", "next thursday"),
            ("Call them TOMORROW.", "tomorrow"),
            ("Today they moved.", "today"),
            ("The review is in two weeks.", "in two weeks"),
            ("It ships in 3 days.", "in 3 days"),
            ("They started 2 months ago.", "2 months ago"),
            ("Due a week from now.", "a week from now"),
            ("Free this weekend.", "this weekend"),
            ("Busy last month.", "last month"),
            ("Back the coming Monday.", "coming monday"),
        ] {
            let message = format!(
                "“{phrase}” means a different day next week: name the calendar date instead, as YYYY-MM-DD"
            );
            assert_eq!(
                check(&Asked::about_owner(text), NOW),
                Err(Unfit::Said(message.clone())),
                "{text}"
            );
            let guidance = Asked {
                kind: Some("guidance"),
                why: Some(text),
                how_to_apply: Some("Always."),
                ..Asked::about_owner("Remind them.")
            };
            assert_eq!(
                check(&guidance, NOW),
                Err(Unfit::Said(message.clone())),
                "why: {text}"
            );
            let applied = Asked {
                why: Some("They asked."),
                how_to_apply: Some(text),
                ..guidance
            };
            assert_eq!(
                check(&applied, NOW),
                Err(Unfit::Said(message)),
                "how: {text}"
            );
        }
        for fine in [
            "Their dentist is on 2026-12-03.",
            "They write weekly notes.",
            "The next step is review.",
            "In Europe they use metric.",
            "They have two cats.",
            "A day off matters to them.",
            "The days are long in June.",
            "Several years of Rust.",
            "This is their monthly plan.",
            "They travel in summer months.",
            "Long years ago they lived in Rome.",
            "It takes two weeks from the start.",
            "They moved two houses ago.",
        ] {
            assert!(check(&Asked::about_owner(fine), NOW).is_ok(), "{fine}");
        }
    }

    #[test]
    fn a_day_to_check_again_is_a_real_day_after_today() {
        let on = |day: &'static str| Asked {
            check_by: Some(day),
            ..Asked::about_owner("x")
        };
        assert_eq!(
            check(&on("2026-11-27"), NOW).unwrap().check_by.as_deref(),
            Some("2026-11-27")
        );
        assert_eq!(check(&on(" "), NOW).unwrap().check_by, None);
        let refused = Err(said(
            "check_by is a calendar day after today, as YYYY-MM-DD",
        ));
        for bad in [
            "2026-11-26",
            "2026-11-25",
            "2026-02-30",
            "2026-13-01",
            "26-11-27",
            "2026/11/27",
            "2026-11-2x",
            "next week",
            "2026-11-270",
            "20261127",
            "2026-11-27T00:00:00Z",
            "+2026-11-27",
            "2026-1-27",
        ] {
            assert_eq!(check(&on(bad), NOW), refused, "{bad}");
        }
    }

    #[test]
    fn days_are_counted_from_the_epoch() {
        assert_eq!(calendar_day("1970-01-01"), Some(0));
        assert_eq!(calendar_day("1970-01-02"), Some(1));
        assert_eq!(calendar_day("2026-11-27"), Some(NOW / DAY_SECS + 1));
        assert_eq!(calendar_day("2024-02-29"), Some(19_782));
        assert_eq!(calendar_day("2023-02-29"), None);
    }
}
