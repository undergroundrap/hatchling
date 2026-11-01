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
  - Each chat run is given the last exchanges, bounded by count (8) and size (24 KiB as sent, escaped as JSON; whole exchanges, newest kept).
  - The broker gives them as typed data (`HATCHLING_RUN_HISTORY`, a JSON array of `{user, assistant}`). The agent refuses any past the bounds, and any given to a run that isn't a chat turn.
  - The loop places them as user and assistant turns between the system prompt and the new message, before recalled memory and evidence.
- **Next slices:**
  - A way to start a new conversation from the app. *Done in part 2 (#258).*
  - The conversation survives a broker restart, sealed by the vault like the schedules, and the app shows it after one. *Part 3 (#259).*
  - Compaction past the bounds, keeping decisions and corrections verbatim. *Part 4:* the owner's own messages are kept, and the answers go (see the notes).
  - ~~Turn ids, so a redelivered message isn't kept twice.~~ Dropped: nothing redelivers a message. The app sends each under its own request id, so a retry on one connection is answered once. When no answer comes, it tells the owner the message is unconfirmed and never sends it again itself. A message the owner sends twice is theirs to send twice. This comes back if a channel that redelivers ever feeds the chat.

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
- [ ] Later slices: the owner starts a new conversation from the app, and the next run is given nothing said before it (part 2); the conversation survives a restart, sealed, and the app shows what the agent is still given (part 3); a correction made two turns back survives a new cell and a compaction, and what the owner said past the bounds is still given, verbatim (part 4).

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- **Why the environment, not a request over the cell's connection.** The broker writes it once, at launch, before any of the cell's code runs. The agent reads it as typed data. There is no request a cell could send to change it, and nothing to keep in sync. A run that isn't a chat turn is never given one, and the agent refuses one if it is.
- **What the model is told.** Prior assistant turns are the model's own earlier words, as the owner saw them. They can echo text that came from a tool or a page, but only text the output gate let reach the owner. They carry no authority: an approval is never a turn.
- **Kept on disk only when the operator says (part 3).** With `HATCHLING_CONVERSATION_DIR` set, the conversation is sealed in a store of its own (`conversation`), under its own key, which the keeper hands the broker like the schedules'. Without it, a broker restart starts a new conversation, and the transcript never touches disk.
- **What's on disk is what a run is given:** the exchanges within the bounds, nothing more. Each side is an entry of its own, raw, so a side never needs more than one sealed entry. An index names the exchanges; their sides live in nine slots, one more than a conversation holds, so a new exchange never overwrites one still kept, and a keep the broker didn't finish leaves its sides in the one slot the index says is next, which the next start removes.
- **A new conversation is durable when the owner is told.** The API removes every slot, whatever the index says, and only then moves the hub on, with the store locked. The chat keeps an exchange only if, with the store locked, the hub's conversation is still the one its message was sent in.
- **After a restart, the owner sees what the agent is given.** The store is read once, at the broker's start: the chat resumes it, and the app is shown each exchange under Hatchling's notice. One that can't be read (another key, a changed file) is removed, the broker's log says why, and the app says a new conversation started.
- **Compaction keeps the owner's words, not a summary (part 4).** A summary would be written by a model, in a cell that reads pages and tool output, so a planted line could become part of what the agent is told the owner decided. Past the bounds, an exchange's answer goes, and what the owner said in it is kept verbatim: up to 32 messages and 8 KiB as sent, newest kept, whole messages. A decision or a correction the owner made is their own message, so it survives word for word, and nothing is classified or paraphrased. A run is given them typed (`HATCHLING_RUN_EARLIER`, a JSON array of strings), which the agent refuses past the bounds or for a run that isn't a chat turn. The model sees them as one message in the owner's role, labeled as Hatchling's record, before the exchanges. With a store, they're kept in the index, which is one write, so they change with the exchanges they were said in. After a restart the app shows them under a notice of their own, saying the agent no longer has its answers to them.
