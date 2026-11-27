//! Where a kept item came from (WO-048): its record, written by the broker
//! alone, when the owner approves keeping it.
//!
//! Nothing the agent or a cell says sets or changes a record:
//! - [`MemoryStore::write`](crate::MemoryStore::write), the cell's path,
//!   writes none, and replacing a kept item through it drops its record;
//! - [`MemoryStore::keep`](crate::MemoryStore::keep), the broker's path
//!   after the owner's approval, is the only way one is written.
//!
//! An item kept before records were (Hatchling 0.10 and earlier) has none,
//! and says its provenance isn't recorded rather than guessing.
//!
//! The record holds every field the work order's later steps fill (kind,
//! why and how to apply, a date to check again, an absence, what it
//! supersedes), so those steps add behavior without changing the format.

use serde::{Deserialize, Serialize};

use crate::MemoryId;

/// Whose words an item keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Words {
    /// The agent proposed them, and the owner approved them as shown.
    Agent,
}

/// What kind of thing an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// About the owner.
    Owner,
    /// The owner's guidance on how the agent should work.
    Guidance,
    /// A fact about ongoing work.
    Work,
    /// Where something lives.
    Pointer,
}

/// Where a kept item came from, and when it was true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptRecord {
    /// Whose words they are.
    pub words: Words,
    /// The approval it was kept by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    /// The run that proposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The chat that run answered, when it was a chat's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// When it was kept, Unix seconds.
    pub kept_at_secs: u64,
    /// When the owner last confirmed it, Unix seconds: when it was kept,
    /// until they confirm it again.
    pub confirmed_at_secs: u64,
    /// What kind of thing it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MemoryKind>,
    /// Why, as the owner gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// When it matters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how_to_apply: Option<String>,
    /// The day after which it's put to the owner to confirm, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_by: Option<String>,
    /// Whether it records a settled negative: something known not to be so.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub absence: bool,
    /// The item it corrects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<MemoryId>,
}

impl KeptRecord {
    /// A record of the agent's words, approved in `approval`, proposed by
    /// `run` in `chat`, kept at `now_secs`.
    #[must_use]
    pub const fn agent_words(
        approval: Option<String>,
        run: Option<String>,
        chat: Option<String>,
        now_secs: u64,
    ) -> Self {
        Self {
            words: Words::Agent,
            approval,
            run,
            chat,
            kept_at_secs: now_secs,
            confirmed_at_secs: now_secs,
            kind: None,
            why: None,
            how_to_apply: None,
            check_by: None,
            absence: false,
            supersedes: None,
        }
    }

    /// The record as a run is given it: without the ids of the run, chat
    /// and approval it came from, which only the owner's list shows.
    #[must_use]
    pub fn shareable(&self) -> Self {
        Self {
            approval: None,
            run: None,
            chat: None,
            ..self.clone()
        }
    }
}

/// The calendar day, `YYYY-MM-DD` in UTC, of `secs` past the Unix epoch.
#[must_use]
pub fn utc_day(secs: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm, for days since 1970-01-01.
    let days = secs / 86_400;
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_are_calendar_days() {
        for (secs, day) in [
            (0, "1970-01-01"),
            (86_399, "1970-01-01"),
            (86_400, "1970-01-02"),
            (951_782_400, "2000-02-29"),
            (951_868_800, "2000-03-01"),
            (1_709_164_800, "2024-02-29"),
            (1_735_603_200, "2024-12-31"),
            (1_735_689_600, "2025-01-01"),
            (1_796_256_000, "2026-12-03"),
            (1_795_651_200, "2026-11-26"),
            (4_107_542_400, "2100-03-01"),
        ] {
            assert_eq!(utc_day(secs), day, "{secs}");
        }
    }

    #[test]
    fn a_record_reads_back_and_omits_what_it_lacks() {
        let record = KeptRecord::agent_words(
            Some(String::from("confirm-1")),
            Some(String::from("run-1")),
            Some(String::from("local")),
            100,
        );
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(
            json,
            r#"{"words":"agent","approval":"confirm-1","run":"run-1","chat":"local","kept_at_secs":100,"confirmed_at_secs":100}"#
        );
        assert_eq!(serde_json::from_str::<KeptRecord>(&json).unwrap(), record);
        let full = KeptRecord {
            kind: Some(MemoryKind::Guidance),
            why: Some(String::from("w")),
            how_to_apply: Some(String::from("h")),
            check_by: Some(String::from("2027-01-01")),
            absence: true,
            supersedes: Some("kept-1".parse().unwrap()),
            ..record
        };
        let json = serde_json::to_string(&full).unwrap();
        assert!(json.contains(r#""kind":"guidance""#) && json.contains(r#""absence":true"#));
        assert_eq!(serde_json::from_str::<KeptRecord>(&json).unwrap(), full);
    }

    #[test]
    fn a_run_is_given_no_ids_of_where_it_came_from() {
        let record = KeptRecord {
            why: Some(String::from("w")),
            ..KeptRecord::agent_words(
                Some(String::from("confirm-1")),
                Some(String::from("run-1")),
                Some(String::from("local")),
                100,
            )
        };
        let shared = record.shareable();
        assert_eq!(
            (shared.approval, shared.run, shared.chat),
            (None, None, None)
        );
        assert_eq!(shared.why.as_deref(), Some("w"));
        assert_eq!(shared.kept_at_secs, 100);
    }
}
