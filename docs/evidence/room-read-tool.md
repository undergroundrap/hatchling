# Evidence: the agent reads a file in the Room from a chat

What was tested, how, and what came back, for the agent's `read` tool:
a page of a file in its own computer, asked for through its broker
session. Every number here is read from a file in the results bundle,
which is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `d704aea0` (`feat(room): hand back a page of a file from the Room (#436)`), with the change before this one (four of the broker's tests) and its record |
| Tested tree | `e02845fe3fb757754d8f18db5c01c3f03b15b01b` |
| Date | 2026-11-13 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's check on Windows 11 with Node 22.17.0 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-room-read-tool-results.tar.gz`, 608178 bytes, sha256 `65b1308ef5e4900995be62221d922687aeb00cb52681cfa980788d88ef2b8cc6` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What this is for

The agent could run a program in its computer, and read a file there
only by running one. It was shown the first 1 KiB and the last 3 KiB of
what `sed` or `head` printed, and each such command took the place of an
earlier one among the sixteen ended commands the Room remembers. The
change before the last gave the Room's supervisor a way to hand back a
page of a file with no command run. This change gives the agent a tool
that uses it.

## What changed

- `crates/hatchling-core`: `RoomOp::Read { path, from?, lines? }`,
  `RoomResult::Page` and `RoomResult::NoPage`, the ordinary count of
  lines (200), and the session protocol's version, now 22.
- `crates/hatchling-broker/src/room_ops.rs`: a session's read. Only a
  session with its person there may ask, as for a command. A path that
  isn't absolute is taken under `/workspace`; the first line is 1 and
  the count 200 unless the session says. What may not be asked for is
  refused before the Room is asked anything. The page is carried as it
  came, with bytes that aren't text shown as the replacement character.
  The log says a file was read, and not which.
- `crates/hatchling-broker/src/run_trail.rs`: a page that was read is a
  use that was done, and a file that couldn't be read one that failed.
- `crates/hatchling-agent`: the `read` tool, offered where `run` is. It
  puts each line's number and a tab in front of it and adds a note: the
  line to ask for next, that this is the end of the file, that the file
  is empty, or that a line too long for a page was cut. `run`'s
  description now sends the model to `read` for files, and the note of
  what the agent can do names it.
- The app: its generated protocol types, and the Activity's line for a
  use of the computer, now "Used its computer".
- `docs/session-protocol.md` (v22), the trust boundary page and the
  changelog say it.

## Decisions, and why

- **Who may read, and what.** The sessions that may run a command, and
  no others: a chat. Any file the Room's user can read may be asked
  for, since a command the same session ran could read it. Nothing is
  put to the owner first, for the reason no command is: the Room has no
  network, and nothing in it reaches the owner's machine or leaves the
  Room.
- **What the Activity keeps.** That a read was made and how it ended,
  not which file. In a chat the conversation shows it. Keeping the path
  is written on the trust boundary page, with a command's text, as a
  condition of giving these tools to any run without its person there.
- **Where the lines are numbered.** In the agent's cell, which is the
  least trusted part and the one that speaks to the model. The broker
  and the spawner carry the file's bytes and look into none of them.

## Each part taken out by hand

27 breaks. The tests they are run against passed unbroken before and
after: the broker's (29), the agent's library (162) and its loop test
(7), and the core's library (187). All 27 were caught by the test named
for each, and each failure was read.

In the broker (`room_ops::tests::` unless it says):

| Break | Test that fails |
|---|---|
| A run with nobody there reads a file | `what_may_not_be_asked_is_never_asked` |
| A path is asked for as it was given | `a_file_is_read_a_page_at_a_time` |
| Every path is put under the workspace | `a_path_that_is_not_absolute_is_under_the_workspace` |
| A read starts at line 2 | `a_file_is_read_a_page_at_a_time` |
| A read asks for every line there may be | the same |
| A read that may not be asked for is asked | `a_file_that_cannot_be_read_says_why` |
| Bytes that aren't text empty the page | `a_file_is_read_a_page_at_a_time` |
| A page is never said to go on | the same |
| A cut line isn't said to be cut | the same |
| The file's length isn't told | the same |
| A file that isn't there is a refusal | `a_file_that_cannot_be_read_says_why` |
| No Room to read in is told as nothing run | the same |
| A file that couldn't be read is counted done | `run_trail::tests::each_answer_about_the_room_ends_one_way` |
| A page that was read is counted refused | the same |
| A session asks the spawner for another line | `spawner::tests::a_page_reaches_the_session_and_starts_it` |

In the agent (`tools::tests::` unless it says) and the core:

| Break | Test that fails |
|---|---|
| The lines have no numbers | `the_read_tool_asks_the_broker_and_numbers_the_lines` |
| Every line has the first one's number | the same |
| A cut line is told as a whole one | `a_page_is_told_with_what_comes_after_it` |
| The end of a file is told as more to come | the same |
| An empty file is told as its end | the same |
| No page is told as an empty one | `the_read_tool_asks_the_broker_and_numbers_the_lines` |
| The first line asked for is dropped | the same |
| A count of lines too large to count is dropped | the same |
| `run` sends the model to `sed` for files | `the_read_tools_metadata_is_stable` |
| The model isn't told it can read | `capabilities::tests::the_note_says_it_has_a_computer_only_where_it_has` |
| The read tool isn't offered | `loop_asks_the_broker_to_read_a_file` (the loop test) |
| A read is 201 lines | `messages::room_tests::a_page_is_written_as_what_was_read` (core) |

`unit-breaks.log` sha256
`ada28b1cb993405853b8eb226a2920306484e284d9da55ffb8597f7f2175bdb0`.

## Mutation

`cargo mutants --in-place --in-diff <the crate's lines> --timeout <60 or 90>`,
each after an unmutated baseline, with only the mutated crate's tests:
`-- --lib` for `hatchling-core` (60) and `hatchling-broker` (90), and
`-- --lib --test loop` for `hatchling-agent` (90).

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 0 | | | | |
| `hatchling-agent` | 16 | 14 | 0 | 0 | 2 |
| `hatchling-broker` | 5 | 3 | 0 | 0 | 2 |

The core's change is message shapes, a constant and a version number:
"No mutants to filter" (`mut-core.log`). What it would have had mutants
for is held by the breaks above and by the tests of each message as it
is written.

Unviable, one line each:

- `crates/hatchling-agent/src/tools.rs:1298:9: replace <impl Tool for ReadTool<'_>>::execute -> Result<ToolOutput, LoopError> with Ok(Default::default())`: unviable, build failure.
- `crates/hatchling-agent/src/tools.rs:1394:5: replace room_output -> ToolOutput with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/run_trail.rs:558:5: replace room_ending -> Ending with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/room_ops.rs:170:9: replace RoomSession<'r>::handle -> RoomResult with Default::default()`: unviable, build failure.

`mut-agent-outcomes.json` sha256
`92a4749b09c4281c4169fa1b3a810e7685ffdbfe4c852ae7781c042da73fc56c`;
`mut-broker-outcomes.json` sha256
`c6e6c1c22d6c23a75369005207ff6bfcb6b40d30a92346de734f8a16fa13c0d5`.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3804 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3804 | 0 | 0 |

`gate.log` sha256
`f3e51e38b562ee1f1549dfd322b246c120eff8972d017552e53b34e85b7ed2de`.

The gate was run once, on the tested tree, after the breaks and the
mutants were clean.

## The app

`npm run check` in `frontend/`, on Windows, on the tested tree's app
files: exit 0. Its three test runs passed 490, 176 and 16 tests with
none failed, and the generated protocol types match the schema, which
the gate's own schema check found current.

`app/check.log` sha256
`4171956a0d3a59100bed578a0c15a4e3a37d7c8c2b7133a266907952dc8faa41`.

## With a real Room, and with a model

Not yet run. The tests above stand in for the spawner (in the broker)
and for the broker (in the agent), and the agent's loop test goes
through a real broker session with no Room behind it. The Room's own
part, a page read by the supervisor in a verified image through the
spawner, was run as root for the change before the last, and is
unchanged here.

What is still to be seen is the whole of it at once: a model, on a real
install, asked in a chat to read files. That run is to be made from
this change's own release build, as the run tool's was, and its result
added to this record before the change is merged.

## What this change does not do

- **Search files.** Finding where something is in files is `rg` or
  `grep`, through `run`. A search tool is a later change.
- **Change files.** Writing and editing are later changes.
- **Read past the start of a line longer than a page.** Such a line
  comes back as its first 8 KiB, said to be cut. The rest of it is read
  with a program.
- **Keep the path in the Activity.** See the decisions above.
- **Give the tool to a run without its person there.** A scheduled run,
  a hook's and a subagent are refused, and aren't offered it.
