# WO-058 slice 1a: the memory loop's claims and the broker's rule

Part of [#782](https://github.com/undergroundrap/hatchling/issues/782), [WO-058](../workorders/workorder-058-memory-loop.md). Builds on WO-054's seat checks ([wo054-seat-checks.md](wo054-seat-checks.md)) and the seat's cell ([wo054-seat-cell.md](wo054-seat-cell.md)).

## What it is

The parts of the memory loop that need no cell and write nothing. Slice 1b launches the seat from the background clock and logs what each run would do. Slice 2 applies.

- **The loop's prompt kind** (`hatchling_prompt::PromptKind::Memory`, a seventh kind).
  - Its sections are the seat's rules (identity, hierarchy, tools, honesty, accuracy, outside content, boundaries, environment), then `memory-reconciliation` and `memory-injection-defense`, which leave the waiting list.
  - `memory-reconciliation` is rewritten for what Hatchling has: items in a store, the four claims, and their handles. #714's version described `MEMORY.md`, `USER.md` and dated logs, which Hatchling doesn't have. Its rules carry over:
    - evidence only;
    - similarity isn't duplication;
    - age alone is never a reason;
    - no removals;
    - no sensitive value;
    - the owner's later words outweigh their earlier ones;
    - an unresolved handle refuses the output.
- **The four claims** (`hatchling_core::memory_loop::LoopClaim`), each citing its records:
  - `merge` names two to eight memories and the one whose words stay. It never carries words of its own, so a merge can't reword.
  - `correct` names a memory and the words true now.
  - `check` names a memory and why it may no longer be so.
  - `keep` carries words, a kind, and, for guidance and work, why and how it applies.

  `check` holds an output to the seat's checks, then to each claim's shape. Every memory a claim targets must be one the seat was given, a merge's kept memory must be among its own, and words are never empty or over 2 KiB. A wrong field, an unknown claim (there is no `remove`), or a merge with words is off the schema.
- **The snapshot** (`hatchling_broker::memory_loop::snapshot`).
  - **Memories:** each memory the agent recalls, as `memory:<scope>/<id>`: the owner's words when they approved it, the agent's when it was kept automatically.
  - **The conversation:** each unread exchange as `message:<conversation>/<slot>-owner` and `-agent`.
  - **Bounds:** at most 192 memories, the newest, within half of 96 KiB; then the oldest exchanges in what's left, so the rest are read next run. 96 KiB keeps the packet under the 128 KiB Linux allows one environment string, which is how 1b hands it to the cell, and near what a small model's window holds.
- **The fingerprint.** A memory's is SHA-256 over its words, tags and record, so anything the owner or the broker changes about it shows; when it was written, alone, doesn't. A message's is SHA-256 over its words.
- **The rule** (`judge`), for each claim the checks passed, in the order apply takes them: merges, corrections, keeps, checks.
  - **Dropped:**
    - every source is outside content;
    - a merge across kinds;
    - a correct or keep that doesn't quote the owner's own message (an agent's answer, or the owner's message unquoted, isn't enough);
    - words that fail the proposal check the agent's own proposals pass (WO-057: a kind, why and how for guidance and work, no relative dates);
    - a secret the vault holds;
    - words the owner refused;
    - a keep a memory already says;
    - a target the snapshot wasn't made of.
  - **Waits for the owner:**
    - a correct of a memory they approved that's about them or how they want the agent to work (or that predates kinds, as the strictest);
    - a keep from a conversation that read from outside;
    - a keep past the day's 20.
  - **Applies:** a merge of one kind, any other grounded correct, a check, and a keep inside the day. Each keep spends one of the day's.
- **The tally:** counts of apply, wait and dropped, never text, for the gate's record.

## Tests

- **`hatchling-prompt`:** the pinned section text and kind list, the seventh kind in `ALL`, a tool kind told the cell's rules, and the two sections no longer waiting.
- **`hatchling-core`, `memory_loop_tests.rs`:** 10 tests:
  - each claim reads as its kind, every kind of keep included;
  - off the schema is refused (an unknown field, `remove`, a merge with words, an unknown kind, no claim named);
  - a merge's bounds, duplicates, kept memory, unknown, non-memory and malformed targets;
  - a correct or a check targets a given memory;
  - words' bounds;
  - the seat's reasons and the claims' said together and bounded;
  - outside-only kept apart;
  - no change;
  - only a quoted owner's record quotes the owner.
- **`hatchling-broker`, `memory_loop_tests.rs`:** 19 tests:
  - the snapshot's records, origins and fingerprints;
  - the newest memories and the oldest exchanges within the bounds, by count and by bytes, and a snapshot exactly at both byte bounds keeping everything;
  - a long record cut at a character's edge;
  - a message's fingerprint is FIPS 180-2's SHA-256 of "abc";
  - a memory's fingerprint is over its words, tags and record, and moves with each;
  - each row of the rule above, a memory from before kinds, the day's count spent one keep at a time, the order apply takes them, and the tally.

## The gate

On the sandbox, at the tree under test:

- `cargo fmt --check`, and clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- The whole workspace's tests: 4777 passed, 0 failed. `hatchling-dev audit`: all checks passed.
- **Hand-breaks**, each caught by these tests:
  - a merge across kinds applied;
  - the owner's own memory corrected without them;
  - the day's keeps not counted;
  - a keep from a conversation that read from outside applied;
  - a claim sourced only outside applied;
  - a secret kept;
  - the fingerprint ignoring the record;
  - the agent's words counted as the owner's;
  - a claim's targets not checked;
  - memories taking the whole snapshot;
  - a snapshot over its bytes.
- **Mutation** on the changed lines of `hatchling-broker`, `hatchling-core` and `hatchling-prompt`: 79 mutants, 60 caught, 17 unviable, 2 missed. Both misses were the byte bounds compared with `>` where `>=` passed too: nothing tested a snapshot exactly at its bound. `a_snapshot_exactly_at_its_bounds_keeps_everything` does now, and the snapshot's 13 mutants rerun: 13 caught.
- An earlier, partial pass found two more, each fixed:
  - **A message's fingerprint was tested only against itself,** so replacing it with an empty string passed. It's pinned now to FIPS 180-2's SHA-256 of "abc".
  - **Cutting a record at a character's edge was a loop** that a mutant turned endless. It takes characters while they fit now, with no loop to break.

## Not here

Launching the seat, the clock's loop and activity, and the gate's record of a run are slice 1b. Nothing in this slice runs in production yet.
