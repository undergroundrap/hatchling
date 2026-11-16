# Evidence: an approval runs only what the owner was shown

What was tested, how, and what came back, for three changes that make
what runs after an approval the thing its card showed: an approved
signed-in act re-checked against the page just before it is made, a
click made only where it lands on its element, and an approved build
bound to the request hash the owner approved. Every number here is read
from a file in the results bundle, which is attached to the release that
carries this change.

| | |
|---|---|
| Base commit | `a5b5f4b5` (`feat(ideas): keep what becomes of each idea (#472)`) |
| Tested tree | `d2a2f842cad298a7e263e085213f6da31e440707` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 threads, in a sandbox; headless Chromium (`chrome-headless-shell`) for the real-browser test |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned) |
| Results bundle | `hatchling-approval-binding-results.tar.gz`, 1197614 bytes, sha256 `37ed3e965ba99a361c76746622ec7919669ad56d71df6f8fd281b2628c41d083` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was found

A read of how each kind of approval binds. A decision names the request's
id, revision and digest, and the digest covers the action record, so the
question was whether the card, the record and what runs come from the
same values. For keeping a memory, a schedule, reading a site and
browsing signed in, they do. Two didn't hold all the way:

- **An approved signed-in act** (a click, Enter, a choice) is bound to the
  page as its card showed it, and is made by the session on the live page.
  Nothing checked, at the act, that the page, the element by that number
  or what was typed was still as shown, and the owner can take minutes to
  decide. A click also goes through the browser's input at the element's
  centre, so whatever was on top there took it.
- **An approved build** ran whatever the idea pinned under the activation
  id. It held because an activation id is kept only for the same request
  (same idea revision and hash, kind, builder and request hash), but the
  run didn't check the approved hash itself.

## What changed

- Just before an approved signed-in act, the broker takes a snapshot,
  rebuilds the act's record from it, and acts only when it equals the one
  approved. Otherwise nothing is done and the agent is told to look again
  and ask again. A snapshot that fails isn't acted on.
- `element.js` makes a click only when `elementFromPoint` at the click's
  point is the element, inside it, or one of its labels. This is every
  click the agent makes.
- `ActivationRun` keeps the approved `request_hash`; the worker confirms
  through `activation::confirm_approved`, which refuses any other pin.

## Runs

| Step | Log | Result |
|---|---|---|
| Format, compile, the tests of what it touches (the real browser's included), clippy on both toolchains | `quick.log` | every step exit 0; clippy `-D warnings` clean on 1.85.0 and 1.98.1 |
| Breaks by hand | `unit-breaks.log` | 9 broken, 9 caught, 0 survived |
| Mutation on the changed lines | `mut.log`, `mut-activation.log` | see below |
| Full gate, 1.85.0 | `gate.log` | fmt, clippy (default and all features), build, test, seed-check, protocol-schema: each exit 0; 135 test binaries, 3965 passed, 0 failed; the real-browser test ran and passed |
| Full gate, 1.98.1 | `gate.log` | the same; 3965 passed, 0 failed |
| As root, the broker's tests of what it touches | `root/binding.log` | library 87, `artifacts` 31 passed, 0 failed |

The breaks and the gate ran with `HATCHLING_TEST_BROWSER` set to the
sandbox's Chromium and `HATCHLING_REQUIRE_BROWSER=1`, so the real-browser
test could not skip.

Commands, as run (`ab-break.sh`, `ab_break.py`, `ab-mut2.sh` and the
slice's diffs are in the bundle):

- Mutation, broker: `cargo mutants --in-place --in-diff ab-slice-broker.diff --timeout 180 -o <out> -- --lib`;
  then the mutants in `activation.rs` again with the integration tests
  that guard them: `cargo mutants --in-place --in-diff ab-slice-broker.diff --file crates/hatchling-broker/src/activation.rs --timeout 180 -o <out> -- --lib --test artifacts`.
- Mutation, browser: `cargo mutants --in-place --in-diff ab-slice-browser.diff --timeout 120 -o <out> -- --lib`.
- Gate, on each toolchain: `cargo clippy --offline --workspace --all-targets [--all-features] --locked -- -D warnings`,
  `cargo build --offline --workspace --locked`, `cargo test --offline --workspace --locked --no-fail-fast`.

### Mutation

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-broker` | 15 | 12 | 0 | 0 | 3 |
| `hatchling-browser` | 0 | 0 | 0 | 0 | 0 |

The broker's first run, with its library's tests alone, caught 5 and
missed 7, all seven in `activation.rs`: its confirmations are tested by
the `artifacts` integration tests, which `--lib` doesn't run. Run again
with them, all seven were caught (`mut-activation-outcomes.json` sha256
`e1b3acae15d117540d5c9694708df446e764871a89cc306845c44fc7c8a13c20`; the
first run's `mut-broker-outcomes.json` sha256
`78e9abc8fb3b4cd6a1e251dd2bd63c23d69ccff92b7451337591d86c07daa84e`). The
browser crate's change is a script and its test: cargo-mutants found
nothing to mutate (`mut-browser.log`).

Unviable:

- `approval_board.rs:235:9: replace Board::propose_activation -> Result<Requested, Refusal> with Ok(Default::default())`: unviable, build failure
- `browser_ops.rs:1105:9: replace BrowserSession::changed_since -> Option<BrowserResult> with Some(Default::default())`: unviable, build failure
- `browser_ops.rs:1190:5: replace answer -> BrowserResult with Default::default()`: unviable, build failure

### What the breaks covered (`ab_break.py`)

- A build: the run ignoring the approved hash; the board recording no
  hash; the worker confirming any pin.
- A signed-in act: not looked at again; looked at and never compared; a
  failed look acted on.
- A click (in the real browser): one under a layer made; a label not
  counted; a click inside the element not counted.

## Found on the way

- **A test that couldn't see a break.** The first run had one break
  survive: acting after a failed look. The test broke the fake browser to
  make the look fail, and a broken browser is closed, so no act could
  follow either way. A real controller can answer a look with the page
  gone and keep running; the fake now can, and the test checks that no
  act follows.

## What wasn't tried

- **An install:** no real signed-in site, and no owner deciding while a
  page changed under them.
- **Every page's layers:** the click check was tried on a layer over a
  button, a checkbox under its label, and a button's own text; pages that
  put something over an element in other ways (a shadow root, a frame
  over a frame) weren't.
