# Evidence: a kept chat card says where its run said it stopped

What was tested, how, and what came back, for the line a card kept from a
chat holds of its run's last answer (session protocol v31). Every number
here is read from a file in the results bundle, which is attached to the
release that carries this change.

| | |
|---|---|
| Base commit | `a5b5f4b5` (`feat(ideas): keep what becomes of each idea (#472)`); tested on a commit of the same tree, `2b5614beec6aa13c2424b69e6063b41ef57452af`, before #472 was merged |
| Tested tree | `3079c65f88271043a3d651ea63937eff4c650411` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 threads, in a sandbox; the app's check and breaks on Windows 11, Node 22.17.0 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned) |
| Results bundle | `hatchling-stopping-line-results.tar.gz`, 621463 bytes, sha256 `a83b7a5e3cd9d164ed2544150567a089488e5c3a86b1a4813127e1fb327ffa17` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What it is

When the owner keeps a chat that ran out of steps, the card now holds the
run's last answer as `run_said`, apart from the owner's words: trimmed to
400 bytes, as theirs are, and none when the run said nothing. The broker
holds it with the offer and keeps it on the card; no model call makes it.
It is the agent's claim, never checked. The app shows it labelled so, and
a run started from the card is given the owner's words alone.

`SuggestionResult::Changed` holds its card boxed: clippy's
`large_enum_variant` refused the enum once the card grew. It is written
on the wire as before.

## Runs

| Step | Log | Result |
|---|---|---|
| Format, compile, the tests of what it touches, the protocol schema, clippy on both toolchains | `quick.log` | every step exit 0; the schema matches the code; clippy `-D warnings` clean on 1.85.0 and 1.98.1 |
| Breaks by hand, Rust | `unit-breaks.log` | 10 broken, 10 caught, 0 survived |
| Breaks by hand, the app | `app-breaks/breaks.log` | 5 broken, 5 caught, 0 survived |
| Mutation on the changed lines | `mut.log`, `mut-*-outcomes.json` | see below |
| Full gate, 1.85.0 | `gate.log` | fmt, clippy (default and all features), build, test, seed-check, protocol-schema: each exit 0; 135 test binaries, 3964 passed, 0 failed |
| Full gate, 1.98.1 | `gate.log` | the same; 3964 passed, 0 failed |
| The app's check (`npm run check`) | `frontend-check.log` | exit 0; 503 unit tests passed; the live adapter's 184 and the operator adapter's 16 passed, each at 100% coverage |
| As root, the tests of what it touches | `root/stopping.log` | core 203, channel 35, broker 118 passed, 0 failed |

Commands, as run (`sp-break.sh`, `sp_break.py`, the app's
`sp-app-break.sh` and `sp_app_break.py`, and the slice's diffs are in the
bundle):

- Mutation, per crate: `cargo mutants --in-place --in-diff sp-slice-<crate>.diff --timeout <60 or 180> -o <out> -- --lib`,
  each diff being the change to that crate against the base.
- Gate, on each toolchain: `cargo clippy --offline --workspace --all-targets [--all-features] --locked -- -D warnings`,
  `cargo build --offline --workspace --locked`, `cargo test --offline --workspace --locked --no-fail-fast`.

### Mutation

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 0 | 0 | 0 | 0 | 0 |
| `hatchling-channel` | 7 | 7 | 0 | 0 | 0 |
| `hatchling-broker` | 11 | 6 | 0 | 0 | 5 |

`mut-channel-outcomes.json` sha256 `66c9277f74586bbefc61de502606f8e608731fc27867e06557c120fac4728698`;
`mut-broker-outcomes.json` sha256 `1b07a57bfed852941d87daed09b2e77fac58e3c3a11c3119dcc983c987965ff5`.
Core's change is a wire field and a boxed answer: cargo-mutants found
nothing to mutate (`mut-core.log`).

Unviable:

- `chat_run.rs:478:9: replace Chat<R, K>::step -> Step with Default::default()`: unviable, build failure
- `chat_run.rs:679:5: replace chat_turn -> Step with Default::default()`: unviable, build failure
- `suggestions.rs:371:5: replace suggest_from -> SuggestResult with Default::default()`: unviable, build failure
- `suggestions.rs:501:5: replace handle_at -> SuggestionResult with Default::default()`: unviable, build failure
- `suggestions.rs:673:5: replace keep_at -> SuggestionResult with Default::default()`: unviable, build failure

### What the breaks covered

Rust (`sp_break.py`):

- The wire: a card without it written with `null`; the field under
  another name.
- The hub: the answer dropped from the offer; the owner's words given for
  it.
- The chat: the owner's words taken for the answer; the answer handed
  over untrimmed; nothing handed over.
- The keep: the card never holding it; an empty answer kept as a line;
  the run's words kept as the owner's.

The app (`sp_app_break.py`):

- The card never showing it; the run's words shown without their label;
  an empty answer shown; the run's words shown as what the card would do;
  the card's note put into the chat box when it is started.

## What wasn't tried

- **An install:** that a real model's out-of-steps answer appears on a
  kept card, as the owner sees it, on a real page.
- **The signals change under it** was tested on its own
  (`docs/evidence/idea-signals.md`); here its tests ran again as part of
  the gate.
