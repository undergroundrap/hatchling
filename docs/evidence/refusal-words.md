# Evidence: a Library limit is said reached only when it is (WO-043, #522)

What was tested, how, and what came back, for the fix to the words a refused
hand-over is given. Found in WO-043's acceptance on a real install.

| | |
|---|---|
| Base commit | `c9451f5e` (`feat(app): download a folder, a reply's files, or any chosen, as one .zip (WO-043) (#516)`) |
| Tested tree | `3158eb2b0fc42ba62205de5f9cc78c5f8c97eb92` |
| Date | 2026-11-19 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no code:
the tested tree is the tree of the commit before it.

## What was wrong

On a real install, with the release build of `c9451f5e`:

- The agent was told "this conversation has read 128 MiB out of the agent's
  computer" when the conversation had read 96.3 MiB. A 32 MiB folder would
  have taken it past.
- It was told "the Library is full (64 items, 256 MiB or 4096 files in all)"
  with 10 items, 224 MiB and 3,929 files kept. A 115-file folder was kept
  right after.

The Library chose one refusal for both cases: a bound reached, and a bound
only this item would pass. Told nothing more can be given, the agent stops
trying, and the owner removes items they needn't.

## What changed

- **Two refusals are added.**
  - `ReadPast`: "this is more than this conversation may still read out of
    the agent's computer (128 MiB in all): something smaller may still be
    handed over, and a new conversation may read more".
  - `WouldFill`: "this would take the Library past 256 MiB or 4096 files in
    all: something smaller may still fit, or the owner removes items from
    it".
- **`ReadOut` and `Full` are said only when their bound is reached.** `Full`
  now counts a Library already at 4,096 files as reached, which it was in
  effect before, since any item then passed the bound.
- **A refused file still spends the bytes it read.** When those take the
  conversation to its most, the refusal is said as `ReadOut`, never
  "something smaller may still be handed over". So `ReadPast` only ever
  reaches a folder, which is refused before it is read.
- The order of refusals is unchanged: too large, off, spent, read, full. The
  words stay fixed: no number of what is left reaches the agent.

## Tests

- **The refusals in order, at each bound** now also cover:
  - one byte left to read, with an item of 2 (`ReadPast`) and of 0 (none);
  - one byte left in the Library, with an item of 2 (`WouldFill`);
  - a Library at 4,096 files (`Full`), and at 4,095 with a 2-file folder
    (`WouldFill`).
- **Each refusal's own true words** (split from the Library's room test): all
  nine distinct, the new ones saying "something smaller" and never "has
  read" or "is full".
- **A refused file that reads the conversation out says so:** a folder past
  the last byte is `ReadPast` and spends nothing; a file of 2 with 1 byte
  left is `ReadOut`, and so is the next precheck.
- **A file that would fill, and reads out, says read out:** 15 files of
  8 MiB are each `WouldFill`; one of 8 MiB less a byte is still `WouldFill`,
  with a byte left to read; the next is `ReadOut`.
- The tests that asserted the old merged refusal now assert the true one: the
  hand-over's "never checked" loop (`WouldFill`), and a folder past the
  conversation's bytes (`ReadPast`).

## Hand-breaks

9 breaks, each in `library.rs`, each with the tests that must fail named.
All 9 were caught, with the Library's and hand-over's 93 tests green before
and after.

| Break | Caught by |
|---|---|
| past said as out | the refusals in order; a folder past the conversation's bytes; a refused file that reads out |
| out said as past | the refusals in order; a folder spent whole or not at all |
| full said as would fill | the Library's room; the loop of refusals |
| would fill said as full | the Library's room; spends nothing; never checked; would fill and reads out |
| full forgets its files | the Library's room |
| read out a byte early | the refusals in order |
| spent out never said | a refused file that reads out; would fill and reads out |
| spent out a byte early | would fill and reads out |
| spent out over full | the loop of refusals |

A first run named the wrong guard for "out said as past". Its named test
didn't fail, but four others did, among them "a folder spent whole or not
at all", which holds that assertion. The guard was renamed and the run
repeated on the final tree.

## Mutation

`cargo mutants --in-place --in-diff` on the change to `library.rs`, with the
broker's library tests:

- **23 mutants:** 21 caught, 2 unviable, 0 missed, 0 timeouts.
- `outcomes.json` sha256 `7bd599dd25672c416d2759c35631b906e2399e1f02e6b19351d667f3c47d149f`.

## The gate

On the tested tree, with each step's exit code read directly:

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --check` | 0 | 0 |
| `cargo clippy --workspace --all-targets -D warnings` | 0 | 0 |
| the same, `--all-features` | 0 | 0 |
| `cargo test --workspace --no-fail-fast` | 135 binaries, 4,209 passed, 0 failed | 135 binaries, 4,209 passed, 0 failed |
| `hatchling-dev seed-check`, `protocol-schema`, `audit` | 0 | 0 |

The first gate stopped on 1.85.0's clippy: the Library's room test had grown
past `cognitive_complexity`. Its words half became a test of its own; that
is the tree recorded here.

## Not in this change

The app's own words for a full Library (Settings) already counted a bound
only when reached. The Library list and its updates in the app are #523.
