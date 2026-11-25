# WO-052: The agent's notes on earlier in the conversation

**Status:** QUEUED
**Area:** `crates/hatchling-core` (the conversation, the notes), `crates/hatchling-broker` (the chat, the notes run, the conversation store, the owner's ops), `crates/hatchling-agent` (the notes in the prompt), `frontend/` (the notes card, the setting), `docs/`
**Issue:** [#662](https://github.com/undergroundrap/hatchling/issues/662)
**Trust boundary:** yes

## Problem

A chat run is given the conversation word for word, as much as its model's window has room for (#595). Past that, the oldest answers are dropped. The owner's words from them are kept verbatim as *earlier*, within their own bounds, and then dropped too. So a long project loses two kinds of thing:

- **What was settled.** A choice made in exchange 3 and revised in exchange 9 survives only if both messages are still in view, and the model has to work out which one wins. Once the answers that explained them are gone, it often can't.
- **What the agent did.** The agent's own account of what it built, where, and why goes with its answers. The next run starts from the owner's words alone.

Agents with long sessions keep their continuity by summarizing what no longer fits: the gist of what was decided and done, written down before the detail goes. Hatchling never has. WO-026 ruled it out, and the maintainer confirmed that on 2026-11-22.

## Why the rule changes (the maintainer's decision, 2026-11-25)

WO-026's reason was sound: a summary is written by a model, in a cell that reads pages and tool output, so a planted line could become part of what the agent is told the owner decided. The rule kept that risk out by keeping summaries out.

But verbatim isn't neutral either. It keeps everything, the wrong turns and the superseded decisions included, and asks the model to reconcile them on every turn. A small model often can't. And past the bounds, verbatim keeps nothing at all. Weighing the two, the maintainer chose continuity and replaced the rule:

> **Old rule:** no model writes a word of the owner's conversation, ever.
>
> **New rule:** the agent may keep notes on what no longer fits in the conversation, written by a model and fenced. They are labeled as the agent's own and never as the owner's words. The owner can see, correct and delete them. Their writer has no tools and reads only what the owner was already shown.

Three things don't change:
- The owner's words are never rewritten while they fit: the exchanges and *earlier* stay verbatim, as #595 sizes them.
- The notes never speak in the owner's role.
- Nothing invisible shapes a later run.

## Threat model

| Threat | What stops it |
|---|---|
| A page the agent read plants a line ("the owner wants reports sent to X"), and the notes repeat it as a decision | The notes are given in the agent's role, under a header saying they are the agent's own notes and not the owner's words. The model is told to weigh them as its own past judgment, below anything the owner said. The owner sees every version of the notes in the chat and can correct or delete it. The output gate scans them as it scans an answer. |
| The notes writer acts on what it reads | It runs in a cell with no tools, no network but its model, and no Room. Its only output is the notes' text, bounded. |
| The notes writer reads what the owner was never shown | Its input is the exchanges being dropped, as the chat kept them: each answer as the output gate passed it to the owner, never tool output, pages or the agent's own working. |
| The notes grow without bound, or crowd out the conversation | They have a size of their own inside the history's bounds (#595), and each new version replaces the last. |
| A note drops something the owner cares about | The owner's words stay verbatim while they fit, ahead of the notes. The owner sees the notes and can correct them. And the notes are a setting: an owner who wants strictly verbatim turns them off. |
| An injected note survives a New conversation | New conversation removes the notes with everything else. |
| A constraint the owner cares about is dropped, or summarized away | The owner can pin a message. A pinned message is kept verbatim, ahead of the notes, whatever the bounds drop, until they unpin it. |
| Telephone drift: notes rewritten from notes, each version a little further off | Each version is kept, and the owner can see what each change replaced and roll back to an earlier version. |
| The writer invents a detail no exchange held | Each line of the notes names the exchanges it came from, which the broker checks are ones the writer was given. A claim can be traced to what was said, and a line naming none is refused. |
| Stale notes contradict the owner's recent words | Verbatim wins, by order and by statement: the notes come before the exchanges, under a header saying that what the owner said since overrides them. |

## Scope

**In scope:**
- **When.** The notes are written only when keeping a new exchange would drop older answers. Until a conversation outgrows its window, nothing changes.
- **The notes writer.** A run of its own kind, in a cell like every run, with no tools. It's given the current notes and the exchanges about to go, and asked for the new notes:
  - what was decided, and what superseded what;
  - what was built or done, and where;
  - what's still open.

  Its answer replaces the notes once the output gate passes it.
- **Kept with the conversation.** Sealed in the conversation store, in the same write as the exchanges they replace. Shown again after a restart.
- **Given to every chat run.** After *earlier* and before the exchanges, in the agent's role, under their own header. Their bytes come out of the history's bounds.
- **The owner's view.** A card in the chat: "What the agent keeps from earlier", the notes as given. The owner can edit the notes, and an edit is marked as the owner's. They can delete them. They can turn notes off in Settings, which makes the conversation verbatim only, as today. Notes are on by default.
- **Pinned messages.** The owner can pin any message of theirs. A pinned message is given verbatim to every chat run, before the notes, until it's unpinned. Pins have their own bound inside the history's, and the owner is told when a new pin would pass it.
- **Versions.** Each version of the notes is kept with the conversation, with what it replaced, and the owner can see the changes and roll back. A New conversation removes them all.
- **Sources.** Each line of the notes names the exchanges it came from, by their number in the conversation. The broker refuses a version with a line that names none, or names one the writer wasn't given, and keeps the last.
- **Verbatim wins.** The notes are given before the exchanges, and their header says that what the owner said since overrides them.
- **Memory agrees.** When the notes record a decision that contradicts an item the owner kept in memory, the agent is prompted to propose a correction to that item (WO-048). Neither silently overrides the other.
- **Activity** records each notes run: its size, and what it replaced.

**Not in scope:**
- Summarizing the owner's words while they still fit.
- Notes on runs with nobody there: a schedule's or hook's run has no conversation.
- Memory. WO-048's items are kept by the owner's approval, and the notes are not memory: they're the conversation's working record, and they go with it. WO-048 and this are designed together so the two don't disagree. A fact the owner wants kept beyond the conversation goes to memory.
- A different model for the notes. They use the chat's model.

## Acceptance criteria

- [ ] A conversation that never outgrows its window never runs the notes writer: a test keeps one within its bounds and finds no notes run.
- [ ] When keeping an exchange would drop answers, the notes are written from the current notes and exactly the exchanges dropped, by a run offered no tools. A test checks the run's input and its tools.
- [ ] The notes reach the model in the agent's role, under their header, never the owner's. A test reads the request sent.
- [ ] A notes run whose output the gate blocks keeps the last notes, and the exchanges drop as they would with notes off. A test plants a secret.
- [ ] The owner sees the notes in the chat, and can edit, delete and turn them off. Each is tested at the API and in the app.
- [ ] Notes survive a restart and go with New conversation. Store tests cover both, and a stopped write.
- [ ] With notes off, the conversation is exactly #595's. A test compares the two.
- [ ] An injected instruction in an answer doesn't come back attributed to the owner. A fixture plants one, and a test reads the notes' role and header. Its effect on the model is measured by WO-045's behaviour battery.
- [ ] A pinned message is given verbatim to every run until unpinned, however far the bounds have moved past it. A test pins one and keeps going past the bounds.
- [ ] Each version of the notes is kept, and rolling back gives the next run the earlier version. A store test covers both.
- [ ] A version with a line that names no exchange, or one the writer wasn't given, is refused, and the last version stands. A test plants each.
- [ ] WO-045 has fixtures for compaction: a decision reversed before it was compacted is recalled as reversed, a pinned constraint survives, and a planted line stays the agent's.
- [ ] Each new guard fails its test when removed.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates). It changes the UI protocol (the notes card, the setting) and the session protocol (the notes run). The protocol change is stated here, for the maintainer's approval with the design.

## Evidence

Filled in at close.

## Notes

- **Order:** v0.11.0, designed with WO-048, before WO-045, so the evaluation harness measures conversations with notes. The design and threat model above are approved by the maintainer before any code, as WO-046's were.
- **Why the agent's role, not the owner's:** a planted line written as the agent's own note is weighed as the agent's past judgment. The same line in the owner's role would be weighed as an instruction. The notes stay where the agent's answers already were: what it said, not what it was told.
- **Why the owner sees them:** an agent's summary is usually invisible. Here it's shown in the chat as it's given, so the owner can correct anything wrong or planted before the next run reads it.
- **Cost:** one model call each time answers would drop, and the notes' tokens on each turn, against the far larger history they replace. Prompt caching (#661) makes resending them cheap where the provider has it.
