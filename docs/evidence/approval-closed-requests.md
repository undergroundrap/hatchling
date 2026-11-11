# Evidence: an approval request that stopped waiting says so

What was tested, how, and what came back, for the fix that closes an
approval request when it lapses or is withdrawn, and tells the owner when
an answer came too late. Every number here is read from a file in the
results bundle, which is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `56bc72701b4851839330b44d48764d3ac62868f2` |
| Tested tree | `be9f018c3155a6f1772a9638ae2be23d213c178e` |
| Date | 2026-11-11 (UTC) |
| Machines | Broker: x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2). App: Windows 11, Node 22.17.0 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0, TypeScript 5.8.3 |
| Results bundle | `hatchling-approval-closed-results.tar.gz`, 807829 bytes, sha256 `1e8a4644530cb2c0c6900a32e32cc03d2cd4fb1b1dfb11dfff9770913edac41d` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

A request lapsed in the broker fifteen minutes after it was asked, and
nothing said so. The app kept showing it as waiting, and showed it again
on every open, because the stream replays a request until it has been
decided. An answer to it was rejected as expired. The app closed the card
on that rejection and dropped the rejection's words, so the card left the
queue exactly as it does when an answer is taken. Deny on a request that
is still waiting worked before this change and works after it; tests at
the ledger and at the board pin it.

This reading comes from the code. It fits what a first-run test on a slow
machine reported: three Deny answers refused, no error shown, the same
request back at the front of the queue on the next open. It has not been
checked against that run's own timestamps.

## Full gate

Run as an unprivileged user with cgroup delegation,
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, once per
toolchain from a clean build:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked --no-fail-fast
cargo run -q -p hatchling-dev --locked -- seed-check
cargo run -q -p hatchling-dev --locked -- protocol-schema
```

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3667 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3667 | 0 | 0 |

`gate.log` sha256
`3ca4b858414000b1b5789fc4b47c9fa2150abe394ed851bc3ee87dae67737946`.

The broker's library tests were then run 15 times over on 1.98.1, every
test in parallel: 15 runs, 1323 passed and 0 failed in each. `stress.log`
sha256 `db9b65f722b3fcc0211e3ea6e57b0ae4c10174ce0e3402d769e0b5dc9bda6a2b`.

## The app

Run on the tested tree, from the `frontend` directory. The test commands
are the ones `npm test`, `npm run test:live-coverage` and
`npm run test:operator-coverage` run, given to node directly from a POSIX
shell so their quoted coverage patterns are read as written.

| Check | Result |
|---|---|
| `tsc --noEmit` | exit 0 |
| `lint`, `design:check`, `actions:check`, `protocol:check`, `seam:check`, `format:check` | each exit 0 |
| Unit tests, with the 78 / 88 / 81 coverage floor | 490 passed, 0 failed |
| Live adapter tests, with the 100% line, branch and function floor | 176 passed, 0 failed; 100 / 100 / 100 |
| Operator adapter tests, with the 100% floor | 16 passed, 0 failed; 100 / 100 / 100 |

`app/check.log` sha256
`05c8f6895922755d6b22833e4eadac3e760a70712d7cc439aee32ddab0ff4f79`. The
app's logs in the bundle have the machine's own paths replaced by
`<repo>`.

The notice an answer leaves when its request had closed was also looked
at in a browser, on the built app, through the walkthrough scenario this
change adds ("Answer arrives too late"): Deny on the sample request left
the notice in the request's place, the request showed as timed out in
history, the page did not overflow, and Dismiss removed the notice.

The app's browser tests (`npm run test:e2e`), including the one added
here for that notice, were not run: they are left to CI.

## Mutation

Every mutant on the lines changed against the base, on 1.98.1 with debug
info off:

```text
cargo mutants --in-place --in-diff <hatchling-broker's diff> --timeout 60 \
    --exclude-re work_once -- --lib
cargo mutants --in-place --in-diff <hatchling-broker's diff> --timeout 300 \
    --re work_once -- --test artifacts the_workers_
