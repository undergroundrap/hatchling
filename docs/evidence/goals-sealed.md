# Evidence: goals and tracking sealed where they're kept (WO-047 slice 3)

What was added, and how it was tested, for goals and tracking sealed on disk ([#642](https://github.com/undergroundrap/hatchling/pull/642)).

| | |
|---|---|
| Base | `main` at `db18db91` (slice 2, #640) |
| Date | 2026-11-24 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |

## What it is

- **Goals reach their bytes through a backing.** `GoalsSource` reads, and is all the feed's reader holds; `GoalsBacking` writes too, and only `GoalsStore` holds one. `Files` is 0.9's layout, kept for the migration and the crate's tests.
- **The broker's sealed backing** (`Store::GOALS` at `HATCHLING_GOALS_DIR/sealed`) keeps each goal under its user and id, each user's index of ids, and each tracking log as segments of 64 entries under a head counting them.
  - An entry's segment is written before the head counts it. A crash between the two leaves an entry past the head: no read sees it, and the next append writes over it.
  - A store never written to reads as empty, and reading never makes it.
  - A memory-only feed query doesn't open the goals store.
- **0.9's plaintext is moved in at the start.** Each goal is checked and sealed before its file is removed. Each log is checked whole and in order, then imported with its head written last, before it's removed. What isn't a goal or a whole log is left in place and logged.
- **Memory's and goals' keys are published together** (`held_keys`), by one start that runs as the broker's own user. `OpensStores::keys_of` takes several stores, and refuses them all when it doesn't hold one.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4424 passed, 0 failed |
| `hatchling-broker --lib` and `hatchling-goals`, after the last tests were added | 1845 and 58 passed |
| `hatchling-dev audit` | all checks passed |

Among the new tests:
- goals and tracking written and read with no word of them, or where they came from, on the disk;
- a log across segments, an entry a crash left past the head, and a missing segment refused, nothing appended to it;
- another key refused when the store is opened;
- a store never written to reading as empty and made by nothing;
- the keys as names of their own;
- the migration whole, cut short and run again with nothing doubled, with an import cut short before its head;
- what isn't a goal or a whole log left in place, a log whose sealed copy differs left in place, and what can't be sealed keeping its file;
- `tests/sealed_start.rs`: memory and goals moved in and their keys handed on, in a process of its own.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| A goal's file removed before it's sealed | `what_cant_be_sealed_keeps_its_file` |
| A log removed before it's imported | `what_cant_be_sealed_keeps_its_file`, `a_log_whose_sealed_copy_differs_is_left_where_it_is` |
| The never-made check dropped from tracking's read | `a_store_never_written_to_reads_as_empty_and_isnt_made_by_reading` |
| A start that doesn't hand its keys on | `tests/sealed_start.rs` |

Not exercised by a test: the order of an append's segment and head, and of an import's segments and head. Each would need a crash between two writes. The append test forges what such a crash leaves; the order itself was read in review.

**Mutation**, on the lines changed against `db18db91`:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-goals` | 69 | 53 | 0 | 0 | 16 |
| `hatchling-vault` | 7 | 4 | 0 | 0 | 3 |
| `hatchling-broker`, `goals_sealed.rs`, after the fixes | 102 | 87 | 0 | 0 | 15 |

The first passes missed 14 in `hatchling-goals` and 8 in the broker. Each got a test, or an exclusion with its reason in `.cargo/mutants.toml`:
- the 14 were the file helpers only the migration uses;
- 7 were the feed's empty source, of which 2 are the value it already returns, written another way;
- 1 was `migrate`'s log-only condition.

One rerun of the broker's baseline failed on an unrelated timing test. That exposed a race in a site's end, filed as #641 and fixed on its own.
