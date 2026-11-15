# Evidence: an idea the owner started says when it was answered

What was tested, how, and what came back, for a change that ties a
started idea to the conversation the owner started for it, and marks
the idea when a run answers them there. Every number here is read from
a file in the results bundle, which is attached to the release that
carries this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `a66a1a78` (`feat(app): starting an idea opens a conversation of its own (#465)`) |
| Tested tree | `cae7153ff59e2bade1f9af7cd17fdf48e7fd5f44` |
| Date | 2026-11-15 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's check and the app's breaks on Windows 11 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-idea-answered-results.tar.gz`, 1788894 bytes, sha256 `dc90fa20ad7fdb9d2126ff415be2eec3fa19eb73c9fc8a45963912040057e3d8` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it. In the
bundle's `app-breaks/` and `frontend-check.log`, the Windows machine's
paths are replaced with `<repo>` and `<scratchpad>`; nothing else in
those logs was changed.

## What was wrong

An idea the owner started stayed as started whatever came of it. The
broker had no way to tie the idea to the run that answered: "Let's do
this" marked the idea, started a conversation and filled the chat box,
and from there the message was a message like any other.

The broker also couldn't tell an answer from an answer given because
the steps ran out. An agent that answered on its last step, told it
was the last, exited 0 like one that finished.

## What changed

- **The agent's exit status** (`hatchling-loop`, `hatchling-agent`,
  `hatchling-core`). The loop's result says whether the steps ran out
  as it answered (`LoopResult::out_of_steps`): on its last step, told
  it was the last, or with an answer held back until no step was left.
  The agent then exits `80` (`run_failure::ANSWERED_OUT_OF_STEPS`),
  with its answer on its standard output as before. A subagent never
  does: its parent reads its status as done or failed.
- **The broker reading it** (`run_launcher.rs`). A run that exits with
  that status is an answer, not a failure, and the run says how it
  ended.
- **The conversation an idea is for** (`hatchling-channel`,
  `api_ops.rs`). `UiAction::NewConversation` has an optional `idea`.
  When it names an idea on the page that the owner started, the hub
  keeps the new conversation as that idea's, in memory, until the next
  conversation starts. For any other id the conversation starts all
  the same, for nothing.
- **The idea is answered** (`chat_run.rs`, `suggestions.rs`). When a
  run's answer was shown to the owner, in the conversation still open,
  and the run had steps to spare, the chat's turn asks the page: the
  idea that conversation was started for, if the owner left it
  started, becomes `answered` (`SuggestionState::Answered`).
- **The app** (`frontend`). Starting an idea names it as the
  conversation starts. An answered idea stays among those started,
  with a line under it: "Hatchling answered in the conversation you
  started for it." The Ideas page is read again each time the owner
  opens it.
- The session protocol is v29, for the app's sake; no session op
  changes. `docs/session-protocol.md`, `docs/local-api.md`, the
  changelog, the protocol schema and the app's generated types.

## What it is, and isn't

- **It is the broker's finding.** The agent has no op that marks an
  idea answered, and none was added.
- **Answered, not done.** The broker knows an answer was shown to the
  owner. It doesn't know the idea was carried out, so the state isn't
  called done, and the app doesn't show the idea as completed.
- **Any answer in the idea's conversation marks it**, whatever the
  owner sent there. The broker doesn't read the message for the idea.
- **The status is the agent's own word**, read from the kernel like
  every exit status. An agent that misreports it changes what an
  idea's card says, and nothing else: the answer is delivered either
  way, and nothing is granted or withheld by it.
- **Only the owner names an idea.** `NewConversation` is refused to a
  caller without the owner's grants, as before, and it is never a
  command, so nothing that drives the app by commands can start one.
- **The tie doesn't outlive the broker.** It is kept in memory alone:
  after a restart a started idea stays started.
- **The owner still decides.** An answered idea can be started again,
  set aside, or brought back.

## Tests

Twelve new in Rust and one in the app, and others widened.

- **The status** (`run_failure::tests`, one): it is 80, no failure's
  code, and no other status reads as it.
- **The loop** (`runner::tests`, one): out of steps on the last step
  when told so, and with an answer held until none were left; not with
  a step to spare, on a run's only step, or by a run told of no last
  step.
- **The agent** (three): a run's answer keeps what the loop said
  (`model::tests`); the status for each (`cli::tests`); and the built
  binary, run against the real broker and a scripted model, exits 80
  with its answer when the steps ran out and 0 with a step to spare
  (`tests/local_model.rs`).
- **The hub** (`hub::tests`, one): what a conversation was started for
  is kept until the next starts, and asked of any other conversation
  there is none.
- **The launcher** (`run_launcher::tests`, one): status 0 and status 80
  are both the answer, 80 says how the run ended, and 81 is a failure.
- **The chat** (`chat_run::tests`, two): the chat says once which
  conversation a run answered in, and not for an answer out of steps,
  a failed run, an answer not shown, one that reached no one, or one
  after the owner started another conversation; and, with a real hub
  and a real page, a started idea is answered by its conversation's
  run, and not one set aside meanwhile or one whose conversation was
  followed by another.
- **The page** (`suggestions::tests`, two): only a started idea may
  have a conversation; it is answered once, in its own conversation,
  with another started idea left alone; an answered idea is the
  owner's to start again, set aside or bring back; and with scheduled
  work off, or a page that can't be read, nothing is started or
  answered and the page is left as it was.
- **The owner's action** (`api_ops::tests`, one): a conversation for a
  started idea is kept as its own, with a conversation store and
  without; for an idea that waits, or isn't on the page, it starts for
  nothing.
- **The app** (one new, `live-ideas.test.mjs`): opening the page reads
  it again while there is a connection, and not before or after.
- **Widened**: the wire's pinned actions and states, the four Rust
  version tests and the app's (v29); the app's tests of starting an idea, of how an
  idea is shown, and of the new-conversation action.

## Breaks by hand

Thirty-two in Rust, each one part of the change taken out or bent, the
tests run on each, and each failing test named (`unit-breaks.log`,
`unit-*.log`, `ia_break.py`). All thirty-two were caught, and the tests
passed before and after.

| Break | Caught by |
|---|---|
| The status is a failure's code | the status test; the chat's test |
| Any status but 0 says the steps ran out | the status test; the launcher's test; the chat's test |
| An answer on the last step says it had steps to spare | the loop's test; the binary's test |
| It says so with no last step told | the loop's test |
| Any answer of a run told of a last step says so | the loop's test; the binary's test |
| An answer held until no step was left says it had steps to spare | the loop's test |
| A run's answer forgets what the loop said | the agent's answer test; the binary's test |
| The agent exits 0 whatever its steps | the binary's test |
| The agent exits 80 whatever its steps | the binary's test |
| The status is 0 either way | the agent's status test; the binary's test |
| Status 80 is a failed run | the launcher's test |
| The run doesn't say how it ended | the launcher's test |
| Any status comes with an answer | the launcher's test |
| An action for no idea says so on the wire | the wire's pinned test |
| Answered has another word on the wire | the wire's state test |
| A plain conversation keeps the idea of the one before | the hub's test; the action's test; the chat's page test |
| Any conversation is the idea's | the hub's test; the page's test |
| A conversation for an idea is the one already open | the hub's test; the action's test; both page tests |
| What a conversation was started for isn't kept | the hub's test; the action's test; the chat's page test; both page tests |
| An idea that waits may have a conversation | the action's test; the page's test |
| Any started idea lets any id have a conversation | the action's test; the page's test |
| The owner's action forgets the idea | the action's test |
| An idea that isn't started has the conversation | the action's test |
| A conversation for an idea starts for nothing | the action's test |
| An answer given as the steps ran out answers the idea | both chat tests |
| Only an answer given as the steps ran out answers the idea | both chat tests |
| The chat says it again each time it is asked | the chat's test |
| The chat's turn doesn't tell the page | the chat's page test |
| An idea set aside is answered all the same | the chat's page test; the page's test |
| The first started idea is answered, whichever it was for | the page's test |
| An answered idea is left started | the chat's page test; both page tests |
| An answered idea is said to be answered and not kept | the chat's page test; both page tests |

Ten in the app, run on Windows against the three test files that cover
it (`app-breaks/breaks.log`, `app-breaks/*.log`, `ia_app_break.py`).
All ten were caught.

| Break | Caught by |
|---|---|
| The conversation is started for no idea | the conversation's test; the sequence's test |
| The conversation is named by the idea's text | the sequence's test |
| The broker isn't told which idea | the action's test |
| A conversation for no idea names one all the same | the action's test |
| The adapter drops the idea | the action's test |
| An answered idea is called completed | how an idea is shown; the opening test |
| An answered idea doesn't say so | how an idea is shown; the opening test |
| The page is read with no connection | the opening test |
| An open page is read only with no connection | the opening test |
| Opening the page reads nothing | the opening test |

## Mutation

Every mutant cargo-mutants makes on the lines changed against the base,
in each crate, with that crate's own tests (`mut.log`, `mut-*`). None
was missed and none timed out.

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 3 | 3 | 0 | 0 | 0 |
| `hatchling-loop` | 3 | 1 | 0 | 0 | 2 |
| `hatchling-channel` | 6 | 6 | 0 | 0 | 0 |
| `hatchling-agent` | 9 | 7 | 0 | 0 | 2 |
| `hatchling-broker` | 28 | 23 | 0 | 0 | 5 |

Commands, from `mut.log`: `cargo mutants --in-place --in-diff
ia-slice-<crate>.diff --timeout <60|120|180> -o <out> -- --lib`, and
for the agent `-- --lib --test local_model`.

Unviable, each a build failure:

- `crates/hatchling-loop/src/runner.rs:73:9: replace LoopResult::of -> Self with Default::default()`: unviable, build failure.
- `crates/hatchling-loop/src/runner.rs:330:9: replace Loop<'t, 'r, 'm>::run_with_evidence -> Result<LoopResult, Error> with Ok(Default::default())`: unviable, build failure.
- `crates/hatchling-agent/src/model.rs:312:9: replace <impl From<LoopResult> for RunAnswer>::from -> Self with Default::default()`: unviable, build failure.
- `crates/hatchling-agent/src/model.rs:335:5: replace run_command -> Result<RunAnswer, Error> with Ok(Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/api_ops.rs:253:9: replace Ops<'a>::handle -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/api_ops.rs:778:9: replace Ops<'_>::new_conversation -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/chat_run.rs:329:9: replace Chat<R, K>::resuming -> Self with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/chat_run.rs:438:9: replace Chat<R, K>::step -> Step with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/chat_run.rs:639:5: replace chat_turn -> Step with Default::default()`: unviable, build failure.

No equivalence is claimed: there was no missed mutant to explain.

## The gate

On each toolchain, from a cleared build cache (`gate.log`,
`gate-1.*.log`): `cargo fmt --all -- --check`, `cargo clippy
--workspace --all-targets --locked -- -D warnings`, the same with
`--all-features`, `cargo build --workspace --locked`, `cargo test
--workspace --locked --no-fail-fast`, `hatchling-dev seed-check` and
`hatchling-dev protocol-schema`.

| | 1.85.0 | 1.98.1 |
|---|---|---|
| Every step | exit 0 | exit 0 |
| Test binaries with a result | 135 | 135 |
| Tests passed | 3945 | 3945 |
| Tests failed | 0 | 0 |
| Lines saying skipped | 28 | 28 |

As root, outside the sandbox's jail (`root/as-root.log`): 201 tests in
core, 28 in the loop, 34 in the channel, 186 in the agent with 10 in
its local-model test and 10 in its loop test, and 116 in the broker's
modules this touches. All passed.

The app's own check (`frontend-check.log`, `npm run check`: types,
lint, the frozen-file check, 500 unit tests, coverage of the live
adapter at 100%) passed on the tested tree.

## Found on the way

- **A gap a break showed before it ran.** With two ideas started, a
  broker that marked the first started idea on the page, and not the
  one the conversation was for, would have passed every test as first
  written. The page's test now starts a second idea ahead of the
  first, and the break is caught.
- **A test too long for the minimum toolchain.** On tree `8ab5dbd3…`
  everything passed but clippy on 1.85.0, which counted one new test
  as too complex (26, where 25 is allowed). The test was split in two,
  with no exemption, and every run was made again on the tree above.
- **Two functions over their length.** The first build's clippy found
  the loop's and the chat's step functions past 100 lines with this
  change in them. The loop's result is now made by a small
  constructor, and the chat's check of a run's ending by a function of
  its own.
- **The disk guard.** The first mutation run on tree `8ab5dbd3…` was
  stopped in the broker's crate when the sandbox's disk passed its
  limit: the build cache held two toolchains' builds. The cache is now
  cleared before the mutation run, and the run was made again whole.
- **A break's guard named the wrong test.** One Rust break was caught
  by the page's test and listed the chat's. It was reported as
  survived until the list named the test that catches it.
- **Two breaks that didn't build.** Two of the app's ten breaks failed
  to compile at first, so no test ran for them. They were written
  again to compile, and all ten are caught.

## What wasn't tried

- **An install.** No install has this change. That a real model's last
  step ends in status 80, that the app's button names the idea, and
  that the page shows the answered line were not seen on one.
- **The lines that join the parts.** The chat loop calling its turn,
  the app's navigation calling `opened`, and the button passing the
  idea's id are each one line, read and not run.
- **A page left open.** The broker doesn't tell the app when it marks
  an idea. A page that stays open while the answer arrives shows the
  change the next time it is opened, or the connection reopens.
- **A page that can't be written** when an idea is answered: the
  broker logs it and the idea stays started. No test makes that
  happen.
- **A scheduled run that exits 80.** The launcher's test shows such a
  run is an answer. No test takes one through the scheduler to its
  delivery.
- **A message that isn't the idea's.** Any answer in an idea's
  conversation marks the idea, and nothing here tries to tell them
  apart.
