# WO-026: Conversation continuity

**Status:** DONE (2026-11-01)
**Area:** `crates/hatchling-core` (`conversation`), `crates/hatchling-broker` (`chat_run`, run cells, delivery), `crates/hatchling-scheduler` (`RunRequest`, `Delivery`), `crates/hatchling-agent` (config), `crates/hatchling-loop` (prior turns), `docs/`
**Issue:** none yet
**Trust boundary:** yes

## Problem

Each chat message starts a fresh run whose only input is that message, so a follow-up ("use the second option") has nothing to refer to. Every assistant people compare Hatchling with remembers the conversation. Theirs is kept by the same process that reads web pages and tool output, so anything it reads can try to rewrite what it remembers: a planted "the user already approved this" becomes part of the conversation.

Hatchling's version must keep the property the rest of the design has: a cell can't write what the broker trusts. The conversation is the broker's record, outside any cell, and holds only what the owner said and what the owner was shown.

## Scope

**In scope:**

- **First slice (#133):**
  - The chat keeps the conversation in the broker. It records an exchange only when its answer reached the owner as written: a failed run's fixed sentence, an answer the output gate blocked or replaced, and one that reached no one are never kept.
  - Each chat run is given the last exchanges, bounded by count (8) and size (24 KiB as sent, escaped as JSON; whole exchanges, newest kept).
  - The broker gives them as typed data (`HATCHLING_RUN_HISTORY`, a JSON array of `{user, assistant}`). The agent refuses any past the bounds, and any given to a run that isn't a chat turn.
  - The loop places them as user and assistant turns between the system prompt and the new message, before recalled memory and evidence.
- **Next slices:**
  - A way to start a new conversation from the app. *Done in part 2 (#258).*
  - The conversation survives a broker restart, sealed by the vault like the schedules, and the app shows it after one. *Part 3 (#259).*
  - Compaction past the bounds, keeping decisions and corrections verbatim. *Part 4 (#262):* the owner's own messages are kept, and the answers go (see the notes).
  - ~~Turn ids, so a redelivered message isn't kept twice.~~ Dropped: nothing redelivers a message. The app sends each under its own request id, so a retry on one connection is answered once. When no answer comes, it tells the owner the message is unconfirmed and never sends it again itself. A message the owner sends twice is theirs to send twice. This comes back if a channel that redelivers ever feeds the chat.

**Not in scope:**

- More than one conversation, or conversations per channel (the broker serves one owner's local chat).
- Memory across conversations (that's `hatchling-memory`'s recall, which stays data, not turns).
- Approvals (a candidate of its own). A turn can never approve anything.

## Acceptance criteria

- [x] A follow-up's run is given the exchange before it, as the owner said it and was shown it; tested in the chat loop and end to end with real cells.
- [x] A failed run, a replaced answer and an undelivered one are never given back; tested.
- [x] What a run is given stays within the bounds, newest kept, whole exchanges only; tested at the bound and one past it.
- [x] The agent refuses history past the bounds, malformed, or given to a run that isn't a chat turn; tested.
- [x] The model sees the prior exchanges in their own roles, before the new message; tested in the loop.
- [x] A cell has no way to add to the conversation: no request writes it, and what it writes elsewhere (a note, a memory) never comes back as a turn.
- [x] Later slices: the owner starts a new conversation from the app, and the next run is given nothing said before it (part 2); the conversation survives a restart, sealed, and the app shows what the agent is still given (part 3); a correction made two turns back survives a new cell and a compaction, and what the owner said past the bounds is still given, verbatim (part 4).

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Built in #133 (part 1), #258 (part 2: a new conversation), #259 (part 3: kept across a restart, sealed) and #262 (part 4: the owner's words past the bounds). Turn ids were dropped; the scope says why.

Planning part 4 found a real bug, fixed in #261 with a test that failed first. The history's 24 KiB bound was measured on the raw text, but a run is given it as JSON, where a control character takes 6 bytes. An answer the model was steered into filling with control characters was kept, and the agent then refused every later history. Every chat run failed until New conversation, and with a store, across restarts too.

Each part was gated in the sandbox on real cells, with a delegated cgroup:
- fmt and clippy on 1.98.1 and 1.85.0;
- preflight;
- the tests with `REQUIRE_CELL=1`;
- a hand mutant for each claim, each read for failing at its intended check. #133 also ran cargo-mutants on its changed lines: 0 missed, 0 timeouts;
- then CI, whose browser job (`scripts/conversation_end_to_end.sh`) drives the app against a real broker, restarting it.

- **A follow-up is given the exchange before it:**
  - in the chat loop (`a_follow_ups_run_is_given_the_exchange_before_it`, `crates/hatchling-broker/src/chat_run.rs`);
  - end to end: a chat's second run, a fresh cell, is given the first exchange as recorded (`a_chats_next_cell_is_given_the_exchange_before_it`, `crates/hatchling-broker/tests/schedule_run.rs`);
  - in the app, by CI's browser job, where a scripted model answers with the turns it was given.
- **Never given back** (`only_an_answer_the_person_was_shown_is_given_back`): a failed run, one that never started, an answer the output gate replaced, and one that reached no one. With a store, none of them is kept on disk either (`a_resumed_chat_goes_on_and_keeps_each_exchange_beyond_memory`).
- **The bounds** (`crates/hatchling-core/src/conversation.rs`):
  - the newest whole exchanges that fit, exactly at the bound and one byte past it (`a_run_gets_the_newest_whole_exchanges_that_fit`);
  - measured as sent: a side's size agrees with `serde_json` for every character below U+0080 and some beyond (`a_side_is_measured_as_json_writes_it`);
  - the chat keeps no more than a run is given (`the_chat_keeps_no_more_than_a_run_is_given`), and the store neither (`only_what_a_run_is_given_is_kept`).
- **The agent refuses:**
  - history past the bounds, or malformed (`a_run_reads_back_only_what_a_broker_could_have_sent`);
  - history for a run that isn't a chat turn (`only_a_chat_turn_reads_a_conversation`, `crates/hatchling-agent/src/config.rs`). The broker sets it only for one (`only_a_chat_turn_carries_its_history`, `crates/hatchling-broker/src/cell.rs`).
- **The model's view:** the prior exchanges in their own roles, before the new message (`prior_turns_come_before_the_request_in_their_own_roles`, `crates/hatchling-loop/src/runner.rs`), through the agent's own loop (`a_chat_turns_loop_gives_the_model_its_conversation`, `crates/hatchling-agent/src/cell.rs`).
- **A cell can't add to it.** The conversation has two writers, both in the broker:
  - the chat, which keeps an exchange after its answer reached the owner as written;
  - the owner's New conversation (`crates/hatchling-broker/src/api_ops.rs`), which clears it.

  The chat's record is its own. The store is opened only by the chat, and by the API to show it at the start and to clear it. No request a cell sends reaches either. What a cell writes to memory comes back as recalled memory, a labeled block of data after the message, never a turn (the same loop test, and `adversarial_memory_is_inert_at_loop_level`).
- **Part 2:**
  - the next run is given nothing said before New conversation (`a_new_conversation_is_given_nothing_said_before_it`);
  - a fresh cell after one is given no history (the real-cell test's third message);
  - the browser job.
- **Part 3** (`crates/hatchling-broker/src/conversation_store.rs`):
  - resumed as it was (`the_conversation_kept_is_resumed_as_it_was`);
  - one that can't be read, under another key or changed, starts a new one (`a_conversation_that_cant_be_read_starts_a_new_one`), and a crash's half-written sides are never resumed (`sides_written_without_their_index_are_never_resumed`);
  - New conversation removes it all (`a_new_conversation_removes_everything_kept`);
  - the app is shown what the agent is still given (`the_app_is_shown_what_was_kept`);
  - the browser job, across two broker restarts.
- **Part 4:**
  - Each run is a new cell, given up to 8 exchanges, so a correction two turns back reaches the next one.
  - Past the bounds, what the owner said stays, word for word (`past_the_bounds_an_exchange_leaves_what_the_owner_said`, `the_chat_keeps_no_more_than_a_run_is_given`).
  - The model sees it labeled as Hatchling's record, in the owner's role, before the exchanges (`the_model_is_shown_what_was_said_earlier_quoted_and_labeled`, `what_was_said_earlier_comes_before_the_prior_turns`).
  - End to end, a chat's cell is given it, typed, beside the exchange since (`a_chats_cell_is_given_what_the_owner_said_earlier`).

## Notes

- **Why the environment, not a request over the cell's connection.** The broker writes it once, at launch, before any of the cell's code runs. The agent reads it as typed data. There is no request a cell could send to change it, and nothing to keep in sync. A run that isn't a chat turn is never given one, and the agent refuses one if it is.
- **What the model is told.** Prior assistant turns are the model's own earlier words, as the owner saw them. They can echo text that came from a tool or a page, but only text the output gate let reach the owner. They carry no authority: an approval is never a turn.
- **Kept on disk only when the operator says (part 3).** With `HATCHLING_CONVERSATION_DIR` set, the conversation is sealed in a store of its own (`conversation`), under its own key, which the keeper hands the broker like the schedules'. Without it, a broker restart starts a new conversation, and the transcript never touches disk.
- **What's on disk is what a run is given:** the exchanges within the bounds, nothing more. Each side is an entry of its own, raw, so a side never needs more than one sealed entry. An index names the exchanges; their sides live in nine slots, one more than a conversation holds, so a new exchange never overwrites one still kept, and a keep the broker didn't finish leaves its sides in the one slot the index says is next, which the next start removes.
- **A new conversation is durable when the owner is told.** The API removes every slot, whatever the index says, and only then moves the hub on, with the store locked. The chat keeps an exchange only if, with the store locked, the hub's conversation is still the one its message was sent in.
- **After a restart, the owner sees what the agent is given.** The store is read once, at the broker's start: the chat resumes it, and the app is shown each exchange under Hatchling's notice. One that can't be read (another key, a changed file) is removed, the broker's log says why, and the app says a new conversation started.
- **Compaction keeps the owner's words, not a summary (part 4).** A summary would be written by a model, in a cell that reads pages and tool output, so a planted line could become part of what the agent is told the owner decided. Past the bounds, an exchange's answer goes, and what the owner said in it is kept verbatim: up to 32 messages and 8 KiB as sent, newest kept, whole messages. A decision or a correction the owner made is their own message, so it survives word for word, and nothing is classified or paraphrased. A run is given them typed (`HATCHLING_RUN_EARLIER`, a JSON array of strings), which the agent refuses past the bounds or for a run that isn't a chat turn. The model sees them as one message in the owner's role, labeled as Hatchling's record, before the exchanges. With a store, they're kept in the index, which is one write, so they change with the exchanges they were said in. After a restart the app shows them under a notice of their own, saying the agent no longer has its answers to them.
