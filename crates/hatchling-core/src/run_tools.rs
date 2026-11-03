//! The tool groups a run's agent is offered only when the operator turned
//! them on.
//!
//! Reading pages (`HATCHLING_WEB`), searching (`HATCHLING_WEB_SEARCH`),
//! a browser, skills (`HATCHLING_SKILLS_DIR`) and reading the owner's
//! goals (`HATCHLING_GOALS_DIR`, and a run that holds `query-goals`) are
//! off unless the operator sets them up. The broker
//! tells each run which are on (`HATCHLING_RUN_TOOLS`), and the agent
//! offers its model only those: a tool the broker can only refuse costs
//! a small model a turn, and room in its prompt. The broker stays the
//! authority. A call to a group that is off is refused there all the
//! same.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// A tool group the operator turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolGroup {
    /// Reading a page the owner allows.
    Web,
    /// Searching the web.
    Search,
    /// Driving a browser.
    Browser,
    /// Invoking an installed skill.
    Skills,
    /// Reading the owner's open goals.
    Goals,
}

impl ToolGroup {
    /// Every group, in order.
    pub const ALL: [Self; 5] = [
        Self::Web,
        Self::Search,
        Self::Browser,
        Self::Skills,
        Self::Goals,
    ];

    /// The group's name in [`format`] and [`parse`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Search => "search",
            Self::Browser => "browser",
            Self::Skills => "skills",
            Self::Goals => "goals",
        }
    }
}

/// The value naming no group: every one is off.
pub const NONE: &str = "none";

/// The groups `on`, comma-separated, or [`NONE`].
#[must_use]
pub fn format(on: &BTreeSet<ToolGroup>) -> String {
    if on.is_empty() {
        return String::from(NONE);
    }
    on.iter()
        .map(|group| group.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Parses [`format`]'s output: group names, comma-separated, or [`NONE`].
///
/// # Errors
///
/// Why it isn't one, naming the part that isn't a group.
pub fn parse(raw: &str) -> Result<BTreeSet<ToolGroup>, String> {
    if raw.trim() == NONE {
        return Ok(BTreeSet::new());
    }
    raw.split(',')
        .map(str::trim)
        .map(|name| {
            ToolGroup::ALL
                .into_iter()
                .find(|group| group.as_str() == name)
                .ok_or_else(|| {
                    format!("{name:?} is not a tool group: web, search, browser, skills or goals")
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_set_of_groups_round_trips() {
        for bits in 0..32_u8 {
            let on: BTreeSet<ToolGroup> = ToolGroup::ALL
                .into_iter()
                .enumerate()
                .filter(|(i, _)| bits & (1 << i) != 0)
                .map(|(_, group)| group)
                .collect();
            assert_eq!(parse(&format(&on)).unwrap(), on, "{bits}");
        }
        assert_eq!(format(&BTreeSet::new()), "none");
        assert_eq!(
            format(&ToolGroup::ALL.into_iter().collect()),
            "web,search,browser,skills,goals"
        );
    }

    #[test]
    fn anything_else_is_refused() {
        for raw in ["", "web,", "shell", "web,none", "Web", "web;search"] {
            assert!(parse(raw).is_err(), "{raw:?}");
        }
        assert_eq!(
            parse(" search , web ").unwrap(),
            [ToolGroup::Web, ToolGroup::Search].into_iter().collect()
        );
    }
}
