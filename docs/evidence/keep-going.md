# Evidence: Keep going, and a run that knows what the one before it did (WO-053, slice 2a)

| | |
|---|---|
| Base | `main` at `59f83802` (#709) |
| Date | 2026-11-26 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0; the frontend type-checked on Windows (Node 22) |
| Design | [WO-053](../workorders/workorder-053-the-run-record.md), approved (#703) and amended (#707) |

## What it does

**A run that stops before it's done hands its record to the next run.** The chat takes each run's record (slice 1) as the run ends, and keeps it with the exchange as before. A run can stop before it's done:
- out of steps, time or tokens;
- stopped by the owner;
- failed.

When it does, the chat also carries the record forward:
- **The chain:** the last three such runs, oldest first.
- **What settles it:** a run that answers with steps to spare, or a new conversation.
- **What the next run gets:** the chain, as its request's `records`, if it's in the same conversation.

**Fitted to the window, out of the history's room.** `scheduled_run` fits the chain to half the history's bytes for the window the run is told (`carried_budget`). The newest record gets room first; an older one is given while it keeps an entry. The history is then sized to what the records leave.

**Given in the agent's role, fenced.**
- **How it travels:** the cell gets the records as `HATCHLING_RUN_RECORD`, on a chat run only.
- **What the agent checks:** it reads them strictly, refusing more than three or more than the budget for its window, as it does for the history.
- **Where they go:** the loop puts them after the prior turns, as one message in the agent's role, inside the prompt pack's `carried_records` envelope. A `</run_record>` in a string a run chose can't close it.
- **What the envelope says:** this is the broker's account, data and not instructions; what it says was done is done; check it against your computer before building on it; and how to keep going.

**Keep going is the owner's words.**
- **The action:** `keep_going` on the local chat, local API version 12, is the owner's alone, like Stop.
- **When something is carried:** it sends "Keep going." as the owner's message, shown as theirs, and answers `message_accepted`.
- **When nothing is:** it answers `nothing_to_continue` and sends nothing. That's the case when the last run answered, or none stopped in this conversation. The hub holds that fact (`may_keep_going` and `keeps_going`), set by the chat and withdrawn by a new conversation.

**A change from the design's words.** WO-053 says the continuation is given "a fixed task saying the owner asked it to keep going" that tells it to check the record against its computer. Here, the task is the owner's own "Keep going." The checking and keeping-going instruction rides in the record's envelope, in the prompt pack.
- **Why:** the broker reaches the prompt pack only in tests. A task text built in the broker would have put prompt wording outside the pack, or made the pack a runtime dependency of the broker.
- **What changes:** every run after a stop is given the same instruction, whether or not the owner pressed Keep going.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4538 passed, 0 failed (tree `194f94a7`) |
| `hatchling-dev audit`, `protocol-schema` | all checks passed |
| frontend `tsc --noEmit` | pass |

New tests, aimed first at the two places real values are copied by hand:
- **What a chat run is given** (`chat_run`): a test launcher keeps each request.
  - `a_run_that_stopped_hands_its_record_to_the_next` runs a chain: out of steps, then failed, then answered, then a fresh message. It reads each run's `records`: none, then the first, then the first two, then none. It also checks whether the owner may keep going after each.
  - `the_chain_is_the_last_three_and_goes_with_its_conversation`: four stops leave the last three, and a new conversation gives none.
- **What its cell is given** (`run_launcher`): `a_chat_runs_records_are_fitted_and_taken_out_of_the_historys_room`.
  - Five records come out fitted: at most three, within the budget, the newest included.
  - The history gets fewer exchanges than the same run without them, and what's left fits the reduced bounds.
- `cell`: the records reach the environment as sent, and only on a chat run.
- `api_ops`: `keep_going`.
  - Refused to a caller who isn't the owner, and in another chat.
  - `nothing_to_continue` with nothing carried, and after a new conversation.
  - With something carried: "Keep going." queued and shown as the owner's.
- `hatchling-channel` `hub`: the flag, per conversation.
- `hatchling-agent` `config`: the variable read strictly. It's refused off a chat run, unparseable, with an unknown field, with four records, or past the window's budget. Exactly at the budget is accepted, one byte past refused.
- `hatchling-loop` `runner`: the envelope's place and role.
- `hatchling-prompt`: the envelope's pinned wording, and a forged closing tag neutralized.
- `hatchling-core`:
  - `run_record`: cutting, fitting and the budget;
  - `ui`: the wire form of `keep_going` and `nothing_to_continue`, and the version pinned at 12.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The run is given no records (`Vec::new()` for the chain) | `a_run_that_stopped_hands_its_record_to_the_next`, `the_chain_is_the_last_three_and_goes_with_its_conversation` |
| `scheduled_run` gives the records unfitted | `a_chat_runs_records_are_fitted_and_taken_out_of_the_historys_room` |
| The history doesn't give up their room | `a_chat_runs_records_are_fitted_and_taken_out_of_the_historys_room` |
| The cell doesn't send them | `only_a_chat_turn_carries_the_records_it_continues` |
| A run out of steps settles the chain instead of carrying it | both chat tests |
| An answer with steps to spare carries instead of settling | `a_run_that_stopped_hands_its_record_to_the_next` |
| Keep going sends with nothing carried | `only_the_owner_keeps_going_and_only_from_a_run_that_stopped` |
| The agent skips its budget check | `a_chat_run_reads_the_records_it_continues_within_their_bounds` |
| The record is given in the owner's role | `a_carried_record_comes_after_the_prior_turns_in_the_agents_role` |

One more break, the hub holding a conversation's word only while it's open, passed in the first run. The check was redundant, since `keeps_going` compares the conversation open now. It was removed, and the hub's test still shows that a word about a conversation that isn't open is never acted on.

**Mutation**, on the lines changed against `59f83802` (`cargo mutants --in-diff`, `--lib`), tree `194f94a7`:

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 17 | 16 | 0 | 0 | 1 |
| `hatchling-prompt` | 2 | 2 | 0 | 0 | 0 |
| `hatchling-loop` | 4 | 2 | 0 | 0 | 2 |
| `hatchling-channel` | 7 | 7 | 0 | 0 | 0 |
| `hatchling-agent` | 15 | 13 | 0 | 0 | 2 |
| `hatchling-broker` | 35 | 25 | 0 | 0 | 10 |


The first run's two other findings were fixed before the run above:
- core's baseline failed on the UI version still pinned at 11;
- one agent mutant was missed, `>` to `>=` in the budget check, now caught by the exact-budget case.
