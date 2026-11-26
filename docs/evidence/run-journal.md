# Evidence: each chat run's record, kept by the broker (WO-053, slice 1)

| | |
|---|---|
| Base | `main` at `893de6a0` (#707) |
| Date | 2026-11-26 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Design | [WO-053](../workorders/workorder-053-the-run-record.md), approved by the maintainer (#703), amended (#707) |

## What it does

The first part of WO-053's run record, the effect journal, recorded and kept. Nothing reads it yet: Keep going and what each run did under its answer come next, and read it.

**Recorded by the broker, as the session serves it.** The session loop already has every request, its answer, the session and the cell in scope. Before a request is handled, what the journal needs of it is taken (`run_record::asked`: a send's channel, text and whether an approval came with it), the way the run trail takes its tool. Once it's answered, `Journal::note` turns the pair into effects:

| Effect | When |
|---|---|
| a message sent, with its text | a send answered `sent`; with an approval presented, the approval used too |
| a message redacted, without its text | a send the output gate stopped |
| a file written or edited in the Room | a Room write or edit answered as done |
| an item or folder in the Library | a hand-over answered as done |
| an approval asked | any use the trail counts as put to the owner |
| a use refused | any use the trail counts as refused |

The agent writes none of it: no session op reaches the journal, and its entries come from the broker's own reading of each answer.

**Bounded.** At most 64 entries and 16 KiB as JSON. Past either, an effect is counted by its kind instead of kept. A message's text is cut to 400 bytes and a name or path to 256, each at a character's edge.

**Checked against the vault before it's kept.** When the session ends, however it ends, a known run's record is checked once, whole, with the held check (`CheckFor::Record`). Only when that finds a secret, or can't say, is each string checked, so that one alone is kept as `[withheld: it held a secret]`. A string that can't be checked is withheld too.

**Sealed with its exchange.** The record is held for the chat (at most 16, the oldest let go). It's taken when the chat keeps the exchange its run answered, and written to the conversation store beside that exchange in the same locked keep: `record-<slot>`, sealed like the exchange's sides. An exchange kept without one leaves none in its slot. A slot that's cleared, when its exchange leaves the conversation or with New conversation, takes its record with it. A store opened again, as after a restart, reads them back. A record that isn't one is an error to read, and never stops the conversation itself being resumed.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4524 passed, 0 failed (tree `eee46327`) |
| `hatchling-dev audit` | all checks passed |

New tests:
- `hatchling-core` `run_record`:
  - effects kept in order with their time;
  - strings cut at a character;
  - the entry bound and the byte bound, each counted by kind;
  - only the string holding a secret withheld;
  - strict decoding, refusing unknown fields and kinds;
  - every kind named;
  - the texts in order.
- `hatchling-broker` `run_record`:
  - a send's channel, text and approval taken before it's handled;
  - each message ending;
  - each Room effect;
  - approvals asked and refusals, by tool;
  - the record checked whole, then string by string only on a hit;
  - a string that can't be checked withheld;
  - the held records bounded and each taken once.
- `conversation_store`:
  - kept sealed with its exchange, read back by a store opened again, and not on disk in the clear;
  - a slot never holding another run's record: a stale one cleared, gone with New conversation, gone with its exchange past the conversation's bound;
  - a corrupt record an error to read but not to resume;
  - the chat's keep taking its run's record, and one from an earlier conversation taken and not kept.
- `serve::run_record_tests`, through a real session:
  - two answers sent, one holding a planted vault secret, and a third send refused, leave exactly the plain answer, the withheld mark and the refusal;
  - a session that isn't a known run's leaves no record.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The session never publishes its run's record | `a_runs_session_leaves_its_record_checked_against_the_vault` |
| The session asks nothing of a send before it's handled | `a_runs_session_leaves_its_record_checked_against_the_vault` |
| A record kept without its vault check | `a_record_is_checked_whole_then_each_string_only_on_a_hit`, `a_string_that_cant_be_checked_is_withheld`, `a_published_record_is_checked_then_taken_by_its_run`, and the session test |
| The record not written with its exchange | `a_runs_record_is_kept_sealed_with_its_exchange`, `a_slot_never_holds_another_runs_record`, `the_chats_keep_takes_its_runs_record` |
| A slot cleared without its record | `a_slot_never_holds_another_runs_record` |
| An approval used not noted | `a_message_is_noted_as_it_ended` |
| The chat's keep takes no record | `the_chats_keep_takes_its_runs_record` |
| Past its entries, an effect kept anyway | `past_its_entries_an_effect_is_counted_by_kind` |

**Mutation**, on the lines changed against `893de6a0` (`cargo mutants --in-diff`, `--lib`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 33 | 32 | 0 | 0 | 1 |
| `hatchling-broker` | 38 | 34 | 0 | 0 | 4 |

The first run had one broker mutant time out: `>=` to `<` in `Held::hold`'s eviction loop made a `while` that never ends on an empty queue. Since records arrive one at a time, at most one needs letting go, and the loop became an `if`. That mutant now leaves the queue at one record, and `a_record_is_taken_once_and_the_oldest_are_let_go` catches it. The rerun above is on the final tree.
