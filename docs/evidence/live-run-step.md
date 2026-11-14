# Evidence: the chat says what a run is doing

What was tested, how, and what came back, for a change that has the
broker tell the owner's app each step a chat run starts, and the chat
say it with the time since the owner asked. Every number here is read
from a file in the results bundle, which is attached to the release that
carries this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `76e0d1cf` (`fix(broker): say a slow model is slow, not that it can't be reached (#443)`) |
| Tested tree | `a9127456ff6d38fa52e947726ed1fc5cc9d5b765` |
| Date | 2026-11-14 (UTC) |
| Machines | The broker's runs: x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox. The app's: Windows 11, Node 22.17.0 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-live-run-step-results.tar.gz`, 587302 bytes, sha256 `3658876713bed56a3d33daee4fcae38dd7498efee4fc95d19c0088a6a5373de8` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

While an answer was on its way, the chat said "Working on it" and
nothing else. On a real install with a slow hosted model that was one to
three minutes of it for an ordinary question. The app learnt what a run
had done only when it ended, from the one summary the broker writes
then.

## What changed

- **The wire** (session protocol v23): `UiEventBody::AgentStep { tool? }`,
  the step a working agent just started. `tool` is the tool in use, by the
  name a run's summary already gives it (`ToolUse`); it is absent while
  the run waits for its model. It says what kind of thing is being done
  and never what with.
- **The broker**: a session's trail tells the owner's app each step as
  the request for it comes in, and a model call as it goes. Only a change
  of step is told, and only by a chat run. It goes to the app alone and
  isn't written to the activity log.
- **The app**: the live adapter keeps the step against the task that is
  waiting for its answer. The chat's working line says it in the three
  words a status title has, with the time since the owner asked beside
  it, counted up each second by an element of its own.

What isn't shown, on purpose: the model's own thinking, which isn't the
answer and hasn't been through the checks an answer goes through; and a
progress bar or a percentage, because nothing knows how long a model
will take.

## The broker's tests

Four, in the broker.

- `app_stream::tests::a_step_is_told_to_the_app_once_one_is_attached`
- `run_trail::tests::a_chat_runs_steps_are_told_as_they_change`: a change
  of step is told and a repeat isn't; a run that isn't a chat run tells
  nothing; a trail with no app goes on as it did.
- `serve::gate_scope_tests::a_chat_sessions_steps_are_told_to_the_app_as_it_is_served`:
  a real session, served over a socket: its requests are told as steps
  in order, a repeated one once, and a session that isn't a chat run
  tells none.
- `serve::gate_scope_tests::a_chat_runs_model_call_is_told_to_the_app_as_it_goes`:
  told before its answer comes or doesn't; a listing isn't a model call.

### Breaks by hand

Nine, each one part of the change taken out or bent, the tests of what
it touches run on each, and each failure read (`unit-breaks.log`,
`unit-*.log`). All nine were caught, and the 80 tests passed before and
after.

| Break | Caught by |
|---|---|
| Any run tells its steps, a chat run or not | the trail's test ("not a chat run") and the session's |
| A step is told each time, changed or not | the trail's test and the session's |
| The step told isn't remembered | the trail's test and the session's |
| The trail keeps its steps to itself | the trail's, the session's and the model call's |
| A model call isn't told | the model call's test |
| Any request through the egress is told as the model | the model call's test: "a listing isn't a model call" |
| The serving loop tells no tool | the session's test |
| A session's trail has no app to tell | the session's test |
| The step reaches the app as another | the stream's test, the trail's and the session's |

### Mutation

`cargo mutants --in-place --in-diff` on the change, each crate's library
tests on each mutant (`mut.log`, `mut-broker-outcomes.json`, sha256
`443c0a62194cd4a7f81b70e0b6e41a7ad6345edc89f591ffac014f7b0eb23be2`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-broker` | 9 | 7 | 0 | 0 | 2 |
| `hatchling-core` | 0 | | | | |

The core crate's change is one variant of a type and a version number,
which mutation has nothing to alter in. The unviable ones:

- `run_trail.rs:116:9: replace Trail::telling -> Self with Default::default()`: unviable, build failure
- `serve.rs:1376:5: replace handle_egress_op -> Result<EgressResult, Error> with Ok(Default::default())`: unviable, build failure

