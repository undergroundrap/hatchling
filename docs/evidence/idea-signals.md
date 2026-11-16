# Evidence: what becomes of each idea is kept

What was tested, how, and what came back, for the record the broker keeps
beside the owner's Ideas page (`crates/hatchling-broker/src/idea_signals.rs`,
`suggestions.rs`, `kept_log.rs`). Every number here is read from a file in
the results bundle, which is attached to the release that carries this
change.

| | |
|---|---|
| Base commit | `50603c67` (`test(vault): the keyring tests read a key's day from its record (#470)`) |
| Tested tree | `76392e048fdb587f2071239847a2f298015bc6dc` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 threads, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned) |
| Results bundle | `hatchling-idea-signals-results.tar.gz`, 936210 bytes, sha256 `41a09bf3b0072f3cfc3f69a3678e8e2383a1d7bdf17412e6406d5664a3bd5ce1` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What it is

The page keeps its newest 100 cards and lets go first of those the owner
started, set aside or let go stale, so nothing kept what became of them.
The broker now appends a line beside the page for each event: put on the
page, kept from a chat, fired, set aside, brought back, stale, its source
gone, answered. A line holds when, the idea's id, its source kind and its
area: never a card's words, a goal's id or title, or what was said in a
chat. An event is kept only once the page it changed is written. Past
20,000 lines the record keeps the newest 10,000. Nothing reads it yet.

## Runs

| Step | Log | Result |
|---|---|---|
| Format, compile, the broker's tests of what it touches, clippy on both toolchains | `quick.log` | every step exit 0; clippy `-D warnings` clean on 1.85.0 and 1.98.1 |
| Breaks by hand | `unit-breaks.log` | 17 broken, 17 caught, 0 survived |
| Mutation on the changed lines | `mut.log`, `mut-broker-outcomes.json` | see below |
| Full gate, 1.85.0 | `gate.log` | fmt, clippy (default and all features), build, test, seed-check, protocol-schema: each exit 0; 135 test binaries, 3963 passed, 0 failed |
| Full gate, 1.98.1 | `gate.log` | the same; 3963 passed, 0 failed |
| As root, the broker's tests of what it touches | `root/signals.log` | 40 passed, 0 failed |

Commands, as run (`sg-break.sh`, `sg_break.py` and the slice's diff are in
the bundle):

- Mutation: `cargo mutants --in-place --in-diff sg-slice-broker.diff --timeout 180 -o <out> -- --lib`,
  the diff being the change to `crates/hatchling-broker` against the base.
- Gate, on each toolchain: `cargo clippy --offline --workspace --all-targets [--all-features] --locked -- -D warnings`,
  `cargo build --offline --workspace --locked`, `cargo test --offline --workspace --locked --no-fail-fast`.

### Mutation

`mut-broker-outcomes.json` sha256 `06b94ac86442316f8759d6879b90612d5b29fc53197cf28cc23c93cd7f98258d`.

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-broker` | 20 | 15 | 0 | 0 | 5 |

Unviable:

- `idea_signals.rs:72:9: replace Source::of -> Self with Default::default()`: unviable, build failure
- `suggestions.rs:227:5: replace retire -> Vec<Suggestion> with vec![Default::default()]`: unviable, build failure
- `suggestions.rs:366:5: replace suggest_from -> SuggestResult with Default::default()`: unviable, build failure
- `suggestions.rs:495:5: replace handle_at -> SuggestionResult with Default::default()`: unviable, build failure
- `suggestions.rs:665:5: replace keep_at -> SuggestionResult with Default::default()`: unviable, build failure

### What the breaks covered (`sg_break.py`)

- Each event the page keeps, taken out or named wrong in turn: put on the
  page, a retirement, stale against source gone, fired, brought back, an
  owner's change, an answer, a keep, a listed page's retirements.
- An owner's change kept before the page is written (kept twice, and kept
  for a write that failed).
- A card's title written into the record; a chat's card kept as having no
  source; the record kept somewhere other than beside the page.
- The record given the shared bound of 200, or grown only to its kept count;
  a log that ignores its own line bound, or its own kept count.

## What wasn't tried

- **An install.** No owner's page was used; the record was read only by the
  tests.
- **A record that fills on a real page.** The bound is tested on a record
  filled by the test to 20,000 lines; no page has made that many events.
