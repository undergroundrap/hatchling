# WO-057 slice 1: the conversation's taint, the mode, and keeping at once

Part of [#767](https://github.com/undergroundrap/hatchling/issues/767), [WO-057](../workorders/workorder-057-automatic-memory.md).

## What changed

- **The broker decides, from what it served.**
  - `conversation_taint` holds each chat run, by its cell, with whether it's tainted.
  - The chat registers each run it starts, tainted when its conversation is. The run's session marks it the moment it asks for a tool that reads from outside (`outside`), before the tool is served, however it ends.
  - A cell not registered is never clean.
- **The chat holds the conversation's flag** (`Chat::tainted`):
  - A run's mark is taken when it ends, answered or failed.
  - Files given at hand-over taint the conversation at once.
  - A new conversation clears the flag.
  - The conversation store's index keeps it (`tainted`, `#[serde(default)]`). Each keep carries it, a new conversation clears it, and a chat resumes tainted when the store says so or its mark can't be read (`resumes_tainted`).
- **What's outside:**
  - Web search and pages, the browser, reading messages, a network request, the agent's computer, a skill, a subagent, a shared space and an artifact.
  - Recall and goals reads count too, where runs may write memory or goals. The install grants runs neither.
- **The mode** (`memory_mode`):
  - `memory-mode.json` in the owner directory. Absent is Automatic.
  - One that doesn't read or can't be opened is Ask me, and so is a broker without scheduling.
- **Keeping at once** (`memory_keep::propose_in`), when all of these hold:
  - the broker says the conversation is clean and the run is the chat's own;
  - the mode is Automatic;
  - the owner never denied or skipped those words (`Board::refuses_memory`, the ledger's own fingerprints);
  - fewer than 20 were kept automatically since a day ago, counting those replaced since.

  Then:
  - the record is `KeptRecord::automatic`: `kept_by: automatic`, with the run, the chat and the time, and no approval;
  - the owner is told in the chat, "Remembered: … Kept automatically: nothing in this conversation came from outside. If it's wrong, forget it on the Memory page.";
  - the agent is answered `MemoryResult::Kept { id }` (session v40).

  Otherwise it's the card, as before.
- **Recall says so:** "kept 2026-11-26 automatically from what the owner said, not approved by them". The index line says "kept 2026-11-26 automatically". `approved_by_owner()` is false for it, and `kept()` is true for both.
- **The agent:**
  - `memory_propose` is answered `{"kept": true, "id": …, "note": "Kept at once: … Tell them you'll remember it, not that you asked."}`.
  - Its description says Hatchling decides whether a memory is kept at once or put on a card, and to say it'll remember only when it says kept.

## Tests

| Rule | Test |
| --- | --- |
| A clean chat proposal is kept at once, recorded as automatic, the owner told, no card | `a_clean_chat_proposal_is_kept_at_once_and_the_owner_told` |
| Not clean, Ask me, or no scheduling: a card | `a_conversation_that_read_from_outside_or_ask_me_puts_it_on_a_card` |
| The cap: 20 a day, replaced ones counted, older or approved ones not; its first second | `past_the_days_cap_it_is_a_card_and_replaced_ones_count`, `the_days_count_starts_after_its_first_second` |
| A denial holds without a card | `what_the_owner_said_no_to_or_skipped_is_never_kept_at_once` |
| A correction kept at once retires what it corrects | `a_correction_kept_at_once_retires_what_it_corrects` |
| A run's outside read taints the conversation through later runs; a new one is clean | `a_run_that_read_from_outside_taints_its_conversation_until_a_new_one` |
| A failed run taints; a conversation the owner left isn't marked | `a_failed_run_taints_and_a_left_conversation_isnt_marked` |
| Files sent in taint at hand-over | `files_sent_in_taint_the_conversation_they_are_given_in` |
| A tainted conversation stays so across a restart | `a_conversation_tainted_before_a_restart_stays_tainted`, `the_chat_marks_its_conversation_and_resumes_as_it_was`, `the_taint_is_kept_with_the_conversation_and_a_new_one_clears_it` |
| The table: unknown is tainted, a mark is the run's own | `conversation_taint::tests` (5) |
| What's outside, and recall or goals where runs write them | `what_reads_from_outside_is_named_and_the_owners_own_is_not`, `what_runs_may_write_is_outside_when_read` |
| The mode: default, kept, unreadable, unopenable, no scheduling | `memory_mode::tests` (5) |
| **End to end, a real session:** clean keeps; a web search first, even refused, is a card; a run the chat didn't start is a card | `a_clean_chat_run_keeps_at_once_and_one_that_read_from_outside_asks` |
| The record and recall's words | `a_record_says_it_was_kept_automatically_and_older_ones_read_as_the_owners`, `a_memory_kept_automatically_is_recalled_as_not_approved` |
| The wire, and the agent told kept | `a_memory_kept_at_once_is_said_with_its_id`, `memory_propose_asks_the_owner_and_says_what_became_of_it` |

## Hand-breaks

Each rule broken alone in the sandbox, and its crate's tests run. Each one failed a test.

| Break | Failed |
| --- | --- |
| Nothing reads from outside | 2 |
| A run not held is clean | 1 |
| A read isn't marked | 4 |
| What runs write to memory isn't outside | 1 |
| Tracking a run writes isn't outside | 1 |
| The session marks nothing | 1 |
| An unregistered run is clean | 1 |
| A run's mark isn't taken | 2 |
| A new conversation stays tainted | 2 |
| Files don't taint | 1 |
| A tainted conversation's run starts clean | 2 |
| The mark isn't kept | 3 |
| A keep drops the mark | 1 |
| A left conversation is marked | 1 |
| A denial isn't checked | 1 |
| A replaced one isn't counted | 1 |
| The mode is ignored | 1 |
| The cap lets one more | 1 |
| The owner isn't told | 1 |
| The record says the owner | 2 |
| An oversize mode reads | 1 |
| An unreadable mode is automatic | 1, once `a_choice_that_cant_be_opened_asks` was added: at first none, since a directory where the file should be opens on Linux and fails only at its read |
| Recall says the owner approved it | 1 |
| The index doesn't say automatic | 1 |
| Automatic counts as approved | 1 |
| The agent is told it asked | 1 |

## Gates

In the WSL2 sandbox, on the pinned toolchain unless named, with `HATCHLING_REQUIRE_CELL=1`:

- **The workspace's tests:** 4731 passed, 0 failed.
- **Clippy, `-D warnings`, all targets and features:** clean on 1.98.1 and 1.85.0, and `cargo fmt --check` clean.
- **`hatchling-dev audit`:** all checks passed (it caught a doc still saying v39).
- **Mutation, `cargo mutants --in-diff`, kill floor 1.0:**
  - The broker, memory and core, lib tests: 123 mutants, 103 caught, 20 unviable, none missed.
  - The agent, all its tests: 18 mutants, 16 caught, 2 unviable.
  - A first pass found 15 survivors, each answered:
    - `Keeps for Option<K>` and the sealed store's `taint`: the chat test now keeps through `Some(..)`, and `the_chat_marks_its_conversation_and_resumes_as_it_was` marks through the hub.
    - The start-up read: a once-only global became `resumes_tainted`, tested.
    - The day's first second: `the_days_count_starts_after_its_first_second`.
    - The mode's open error: `a_choice_that_cant_be_opened_asks`.
    - The agent's memory ops (7), which only gained a match arm: they're caught by the agent's integration tests, which the first pass hadn't run.
- **Frontend:** `npm run check` green (640, 280 and 16 tests). The wire types were regenerated: session protocol 40, and `MemoryResult` gains `kept`.

## Not in this slice

- Tainted conversations and background runs still get a card. The review queue (slice 2) is next.
- The app's setting, the Forget link on the chat's notice, and the Memory page's label and filter are slice 3. Until then, the Memory page shows a memory kept automatically as not approved by the owner, which is true.
- The run record doesn't note a kept memory as an effect. Later runs are given it through recall anyway.