### The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3815 passed, 0 failed | exit 0: 135 binaries, 3815 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

`protocol-schema` passing says the schema the app's types are generated
from is the one these types give. The tests of what the change touches
were run as root too, as CI runs them: 80 passed (`root/as-root.log`).

`quick.log` in the bundle is of an earlier tree,
`875abd8b40a8abcadf6bc2bc4e5e6fcd4596397c`: one lint was cleared after
it, and the changelog, the style guide and the end-to-end check were
written. The breaks, the mutants and the gate are of the tested tree.

## The app's tests

`npm run check` on the tested tree (`app/lv-frontend-check.log`): exit 0.
It is the type check, the lint, the design check, the generated types
against the schema, the format check, and the unit tests with their
coverage floors: 492 passed, and the live adapter's own 177 at 100% of
lines, branches and functions, as that gate requires.

Two tests are new or changed:

- `live-adapter.test.mjs`, "a waiting task says the step its run last
  started…": before the broker has said, "Working on it"; with no tool
  named, "Asking the model"; each of the 22 tools the schema names in
  words of its own, no two alike, each within a status title's three
  words; the answer ends it; a step with no task waiting changes
  nothing.
- `elapsed.test.mjs`: the time is said as `45 s`, `2 min 5 s`,
  `1 h 4 min`.

### Breaks by hand

Five, each still building (`app/lv-fe-breaks.log`). All five were caught
by the live adapter's test.

| Break | What it said |
|---|---|
| Every step is the model, whatever tool was named | "Asking the model" where "Using its computer" was expected |
| A tool is said as another | "Using the vault" for "Using its computer" |
| Two tools read alike | 21 different labels where 22 were expected |
| A label of four words | "Waiting for the model" for "Asking the model" |
| The adapter drops the step | "Working on it" for "Asking the model" |

Three more were written and didn't build: the type checker refuses a
step taken with no task waiting, and code left unused. They are the
compiler's to catch, and it did.

## In the real app

Added on 2026-11-14, after the change was merged (`32828684`). The
files are in a second bundle,
`hatchling-live-run-step-real-app-results.tar.gz`, 166818 bytes, sha256
`5276958fa22bb1959470b6a21a28ca423435be93c9e7f725f0cd8b493253fee2`.

**In CI.** The real app's end-to-end check, with the stand-in model made
slow, waited for the line to read "Asking the model", a dot and a number
of seconds, and passed, in the pull request's own run. The picture it
kept (`app-working-ci-pr444.png`) shows "Asking the model · 0 s" above
the message box, and the same step under the avatar in the panel beside
the chat. The browser suites, the smoke check and the agent's tests
passed in that run too.

**On a real install, with a real model.** An install under WSL2 was
updated to the merged tree and unlocked by its owner; the model was
gemma4:12b, on that machine. The line's text was read four times a
second while two things were asked in the chat
(`working-line-sampled.txt`):

| Asked | The line, in order | The answer |
|---|---|---|
| To run `sleep 12; ls -la /workspace` | Working on it · 0 s; Asking the model · 0 s to 17 s; Using its computer · 17 s to 26 s; Asking the model · 27 s to 28 s | "The command is still running. I will check the status in a few seconds." |
| To run `sleep 25; echo done`, and wait for it | Working on it · 0 s; Asking the model · 0 s to 7 s; Using its computer · 8 s to 32 s; Asking the model · 33 s to 34 s | "The command finished and printed: done" |

The time went up by one each second and carried on across each change
of step, as the time since the owner asked. The line went when the
answer came. Two pictures of the page are in the bundle, one of each
step.

One finding that isn't this change's: in the first run the model ended
its turn saying it would look again in a few seconds. Nothing looks
again: a turn that has ended is over, and the command's output was never
told. The working line showed the truth of it, a last call to the model
of a second or two and then an answer.

## What wasn't tried

- **A hosted model**, on this change: the slow waits it was made for
  were seen with one.
- **A scheduled run or a subagent.** Only a chat run tells its steps.
- **An app that reconnects in the middle of a run.** The step is among
  the events the app's stream keeps in memory and sends again (its most
  recent 4096), so it should be shown; that wasn't tried. Steps take up
  some of that room: a run tells one for each change of step.
- **A screen reader.** The time is hidden from assistive technology and
  the step is in a status region: that is what the markup says, and it
  wasn't listened to.
