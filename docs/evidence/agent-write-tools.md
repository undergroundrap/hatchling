# Evidence: the agent can write and edit the files in its computer

What was tested, how, and what came back, for a change that gives the
agent a `write` tool and an `edit` tool: a file made whole, or one exact
piece of a file's text replaced, in its own computer. Every number here
is read from a file in the results bundle, which is attached to the
release that carries this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `3f1cdb2d` (`feat(room): write and edit the Room's files from the spawner (#457)`) |
| Tested tree | `aa126315762480a8bcfd643dc42ba590f53f61b2` |
| Date | 2026-11-15 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's check on Windows 11 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-agent-write-tools-results.tar.gz`, 1211556 bytes, sha256 `4e1fba4f9df7b2b38939479bc8d9501cc4b46ab9ef99cb7bc47e308d1e6ad900` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What this is for

The Room's supervisor could be asked to write a file, or to replace one
exact piece of a file's text, and nothing in a chat could ask. To put
text in a file the agent ran a program. On a real install a small model
ran `echo` with a `>` among its arguments, which prints the words and
writes nothing with no shell in between, and spent its steps finding
out ([the search tool's record](agent-search-tool.md) has that chat).

## What changed

- **The session protocol is v26** (`hatchling-core`): `RoomOp::Write`
  carries a path, the file's text and whether a file there is to be
  replaced; `RoomOp::Edit` carries a path, the text to find, what to put
  in its place and whether every place is meant. `RoomResult::Wrote`
  and `RoomResult::Edited` say what was done; `RoomResult::NoChange`
  says why no change is known to have been made.
- **The broker** (`room_ops.rs`): takes a path that isn't absolute as
  one under `/workspace`; checks the path and the lengths before
  anything is asked; asks the spawner; and tells the session what the
  Room said. A change takes no job name and none of the session's count
  of commands, and is refused for a run with nobody there, as a command
  is.
- **The owner's Activity** (`run_trail.rs`): a file written and an edit
  made are done; a change not known to have been made is a failure.
- **The agent** (`hatchling-agent`): a `write` tool and an `edit` tool
  beside `run`, `read` and `search`, given wherever those are. Text too
  long for one write or one edit is refused in the agent's cell, by its
  count or by how it is written in a frame, before the broker is asked.
  The `run` tool's description now sends the model to them for a file.
- `docs/session-protocol.md`, `docs/arch/trust-boundary.md`, the
  changelog, and the app's generated protocol types.

## What a change can and can't be

Nothing here adds to what the agent could already do: a command it ran
could change the same files. What it takes away is the command line.

- **The same sessions that may run a command may write and edit**: a
  chat, with its person there. Any other run is refused before the Room
  is asked.
- **What makes a change safe to a reader of the file is in the Room's
  supervisor**: a file is never changed where it lies, a link is never
  followed, and only a file is written over. That was the change before
  this one, and its record is `room-write-wire.md`.
- **A change that got no answer is said as that.** The model is told
  the file may or may not have been changed, in the spawner's words,
  and to read it before changing it again.
- **The log says a file was changed**, and not which or with what.
- **An edit carries nothing of the file as the model last read it.** A
  file changed since is edited all the same, wherever the text to find
  is still there at one place.

## Tests

Eleven new, and three widened.

- **The wire** (`hatchling-core`, `messages::room_tests`, one): both
  ops and the three results in their written form; a switch that is off
  is left out of what is written; an op with no text, no path, a field
  that isn't named, or a switch that isn't true or false never decodes;
  and the protocol is 26.
- **The broker** (`room_ops::tests`, three): files are written and
  edited as the session names them, with what the Room said told as it
  came and no job name taken; each reason a change can't be made is
  said, what may not be asked for never is, and a run with nobody there
  or with no Room changes nothing; what is said of a change to the file
  with the longest path fits one frame.
- **The Activity** (`run_trail::tests`, one): how each of the three
  results ends.
- **The agent's tools** (`tools::tests`, four): their names,
  descriptions and schemas, and the `run` tool's pointer to them; what
  each asks the broker for each thing the model can give, and what it
  tells the model for a file written, one replaced, an edit made and a
  change not made; and that neither asks anything for what isn't a
  path, a text or a switch, or for text that is too long by its count
  or for a frame, while the most that may be written is asked for.
- **The agent's running programs** (`room::tests`, one): a change is no
  program, and one that was running is still running after it.
- **The agent's loop** (`tests/loop.rs`, one): the model is told it has
  the tools, its `write` and `edit` reach the broker, and the broker's
  answers come back to it.
- **Widened**: the spawner's change test asks through the trait a
  session's Room asks through; the search op's wire test and the app's
  protocol test expect version 26.

## Found on the way

- **A break that didn't build.** In the first run of the breaks, on
  tree `dc558f76…`, one of the sixty-nine didn't compile, so no test
  ran for it and the runner called it survived. The break was at fault,
  not the code: it is fixed, and all sixty-nine are caught on the
  tested tree. Only the protocol document changed between that tree and
  the tested one, and every run in the bundle is of the tested tree.
- **A test added for a break that wasn't yet one.** Nothing checked
  that a change not made leaves the agent's count of running programs
  alone. The `room::tests` test above is for that, and one break is of
  it.

## Breaks by hand

Sixty-nine, each one part of the change taken out or bent, the tests of
the three crates run on each, and each failing test named
(`unit-breaks.log`, `unit-*.log`, `wt_break.py`). All sixty-nine were
caught, and the tests passed before and after.

| Break | Caught by |
|---|---|
| The write op has another name on the wire | the wire's test |
| The edit op has another name | the wire's test |
| `replace` left off is written | the wire's test |
| A write must say whether it replaces | the wire's test; the write tool's asking test; the loop's test |
| `all` left off is written | the wire's test |
| An edit must say whether it is of every place | the wire's test; the edit tool's asking test; the loop's test |
| What was written has another name | the wire's test |
| What was edited has another name | the wire's test |
| No change has another name | the wire's test |
| The protocol is still 25 | the wire's test; the search op's wire test |
| A write's path is taken as it came | the broker's naming test; its reasons test |
| An edit's path is taken as it came | the broker's naming test; its reasons test |
| A write is of nothing | the broker's naming test; its reasons test |
| Every write replaces | the broker's naming test |
| No write replaces | the broker's naming test |
| Every edit is of every place | the broker's naming test |
| No edit is of every place | the broker's naming test |
| An edit's texts are the wrong way round | the broker's naming test; its reasons test |
| A change is asked unchecked | the broker's reasons test |
| What is no change is refused in other words | the broker's reasons test |
| A written file is said to be empty | the broker's naming test |
| No file is said to have been replaced | the broker's naming test |
| An edit is said to be at one place | the broker's naming test |
| An edit is said to start on the first line | the broker's naming test |
| An edited file is said to be empty | the broker's naming test |
| A change not made is a refusal | the broker's reasons test |
| With no Room a change is a failed command | the broker's reasons test |
| With no Room only a write is no change | the broker's reasons test |
| With no Room only an edit is no change | the broker's reasons test |
| The spawner isn't asked for a session's change | the spawner's change test |
| A written file is a failure in the Activity | the Activity's test |
| An edit made is refused in the Activity | the Activity's test |
| A change not made is done in the Activity | the Activity's test |
| The write tool has another name | the tools' description test; the loop's test |
| The edit tool has another name | the tools' description test; the loop's test |
| The `run` tool doesn't point at `write` and `edit` | the tools' description test |
| The write tool doesn't say its limit | the tools' description test |
| The write tool doesn't point at `edit` | the tools' description test |
| The write tool doesn't say no command is run | the tools' description test |
| The edit tool doesn't say how to replace every place | the tools' description test |
| The edit tool doesn't say line numbers aren't in the file | the tools' description test |
| A write need not give its text | the tools' description test |
| An edit need not give its new text | the tools' description test |
| A write takes what it doesn't name | the tools' description test |
| An edit takes what it doesn't name | the tools' description test |
| `replace` is described as text | the tools' description test |
| `all` is described as text | the tools' description test |
| A write of too much is asked | the asks-nothing test |
| The most a write may be is refused | the asks-nothing test |
| An edit of too much is asked | the asks-nothing test |
| The most an edit's text may be is refused for its count | the asks-nothing test |
| Only an edit's old text is counted | the asks-nothing test |
| An edit's new text is called old | the asks-nothing test |
| Too much is said with its counts the wrong way round | the asks-nothing test |
| A change too long for a frame is asked | the asks-nothing test |
| A write never says to replace | the write tool's asking test; the asks-nothing test |
| A write is asked with no text | the write tool's asking test; the asks-nothing test |
| An edit never says every place | the edit tool's asking test; the asks-nothing test |
| An edit is asked the wrong way round | the edit tool's asking test |
| The model is told no file was replaced | the write tool's asking test |
| The model isn't told the file's length | the write tool's asking test |
| The model is told places and line the wrong way round | the edit tool's asking test |
| The model isn't told the edited file's length | the edit tool's asking test |
| A change not made is told as done | both tools' asking tests |
| A change not made is told in the Room's words alone | both tools' asking tests |
| A change not made forgets every running program | the running programs' test |
| The write tool isn't given to the agent | the loop's test |
| The edit tool isn't given to the agent | the loop's test |
| The model isn't told it can write and edit | the loop's test |

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`, `mut-*-outcomes.json`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 0 | 0 | 0 | 0 | 0 |
| `hatchling-agent` | 28 | 23 | 0 | 0 | 5 |
| `hatchling-broker` | 6 | 1 | 0 | 0 | 5 |

Core's change is types and a constant, with nothing in it to mutate;
`mut-core.log` says so. The broker's change is mostly values put into a
structure and arms of a match, which mutation makes little of: the
twenty-three breaks by hand in the broker and the Activity are what
test it.

Each crate's `outcomes.json`, by sha256:

- agent: `d7cf386eae0bdab3d798998c578726127780bdb4be18a0cc77c4cab45031f579`
- broker: `8307d0972868c80807f9b3f93619bb0ca5f6c0816da6bc8f29c862bc0c1066ed`

The unviable ones:

- `tools.rs:1499:5: replace too_much -> LoopError with Default::default()`: unviable, build failure
- `tools.rs:1511:5: replace change_asked -> Result<ToolOutput, LoopError> with Ok(Default::default())`: unviable, build failure
- `tools.rs:1567:9: replace <impl Tool for WriteTool<'_>>::execute -> Result<ToolOutput, LoopError> with Ok(Default::default())`: unviable, build failure
- `tools.rs:1635:9: replace <impl Tool for EditTool<'_>>::execute -> Result<ToolOutput, LoopError> with Ok(Default::default())`: unviable, build failure
- `tools.rs:1674:5: replace room_output -> ToolOutput with Default::default()`: unviable, build failure
- `run_trail.rs:619:5: replace room_ending -> Ending with Default::default()`: unviable, build failure
- `room_ops.rs:86:9: replace <impl Rooms for crate::spawner::Spawner>::change -> Result<RoomChanged, String> with Ok(Default::default())`: unviable, build failure
- `room_ops.rs:184:9: replace RoomSession<'r>::handle -> RoomResult with Default::default()`: unviable, build failure
- `room_ops.rs:398:5: replace change_of -> Option<RoomChange> with Some(Default::default())`: unviable, build failure
- `room_ops.rs:430:5: replace changed -> RoomResult with Default::default()`: unviable, build failure

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3915 passed, 0 failed | exit 0: 135 binaries, 3915 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The tests of what the change touches were run as root too, as CI runs
them: 197 in core, 181 in the agent and 10 in its loop, 38 in the broker
(`root/as-root.log`).

The app's own check (`npm run check`: types, lint, 493 unit tests, and
coverage of the live adapter at 100%) passed on the same tree, on
Windows (`frontend-check.log`). The app's code didn't change: only its
generated protocol types and the version its test expects.

## What wasn't tried

- **A chat that writes a file in an installed Room.** Every test is
  against a stand-in for the model, the broker or the spawner. The
  Room's side was proven in a real Room in the change before this one;
  the whole path, from a model's `write` to a file on the room disk,
  hasn't run yet.
- **Whether a small model reaches for `write` and `edit`** where it
  used to run `echo`, whether it copies the text to find exactly, and
  what it does when told the text is at more than one place.
- **Two writers.** An edit carries nothing of the file as the model
  last read it, and no test changes a file between a read and an edit.
- **The app showing a change.** A change is counted in the Activity as
  a use of the Room, as a command is; no page was opened to look at
  one.
- **What writing costs a long chat.** A write's whole text, up to
  32 KiB, stays in the model's history as the call it made; nothing
  here measures a chat with many.
