# Evidence: what the owner sends while a run works reaches it

What was tested, how, and what came back, for a change that gives a chat
run the messages its owner sends while it is working. Every number here
is read from a file in the results bundle, which is attached to the
release that carries this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `3f89c004` (`docs(install): say any other WSL distro can block an unlock, and how to clear it (#448)`) |
| Tested tree | `6698ea121a6c06d14661db0237ab987dc98cba9f` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's checks on Windows 11 with Node 22.17.0 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-chat-follow-ups-results.tar.gz`, 2300856 bytes, sha256 `07bd244b631025ac992be43ce1911b95e1d7cd12c52f8f93595e9ebf67b965c8` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

The chat answers one message at a time. A message the owner sent while
an answer was on its way waited in the broker's queue, and became a run
of its own after that answer. The run working on the first message never
saw it: a correction sent a moment after the request was answered
separately, after the thing it corrected had been done.

The app's activity view also paired answers wrongly when two messages
were waiting: the next answer was shown against the newest message, and
the answer after it against none.

## What changed

- **The wire** (session protocol v24): a session op of a new kind,
  `SessionRequest::Chat(ChatOp::More {})`, answered with
  `ChatResult::More { messages }`; and an event for the owner's app,
  `UiEventBody::MessagesJoined { count }`.
- **The local hub** (`hatchling-channel`): `LocalHub::take_if` takes the
  oldest waiting message only if it is wanted, by its words and the
  conversation it was sent in. One that isn't stays first in the queue.
- **The broker** (`follow_ups.rs`): the chat names its run in flight, by
  its cell, when it starts it, and forgets it when it ends. A session
  that asks is given the owner's waiting messages only if it is that
  run's, only those sent in the conversation the run's own message was
  sent in, and at most 48 KiB of them at once as JSON writes them. What
  is given is off the queue. When the run ends the chat keeps it, with
  the run's own message, as what the owner said in that exchange. The
  app is told each time any were taken, and how many.
- **The loop** (`hatchling-loop`): `Loop::with_more` is asked before
  each turn of the model, and each message joins the history as the
  person's own, where the history then stands.
- **The agent**: a chat run asks; no other run does. A chat run's
  instructions say what a further message is and what to do with it.
- **The app**: an answer is the oldest waiting task's. Messages a run
  took are no tasks of their own.
- `docs/session-protocol.md` (v24) and the changelog.

## What the change does not do

- **It interrupts nothing.** A run is asked between its steps. A message
  that arrives while the model is writing its answer waits for the next
  run, as before.
- **It gives no session anything new to hold.** No grant is involved:
  the run in flight already holds the owner's message as its task, and
  these are more of the same, from the same person, in the same
  conversation. A scheduled run, a subagent, and a chat run that has
  ended are answered with no messages and learn no more than that.
- **It doesn't change the queue's bounds.** The hub still holds at most
  64 messages of at most 16 KiB each.

## Something the tests found

A session request is read strictly: an op refuses a field it doesn't
name. The new op has no fields, and was first written as a variant
without any. Written so, it was read with whatever field it was sent:
`{"kind":"chat","op":"more","cell":"x"}` decoded. The test of its wire
form caught it, and the op is now written as a variant with an empty
set of fields, which refuses one. No older op is written without
fields, so no older op had this.

The schema check passed either way: it reads the schema, which said the
op was closed, and not what the decoder does.

## Tests

Eighteen in Rust, one in the app.

- **The wire** (`hatchling-core`):
  `messages::room_tests::the_chat_op_and_its_reply_have_their_wire_form`:
  both forms as the protocol document writes them, an op with a field it
  doesn't name refused, and the 48 KiB bound. That a reply of that much
  fits a frame is checked when the crate is compiled.
- **The hub** (`hatchling-channel`):
  `hub::tests::the_oldest_message_is_taken_only_if_it_is_wanted`.
