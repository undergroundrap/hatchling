# Evidence: the conversation sized to the model's window (#595)

What was changed, and how it was tested: a chat run is given as much of the owner's conversation as its model's window has room for, word for word, within a cap.

| | |
|---|---|
| Base | `main` at `de1e137b` (#659, WO-051's close) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Decided by the maintainer | WO-026's rule stands: no model writes a word of the conversation (2026-11-22). History is half the window, capped at three times today's, after room is set aside for the agent's prompt and tools (2026-11-25) |

## What it is

- **Bounds sized to the window** (`conversation::Bounds::for_window`). First, 12,288 tokens are set aside for the agent's own prompt and tools (`RUN_RESERVE_TOKENS`): they took about 9,400 on every call in WO-051's measurement. The conversation is given half of what's left, at three bytes a token, and the other half is the run's own work: its tools' output and its answer. Each of the four bounds (exchanges, their bytes, the owner's earlier messages, their bytes) is scaled from today's alike. Never less than today's, `Bounds::LEAST`, which fit the smallest window and stand with none declared; never more than three times them, `Bounds::MOST`, reached at 77,824 tokens:

  | Window | Exchanges | Their bytes | Earlier messages | Their bytes |
  |---|---|---|---|---|
  | none, or 32,768 or less | 8 | 24 KiB | 32 | 8 KiB |
  | 65,536 (the installer's) | 19 | 58.5 KiB | 78 | 19.5 KiB |
  | 77,824 and up | 24 | 72 KiB | 96 | 24 KiB |

  Without the set-aside, a 32,768-token window would have given the conversation half of itself, leaving the run less room to work than before: a long conversation there would run out of room sooner.

- **Why a cap.** Each turn of a run resends the whole conversation. At the most, about 32,000 tokens by the run's own estimate, a chat run's 2,000,000-token budget still allows about 60 turns of conversation alone, or nearer 50 with the agent's prompt and tools beside it; with no cap, a 1,000,000-token window's half would spend it in 4, and a paid model would bill it every turn. The cap also keeps the history and the earlier messages each under the 128 KiB one environment string may be, so a run is still given them as it was.
- **One compaction** (`Conversation::compact`, `Conversation::within`). Past the bounds, the oldest answers go and what the owner said in them is given as earlier, word for word, as before. The chat keeps the conversation within the bounds of the window of the server in use (`model_choice::window`, so a switch in Settings counts), and tells the owner when answers go, as before. A run's cell is given it within the bounds of the window the cell is told (`scheduled_run`), and the agent reads it by the same window's bounds, so the broker never sends what the agent refuses.
- **The store keeps what the chat holds.** It reads back up to `Bounds::MOST`, so a conversation kept under a large window resumes under a smaller one, compacted as it's given.
  - **Slots:** its sides go round 25 slots, one more than the most a conversation holds, where there were 9.
  - **Parts:** a side larger than one sealed entry (32 KiB) is kept in up to 3 parts, `user-N`, `user-N-1`, `user-N-2`. A side from before is its first part alone.
- **A store from before is moved as the broker starts** (`ConversationStore::move_slots`). Its index names no slots, so it has 9. Each side is written again under a number from 9 on, so in a slot the old index never names. Then the new index names them, in one write. Then the old slots go. Stopped before the index, the old conversation is whole and the move is made again; stopped after, the new one is, and whatever the old slots still hold is never read.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | pass, after the last round's four lints in tests were fixed: two size invariants the tests asserted are now checked as the broker is built |
| the whole workspace's tests | 4490 passed, 0 failed (after a `cargo build --workspace`, which a cleaned target needs: some suites run the workspace's binaries) |
| `hatchling-dev audit` | all checks passed |

New tests:
- the bounds by window: none, small, 32,768, 40,000, 65,536, just under the most, and larger;
- at the most, the history and the earlier messages each under 128 KiB as an environment string, read back whole, and refused by a run told no window;
- a conversation given within smaller bounds is compacted as a push would, and what was kept is left as it was;
- the agent reads a history as large as its window's bounds, and refuses it with no window or a smaller one (32,768 and 65,536);
- a chat on a 131,072-token window keeps and gives 24 exchanges;
- a chat run's cell is given the conversation within its window's bounds, at none, 32,768, 65,536 and 131,072, and it reads back by those bounds;
- the chat sizes by the window of the server in use;
- the store: 25 slots and 3 parts, a side kept in parts (a character split across them included), a conversation at the most kept and resumed, a store from before moved and going on, and a move stopped before or after its index leaving a whole conversation.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The window ignored: always the least | `the_bounds_are_sized_to_the_window` |
| A run's cell given the most, whatever its window | `a_chat_runs_cell_is_given_the_conversation_its_window_bounds` |
| The agent reading by the least, whatever its window | `a_chat_run_reads_the_history_its_window_bounds` |
| The chat keeping the least, whatever its window | `a_chat_keeps_what_its_windows_bounds_hold` |
| A store from before never moved | `a_store_from_before_is_moved_to_its_new_slots`, `a_move_stopped_part_way_leaves_a_whole_conversation` |
| A side never split into parts | `a_side_larger_than_an_entry_is_kept_in_parts`, `a_conversation_at_the_most_is_kept_and_resumed` |
| A move writing into the old slots | `a_store_from_before_is_moved_to_its_new_slots`, `a_move_stopped_part_way_leaves_a_whole_conversation` |
| A switch's window ignored | `the_owner_switches_between_the_operators_two_servers` |

**Mutation**, on the lines changed against `de1e137b`:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` (library) | 48 | 45 | 0 | 0 | 3 |
| `hatchling-agent` (library) | 12 | 7 | 0 | 0 | 5 |
| `hatchling-broker` (library and `schedule_run`) | 58 | 53 | 0 | 0 | 5 |

The first full round missed 8, each fixed before this one:
- **Core, 5:** the agent's length checks at their exact boundaries. A test now reads back exactly as many messages as the bounds allow, and raw text of exactly twice the bytes, which is parsed and isn't JSON. One byte more is refused unparsed.
- **Broker, 2:** a guard that could never be false: every store has slots for more than one exchange. It was removed.
- **Broker, 1:** a field the rewrite of New conversation made deletable: its numbers carrying on from the last conversation. A test now checks them.

The tree mutated differs from the one merged only by the last four lints' fixes. They're in tests, and two size invariants moved into the store as checks made when the broker is built. No function changed.

## Said plainly

- **A switch to a smaller window gives less than is kept,** compacted as it's given, and the owner isn't told of that: they're told when the chat itself compacts.
- **An older broker can't read the store after this one has.** Its index now says its slots, a field the older broker refuses, so it starts a new conversation. A release isn't downgraded in place.
- **The window is the one the operator declared.** With none declared, the least stands, as today, even where the model's server would say a larger one.
- **A hosted model's window is used up to the cap, no further.** Giving it more, as an agent with a large window can hold, needs the conversation sent over the session (an environment string holds 128 KiB), a token budget that counts a cached prompt as the provider bills it, and a measurement of the cost. That's a change of its own.
