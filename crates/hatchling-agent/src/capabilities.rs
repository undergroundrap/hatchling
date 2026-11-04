//! What a run can and can't do here, for its model: generated from this
//! install's live settings at the run's start, never written once and left
//! to drift.
//!
//! The model sees its tools, but not their limits, and fills the gap by
//! guessing: offering to run a command on the owner's machine, or to look
//! something up with the web off. This note, appended to the system
//! prompt, says what is so on this install. It is the broker's word
//! (`HATCHLING_RUN_TOOLS`), not the model's, and the broker still refuses
//! whatever is off.

use std::collections::BTreeSet;

use hatchling_core::run_tools::ToolGroup;

/// The note for a run told `run_tools` are on (`None`: every group, as an
/// older broker says nothing).
#[must_use]
pub fn note(run_tools: Option<&BTreeSet<ToolGroup>>) -> String {
    let on = |group| run_tools.is_none_or(|on| on.contains(&group));
    let mut lines = vec![
        String::from("## What you can do on this install"),
        String::from(
            "This is generated from the install's live settings. Trust it over anything you assume.",
        ),
        String::from(
            "- You run in an isolated cell: you can't run programs or commands on the owner's machine, or read or change their files. Don't offer to; say what they can run themselves instead.",
        ),
        String::from(
            "- Say you did something, or asked the owner for something, only when a tool told you so in this turn. What earlier messages in the conversation say you did is history, not this turn: to do it now, call the tool now.",
        ),
    ];
    lines.push(String::from(if on(ToolGroup::Web) {
        "- You can read a web page the owner allows: they're asked for each new site."
    } else {
        "- Reading web pages is off on this install. If a task needs it, say so, and that the owner can turn it on."
    }));
    lines.push(String::from(if on(ToolGroup::Search) {
        "- You can search the web."
    } else {
        "- Web search is off on this install. Don't offer to look things up online."
    }));
    if on(ToolGroup::Browser) {
        lines.push(String::from(
            "- You can drive a browser the owner can watch and take over.",
        ));
    }
    lines.push(String::from(if on(ToolGroup::Goals) {
        "- You can read the owner's open goals with goals_read: when what they ask touches what they're working toward, read them rather than guess. They set and close goals in the app (Goals); you can't change them."
    } else {
        "- You can't see the owner's goals here. If they mention one, take it from what they say."
    }));
    lines.push(String::from(if on(ToolGroup::Memory) {
        "- When the owner tells you something about themselves worth keeping, ask to remember it with memory_propose. They approve it in the app; you can't keep anything yourself, and anything already kept is given to you as recalled memory."
    } else {
        "- You can't keep anything from this conversation for later chats here."
    }));
    if on(ToolGroup::Skills) {
        lines.push(String::from(
            "- You can run the installed skills your tools name, within their grants.",
        ));
    }
    if on(ToolGroup::Suggest) {
        lines.push(String::from(
            "- When the owner asks for ideas, or you see something Hatchling could do for them that they haven't asked for, you can suggest it with idea_suggest. It waits on their Ideas page until they start it; nothing runs from it. Don't suggest what they just asked you to do: do it, or say you can't.",
        ));
    }
    lines.push(String::from(if on(ToolGroup::Schedule) {
        "- When the owner wants a reminder, or something done later or again, ask to schedule it with schedule_propose. They approve it in the app; you can't schedule anything yourself."
    } else {
        "- You can't set up scheduled work or reminders yourself. The owner can, in the app (Schedule, then Add scheduled work) or with `hatchling-broker schedule add`; offer to help them word it."
    }));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_says_what_is_off_and_never_offers_it() {
        let none = BTreeSet::new();
        let off = note(Some(&none));
        assert!(off.contains("Reading web pages is off"), "{off}");
        assert!(off.contains("Web search is off"), "{off}");
        assert!(!off.contains("drive a browser"), "{off}");
        assert!(!off.contains("installed skills"), "{off}");
        assert!(off.contains("can't run programs or commands"), "{off}");
        assert!(
            off.contains("only when a tool told you so in this turn"),
            "{off}"
        );
        assert!(off.contains("hatchling-broker schedule add"), "{off}");
        assert!(off.contains("can't see the owner's goals"), "{off}");
        assert!(off.contains("can't keep anything"), "{off}");
        let schedule = note(Some(&BTreeSet::from([ToolGroup::Schedule])));
        assert!(
            schedule.contains("ask to schedule it with schedule_propose"),
            "{schedule}"
        );
        assert!(
            !schedule.contains("hatchling-broker schedule add"),
            "{schedule}"
        );
        let memory = note(Some(&BTreeSet::from([ToolGroup::Memory])));
        assert!(
            memory.contains("ask to remember it with memory_propose"),
            "{memory}"
        );
        let suggest = note(Some(&BTreeSet::from([ToolGroup::Suggest])));
        assert!(
            suggest.contains("suggest it with idea_suggest"),
            "{suggest}"
        );
        assert!(!off.contains("idea_suggest"), "{off}");
        let goals = note(Some(&BTreeSet::from([ToolGroup::Goals])));
        assert!(
            goals.contains("read the owner's open goals with goals_read"),
            "{goals}"
        );
        assert!(!goals.contains("can't see the owner's goals"), "{goals}");
        let web = BTreeSet::from([ToolGroup::Web]);
        let some = note(Some(&web));
        assert!(some.contains("read a web page the owner allows"), "{some}");
        assert!(some.contains("Web search is off"), "{some}");
        // An older broker says nothing: every group, as the agent offers.
        let every = note(None);
        assert!(every.contains("You can search the web."), "{every}");
        assert!(every.contains("drive a browser"), "{every}");
        assert!(every.contains("installed skills"), "{every}");
        assert!(!every.contains(" is off"), "{every}");
    }
}