- **The broker's own record** (`follow_ups::tests`, six):
  a run in flight is given what was sent, once, oldest first, and all of
  it is handed over at its end; another cell is given nothing; a message
  of another conversation is left where it is; what is given at once
  fits a frame, and what doesn't fit waits for the next asking; the runs
  in flight are bounded; and the process's own record is asked by cell.
- **The session**
  (`serve::gate_scope_tests::the_chats_run_in_flight_is_given_the_owners_further_messages`):
  a real session is served over a socket; the named run's cell is given
  the messages across four askings and the app is told "2" then "1";
  a cell the chat didn't name is given nothing, the app is told nothing,
  and the three messages still wait.
- **The trail and the app's stream**
  (`run_trail::tests::a_chat_run_tells_the_app_the_messages_it_took`,
  `app_stream::tests::messages_a_run_took_are_told_to_the_app_once_one_is_attached`):
  told when any were taken, by a chat run only; asking is no tool's use
  and no step.
- **The chat** (`chat_run::tests`, two): what the owner sent while a run
  worked is kept with its exchange, a blank line between; a run that has
  ended, or failed, is given no more; a chat not told where messages
  wait names no run.
- **The loop** (`runner::tests`, two): asked before each turn, the
  messages join the history after what the run has done, as the
  person's own; with no turn to follow, nothing is asked.
- **The agent** (`chat::tests`, two;
  `cell::tests::a_chat_run_takes_what_its_owner_sends_while_it_works`):
  the op and its failures; a cell's own loop against a stand-in broker,
  where a chat run asks before each of its two turns and is shown both
  messages, and a run that isn't a chat's asks nothing.
- **The app** (`live-adapter.test.mjs`): "an answer is the oldest
  waiting task's, and the messages its run took into it are no tasks of
  their own". The adapter's tests are at 100% of lines, branches and
  functions.

## Breaks by hand

**Twenty-seven in Rust**, each one part of the change taken out or bent,
the tests of the crates it touches run on each, and each failure read
(`unit-breaks.log`, `unit-*.log`, `fu_break.py`). All were caught, and
the tests passed before and after.

| Break | Caught by |
|---|---|
| The op is read with whatever field it is sent | the wire's test: the extra field decoded |
| A reply may carry more than was said | the wire's test: 50176 for 49152 |
| The oldest message is taken, wanted or not | the hub's test, and two of the record's |
| The newest is asked about, and the oldest taken | the hub's test |
| A message of another conversation is given to the run | the record's test of conversations |
| A message is given whatever room is left | the record's test of the frame |
| What is given doesn't take up room | the record's test of the frame: four given for three |
| What a run was given isn't remembered | the record's, the chat's and the session's tests |
| A run that ended is still in flight | the record's and both of the chat's: "the run has ended" |
| Any cell but the run's is given | nine tests, the record's "another cell" among them |
| A run begun again keeps what the last was given | the bound's test: "begun again, given none" |
| The runs in flight aren't bounded | the bound's test: "the oldest was dropped" |
| The session gives the run nothing | the session's test |
| The session doesn't tell the app | the session's test |
| A run that isn't the chat's tells the app | the trail's test: "not a chat run" |
| Taking none is told | the trail's and the session's tests |
| The app is told another number | the app stream's, the trail's and the session's tests |
| The chat doesn't name its run | both of the chat's tests |
| The chat doesn't ask what its run was given | both of the chat's tests |
| What was given is kept without a blank line between | the chat's test |
| The loop never asks | both of the loop's tests, and the cell's |
| What the person sent is shown as the model's words | the loop's test, and the cell's |
| The loop's debug form doesn't say whether it asks | the loop's second test |
| Every run asks, a chat's or not | the cell's test: the other run was given the messages |
| No run asks | the cell's test |
| A chat run isn't told what a further message is | the cell's test |
| The op hands back nothing | the op's test, and the cell's |

**Eight in the app**, each still building (`app/fu-fe-breaks.log`,
`app/fu_fe_break.py`). All were caught by the adapter's new test.