```

The worker's turn (`work_once`) is reached only by an integration test,
so its mutants have a run of their own against that test.

| Run | Mutants | Caught | Missed | Timed out | Unviable | `outcomes.json` sha256 |
|---|---|---|---|---|---|---|
| `hatchling-broker`, library tests | 21 | 12 | 0 | 0 | 9 | `62acb1c36e3c062d95ae1b8c7d2f58cdd35c4aaebd546ac6830c1f7cf2623f0a` |
| `hatchling-broker`, the worker's turn | 3 | 3 | 0 | 0 | 0 | `185a4fad6ef8c0dc47b40dd70cb066f315d978befe5208a0dfe9acdd63809eab` |

`hatchling-core`'s part of the change is one event variant and one enum.
The tool finds no mutant in it (`mut-core-list.txt`); its wire form is
pinned by a test. No mutant was excluded, so there is no equivalence
proof to give.

Unviable:

- `crates/hatchling-broker/src/api_ops.rs:672:9: replace Ops<'a>::decide -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/approval_board.rs:232:9: replace Board::propose_activation -> Result<Requested, Refusal> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/approval_board.rs:307:9: replace Board::propose_site -> Result<Requested, Refusal> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/approval_board.rs:353:9: replace Board::propose_memory -> Result<Requested, Refusal> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/approval_board.rs:416:9: replace Board::propose_schedule -> Result<Requested, Refusal> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/approval_board.rs:481:9: replace Board::propose_signed -> Result<Requested, Refusal> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/approval_board.rs:534:9: replace Board::decide -> Result<State, DecisionRejection> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/approvals.rs:278:9: replace Approvals<A>::take_closed -> Vec<Closed> with vec![Default::default()]`:
  unviable, build failure.
- `crates/hatchling-broker/src/approvals.rs:377:9: replace Ledger<A>::propose -> Result<Requested, Refusal> with Ok(Default::default())`:
  unviable, build failure.

## Defences broken by hand

Each defence was removed by hand, one at a time, and the tests that guard
it were run and their failures read. The unbroken tests passed before and
after. The broker's were run in its sandbox; the app's on its built
JavaScript, from a copy of the file that was put back after.

| Defence removed | Test that failed | How it failed |
|---|---|---|
| A lapsed request is told | `a_request_that_lapses_or_is_withdrawn_is_told_once`, `a_request_waits_for_as_long_as_its_proposal_says`, `a_request_that_stops_waiting_is_told_to_the_app` | nothing told where the lapsed request was due; "a decision that came too late left its request untold" |
| A withdrawn request is told | the first and the last of those | nothing told where the withdrawn request was due |
| A request withdrawn after it was approved isn't told as closed | `a_request_that_lapses_or_is_withdrawn_is_told_once` | "a decided request was told as closed" |
| A closed request is told once | that test, `a_request_that_stops_waiting_is_told_to_the_app` | the same request told again |
| A request waits as long as its proposal says | `a_request_waits_for_as_long_as_its_proposal_says`, `a_request_that_stops_waiting_is_told_to_the_app` | it lapsed at 1000 where 86500 was due |
| A closed request's card is forgotten | `a_request_that_stops_waiting_is_told_to_the_app` | "a lapsed request's card was kept" |
| A closed request is said on the stream | same | no event where one was due |
| A decision that came too late says so at once | same | the late request's event was missing |
| The worker looks each turn | `the_workers_turn_tells_the_app_of_a_request_taken_back` (integration) | no event where two were due |
| The worker looks at the time it was given | same | the lapsed request's event was missing |
| Something to remember waits a day | `a_request_that_stops_waiting_is_told_to_the_app` | it lapsed at 1000 where 86500 was due |
| The app's ledger keeps a request closed | "a request the broker closes leaves the queue for good, and says how it ended" and the tests after it | an answer was sent to a closed request |
| The app marks a request the broker closed | "a request the broker closes leaves the queue for good, and says how it ended" | still pending |
| An answer whose request closed first is still accounted for | "an answer to a request the broker closed under it is told as undecided, whichever word arrives first", "the scripted answer that arrives too late closes its request and grants nothing" | no notice |
| The answer leaves a notice | those two, "an answer that reaches a closed request says nothing was decided, until dismissed" | no notice |
| The newest request leads | "approvals reconcile without resending, and an older revision never replaces a newer one" | the older request first |

## What this change does not do

- **The same proposal, asked again.** A proposal identical to one already
  approved is still put to the owner again. With a day's window it can
  wait beside newer ones; the newest leads the queue.
- **A reminder for a past time.** A schedule proposed for a time already
  gone is still shown without a flag and refused only when it is applied,
  and that refusal still points at a log.
- **A restart.** Requests live in the broker's memory. A restart withdraws
  them, and an answer to one after that is rejected as unknown, which the
  app now says in the same notice.
- **The wire version.** The event is added without changing the app
  protocol's version number: the app and the broker ship together.
