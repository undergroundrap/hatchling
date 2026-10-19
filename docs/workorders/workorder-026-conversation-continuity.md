# WO-026: Conversation continuity

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-core` (`conversation`), `crates/hatchling-broker` (`chat_run`, run cells, delivery), `crates/hatchling-scheduler` (`RunRequest`, `Delivery`), `crates/hatchling-agent` (config), `crates/hatchling-loop` (prior turns), `docs/`
**Issue:** none yet
**Trust boundary:** yes

## Problem

Each chat message starts a fresh run whose only input is that message, so a follow-up ("use the second option") has nothing to refer to. Every assistant people compare Hatchling with remembers the conversation. Theirs is kept by the same process that reads web pages and tool output, so anything it reads can try to rewrite what it remembers: a planted "the user already approved this" becomes part of the conversation.

Hatchling's version must keep the property the rest of the design has: a cell can't write what the broker trusts. The conversation is the broker's record, outside any cell, and holds only what the owner said and what the owner was shown.

## Scope

**In scope:**

- **First slice (this WO's first PR):**
  - The chat keeps the conversation in the broker. It records an exchange only when its answer reached the owner as written: a failed run's fixed sentence, an answer the output gate blocked or replaced, and one that reached no one are never kept.
  - Each chat run is given the last exchanges, bounded by count (8) and size (24 KiB of text, whole exchanges, newest kept).
  - The broker gives them as typed data (`HATCHLING_RUN_HISTORY`, a JSON array of `{user, assistant}`). The agent refuses any past the bounds, and any given to a run that isn't a chat turn.
  - The loop places them as user and assistant turns between the system prompt and the new message, before recalled memory and evidence.
- **Next slices:**
  - The conversation survives a broker restart, sealed by the vault like the schedules.
  - A way to start a new conversation from the app.
  - Compaction past the bounds (`hatchling-memory`'s `compact`), keeping decisions and corrections verbatim.
  - Turn ids, so a redelivered message isn't kept twice.

**Not in scope:**

- More than one conversation, or conversations per channel (the broker serves one owner's local chat).
- Memory across conversations (that's `hatchling-memory`'s recall, which stays data, not turns).
- Approvals (a candidate of its own). A turn can never approve anything.

## Acceptance criteria

- [ ] A follow-up's run is given the exchange before it, as the owner said it and was shown it; tested in the chat loop and end to end with real cells.
- [ ] A failed run, a replaced answer and an undelivered one are never given back; tested.
- [ ] What a run is given stays within the bounds, newest kept, whole exchanges only; tested at the bound and one past it.
- [ ] The agent refuses history past the bounds, malformed, or given to a run that isn't a chat turn; tested.
- [ ] The model sees the prior exchanges in their own roles, before the new message; tested in the loop.
- [ ] A cell has no way to add to the conversation: no request writes it, and what it writes elsewhere (a note, a memory) never comes back as a turn.
- [ ] Later slices: the conversation survives a restart, sealed; a correction made two turns back survives a new cell and a compaction.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- **Why the environment, not a request over the cell's connection.** The broker writes it once, at launch, before any of the cell's code runs. The agent reads it as typed data. There is no request a cell could send to change it, and nothing to keep in sync. A run that isn't a chat turn is never given one, and the agent refuses one if it is.
- **What the model is told.** Prior assistant turns are the model's own earlier words, as the owner saw them. They can echo text that came from a tool or a page, but only text the output gate let reach the owner. They carry no authority: an approval is never a turn.
- **Kept in memory for now.** A broker restart starts a new conversation. Persisting it means a sealed store like the schedules', in the next slice. Until then the transcript never touches disk.