| Break | What the test said |
|---|---|
| An answer is the newest waiting task's | the newest task was shown answered |
| A step is the newest waiting task's | the newest task said "Using its computer" |
| Tasks a run took stay threads of their own | the list of threads wasn't the one expected |
| Tasks a run took still wait for answers | a task was still running where it should have been answered |
| The task being answered is taken with the rest | no thread where the first task's was expected |
| Taking messages isn't the task's news | "taking them is the task's latest news": its time hadn't moved |
| Taking none is news all the same | its time had moved, 09:10 for 09:00 |
| The adapter drops what the broker said was taken | the list of threads wasn't the one expected |

The first run of the app's breaks had one that didn't build; it was
replaced by the sixth above, and the run in the bundle is the second.

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`, `mut-*-outcomes.json`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 2 | 2 | 0 | 0 | 0 |
| `hatchling-channel` | 3 | 2 | 0 | 0 | 1 |
| `hatchling-loop` | 3 | 1 | 0 | 0 | 2 |
| `hatchling-agent` | 17 | 3 | 0 | 0 | 14 |
| `hatchling-broker` | 46 | 36 | 0 | 0 | 10 |

Each crate's `outcomes.json`, by sha256:

- core: `2f8ba12ff43b9e4e4b7a3621338114d7545d623d2bc8dd7724b3ea1d38eb4211`
- channel: `2f51d206c3903d046e54b9c85344f816b8b9d10f97553e951748cec9f50e1fab`
- loop: `1a83c4b314070b968031a89c83a250d7a6b103a42ca81278b8b2bdcdc7286010`
- agent: `c1f6c694aced8719670efd794539acbb28ad8e808719084027a6e8881b80cf96`
- broker: `88e8d5bae9e4700087f76be6929a1ecb75a8ab4cdd2150d03c91707953302971`

An earlier run, on a tree without the wire's own test, missed the two
in `hatchling-core`: the bound written as `48 * 1024` with `+` or `/`
for `*`. Only the broker's tests held the bound then, and a crate's
mutants are run against its own tests. That test was added, and it is
the one that found the op read with any field. Every run in the bundle
is of the tree after both.

The unviable ones:

- `hub.rs:163:9: replace LocalHub::take_if -> Option<Message> with Some(Default::default())`: unviable, build failure
- `runner.rs:127:9: replace Loop<'t, 'r, 'm>::with_more -> Self with Default::default()`: unviable, build failure
- `runner.rs:265:9: replace Loop<'t, 'r, 'm>::run_with_evidence -> Result<LoopResult, Error> with Ok(Default::default())`: unviable, build failure
- `browser.rs:114:5: replace request -> Result<BrowserResult, Error> with Ok(Default::default())`: unviable, build failure
- `cell.rs:525:9: replace Cell<Ready>::run_loop_delegating -> Result<hatchling_loop::LoopResult, hatchling_loop::Error> with Ok(Default::default())`: unviable, build failure
- `channel.rs:211:9: replace ChannelClient<'s>::request -> Result<ChannelResult, Error> with Ok(Default::default())`: unviable, build failure
- `chat.rs:59:5: replace frame_error -> Error with Default::default()`: unviable, build failure
- `egress.rs:211:5: replace request -> Result<EgressResult, Error> with Ok(Default::default())`: unviable, build failure
- `goals.rs:121:5: replace query -> Result<QueryPage, Error> with Ok(Default::default())`: unviable, build failure
- `goals.rs:228:5: replace write -> Result<GoalsResult, Error> with Ok(Default::default())`: unviable, build failure
- `ideas.rs:24:5: replace request -> Result<ArtifactResult, Error> with Ok(Default::default())`: unviable, build failure
- `memory.rs:392:5: replace request -> Result<MemoryResult, Error> with Ok(Default::default())`: unviable, build failure
- `room.rs:90:5: replace ask -> Result<RoomResult, Error> with Ok(Default::default())`: unviable, build failure
- `skill.rs:94:5: replace request_frame -> Result<SkillResult, Error> with Ok(Default::default())`: unviable, build failure
- `subagent.rs:25:5: replace request -> Result<SubagentResult, Error> with Ok(Default::default())`: unviable, build failure
- `vault.rs:110:5: replace request -> Result<VaultResult, Error> with Ok(Default::default())`: unviable, build failure
- `web.rs:46:5: replace request -> Result<WebResult, Error> with Ok(Default::default())`: unviable, build failure
- `chat_run.rs:322:9: replace Chat<R, K>::resuming -> Self with Default::default()`: unviable, build failure
- `chat_run.rs:372:9: replace Chat<R, K>::with_follow_ups -> Self with Default::default()`: unviable, build failure
- `chat_run.rs:422:9: replace Chat<R, K>::step -> Step with Default::default()`: unviable, build failure
- `follow_ups.rs:131:5: replace shared -> MutexGuard<'static, FollowUps> with MutexGuard::new()`: unviable, build failure
- `follow_ups.rs:131:5: replace shared -> MutexGuard<'static, FollowUps> with MutexGuard::from_iter([Default::default()])`: unviable, build failure
- `follow_ups.rs:131:5: replace shared -> MutexGuard<'static, FollowUps> with MutexGuard::new(Default::default())`: unviable, build failure
- `follow_ups.rs:131:5: replace shared -> MutexGuard<'static, FollowUps> with MutexGuard::from(Default::default())`: unviable, build failure
- `run_trail.rs:335:5: replace tool_of -> Option<ToolUse> with Some(Default::default())`: unviable, build failure
- `run_trail.rs:375:5: replace ending_of -> Ending with Default::default()`: unviable, build failure
- `serve.rs:1172:5: replace oversize_reply -> SessionResponse with Default::default()`: unviable, build failure

