# Evidence: memory sealed where it's kept (WO-047 slice 2)

What was added, and how it was tested, for memory sealed on disk ([#640](https://github.com/undergroundrap/hatchling/pull/640)).

| | |
|---|---|
| Base | `main` at `0437fbac` (slice 1, #639) |
| Merged | `db18db91`, 2026-11-24 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0; root runs in the same distro |

This record was added after the merge, in slice 3's pull request. It changes no code.

## What it is

- **Memory items are a sealed store's.** `hatchling-memory` keeps its rules and reaches the bytes through a `Backing`. The broker's backing keeps each item, each space's index of ids, and the registry of spaces in `Store::MEMORY` at `HATCHLING_MEMORY_DIR/sealed`, under a key of memory's own that the keeper hands the broker with the others.
- **Values of any size, whole or not at all** (`sealed_blobs`): a value is up to 99 chunks of the vault's 32 KiB entries, under a head naming one generation. The new chunks are written first, then the head, then the old chunks are forgotten. A missing, short or swapped chunk is refused.
- **0.9's plaintext is moved in at the start.** Each item is checked to be the item it's named for, sealed, and only then removed. A file that isn't an item is left in place and logged.
- **As the broker's own user.** The start runs after the drop to that user, and the broker's state is handed to it before the stores inside it. Both were found by CI's install jobs, not by the sandbox, which runs as a plain user.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4410 passed, 0 failed |
| `hatchling-dev audit` | all checks passed |
| CI, on the merged head | 24 checks passed, the install and the upgrade from 0.9 among them |

Among the new tests:
- every scope written and read with no word of it on the disk;
- purge across spaces, and an index naming an item that's gone;
- a swapped head or another key refused;
- a migration whole, cut short and run again, leaving a file that isn't an item, and keeping a file it can't seal;
- `tests/memory_start.rs` (now `sealed_start.rs`): the start in a process of its own, where the only keys are the ones it hands on;
- the hand-over order, as root.

**Hand-breaks**, each failing its test and passing again when restored:

| Break | Caught by |
|---|---|
| A file removed before it's sealed | `an_item_that_cant_be_sealed_keeps_its_file` |
| A start that doesn't hand its keys on | `tests/memory_start.rs` |
| The read-back id check dropped | `an_item_under_another_items_name_is_corrupt` |
| The broker's state handed over after its stores (as root) | `a_store_inside_the_brokers_state_is_handed_over_after_it` |

**Mutation**, on the lines changed against `0437fbac`:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-memory` | 48 | 37 | 0 | 0 | 11 |
| `hatchling-vault` | 7 | 4 | 0 | 0 | 3 |
| `hatchling-broker`, `memory_sealed.rs` and `sealed_blobs.rs`, after the fixes | 71 | 54 | 0 | 0 | 17 |
| `hatchling-broker`, the hand-over, as root and as a plain user | 3 | 2 | 1 | 0 | 0 |

The broker's first pass, 93 mutants, missed 11. Each got a test, or an exclusion with its reason in `.cargo/mutants.toml`: `purge_expired`'s `is_expired` guard, the first of WO-024's two readings, and `migrate`'s log-only condition.

The one the hand-over leaves is `act_for_broker` -> `Ok(())`. Without a broker user it returns `Ok(())` anyway. As root it acts for that user for real, which a process does only once, and another test does it. CI's install jobs cover it end to end; they caught this slice's two root bugs.