Most of the agent's are in files the change touched only to name the new
kind of reply in a list of every kind.

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3844 passed, 0 failed | exit 0: 135 binaries, 3844 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The library tests of core, the channel, the loop and the agent, and the
broker's tests of what the change touches, were run as root too, as CI
runs them: 483 passed (`root/as-root.log`).

The app: `npm run check` exit 0; 493 unit tests, 0 failed; the live
adapter at 100% (`app/fu-frontend-check.log`).

## On a real install

Added on 2026-11-14, after the change was merged (`8174448e`), on an
install updated by hand to main `2993ce29`. The file is in a second
bundle,
`hatchling-unlock-and-follow-ups-real-install-results.tar.gz`, 1843 bytes, sha256
`f61850b88da1adcb419097ec5b622d9b64a35beaec5eba0d0137728a7400bd4e`.

The model was gemma4:12b, on that machine. Typed into the app's chat:
"In your computer, run this one command and tell me what it printed:
sleep 15; echo first-done". Thirty seconds later, while the working
line said "Using its computer": "Also, after that, run: echo
second-done  and tell me what that printed too."

- **One run answered both.** The broker's activity log has one run
  summary for those minutes, with two uses of the agent's computer and
  four calls to the model, and its log has one chat run started. No run
  was started for the second message.
- **One answer came**, under both messages in the chat: "The second
  command printed: second-done".
- **The working line** went from "Using its computer" at 30 seconds to
  "Asking the model" at 31, counted from the first message, and to the
  answer at 34.

So the second message reached the run that was working on the first,
and that run did what it asked. Its answer said what the second command
printed and not what the first did. Both results were in front of the
model, and its instructions say to answer everything that was sent:
that it didn't is the model's wording, and is what a real model was
tried for.

It is one run, of one model.

## What wasn't tried

- **Another model**, or this one more than once. How well a model
  folds a further message into its answer is the model's to do; with
  this one, once, the work was done and the answer left half of it out.
- **The app's activity view** during that run: the chat was watched,
  and the view beside it wasn't.
- **A run that fails after taking messages.** What it took is gone with
  it, as its own message is; the owner is told the run failed, and not
  that those messages went with it.
- **A message too long for any reply**: more than 48 KiB as JSON writes
  it, which sixteen thousand control characters are. It is never given
  to a working run and waits for a run of its own; while it is first in
  the queue, nothing behind it is given either. No test sends one.
