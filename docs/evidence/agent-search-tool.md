# Evidence: the agent can search the files in its computer

What was tested, how, and what came back, for a change that gives the
agent a `search` tool: the lines that match a pattern, in the files of
its own computer. Every number here is read from a file in the results
bundle, which is attached to the release that carries this change,
unless it says otherwise.

| | |
|---|---|
| Base commit | `33e66efd` (`feat(room): search the Room's files from the spawner (#452)`) |
| Tested tree | `9622a220a1833bb98d1372727f18212f6e43cb25` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's check on Windows 11 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-agent-search-tool-results.tar.gz`, 862762 bytes, sha256 `34dc4e0b0333f92885f04c81c2bb37a1e693d7d9ff1b2232e4df34fed717769c` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What this is for

The Room's supervisor could be asked for the lines that match a
pattern, and nothing in a chat could ask. To find where something was,
the agent wrote a `grep` or `rg` command for `run`: a command line of
its own making, which a small model often got wrong, with a name, kept
output and a place among the Room's jobs for what is a question about
files.

## What changed

- **The session protocol is v25** (`hatchling-core`): `RoomOp::Search`
  carries a pattern, and perhaps a path, a glob and two switches
  (`ignore_case`, `literal`). `RoomResult::Found` carries where the Room
  looked, the matching lines and whether the search was stopped early;
  `RoomResult::NoSearch` says why nothing was searched.
- **The broker** (`room_ops.rs`): checks the pattern, the glob and the
  path for their shape before anything is asked; takes a path that isn't
  absolute as one under `/workspace`, and `/workspace` when none is
  named; asks the spawner; and carries what was found as it came, with
  bytes that aren't text shown as the replacement character. A search
  takes no job name and none of the session's count of commands, and is
  refused for a run with nobody there, as a command is.
- **The owner's Activity** (`run_trail.rs`): a search that was made is
  done, whatever it found; one that couldn't be made is a failure.
- **The agent** (`hatchling-agent`): a `search` tool beside `run` and
  `read`, given wherever those are. The model is told where the Room
  looked, the matching lines as `path:line:text`, how many there are,
  and, when the search was stopped, that there may be more and how to
  narrow it; or that nothing matched; or why nothing was searched. The
  `read` tool's description now points at `search` for finding where
  something is, where it pointed at `rg` or `grep`.
- The read arm of the broker's Room handler moved into a function of its
  own, unchanged, to keep the handler short.
- `docs/session-protocol.md`, `docs/arch/trust-boundary.md`, the
  changelog, and the app's generated protocol types.

## What a search can and can't be

Nothing here adds to what the agent could already do: a command it ran
could search the same files with the same program. What it takes away
is the command line.

- **The model's three words are checked for shape in the broker**, and
  passed on as they are. What makes them values and never options is in
  the Room's supervisor, with everything else the search program is
  told; that was the change before this one, and its record is
  `room-search-wire.md`.
- **The same sessions that may run a command may search**: a chat, with
  its person there. Any other run is refused before the Room is asked.
- **What was found is the Room's words**: data, never instructions. The
  tool's description says so, as `run` and `read` do.
- **The log says files were searched**, and not for what or where.

## Tests

Nine new, and three widened.

- **The wire** (`hatchling-core`, `messages::room_tests`, one): the
  search op and both results in their written form; what is left out is
  left out of what is written; a search with no pattern, a field that
  isn't named, or a switch that isn't true or false never decodes; and
  the protocol is 25.
- **The broker** (`room_ops::tests`, three): files are searched as the
  session names them, each switch on its own, with what was found told
  as it came and no job name taken; each reason a search can't be made
  is said, and what may not be asked for never is; the most a search
  can find, of the bytes longest written, fits one frame.
- **The agent's tool** (`tools::tests`, four): its name, description
  and schema; what it asks the broker for each thing the model can
  give, and what it tells the model for lines found, a search stopped
  early, nothing matched and nothing searched; that it asks nothing for
  what isn't a pattern, a path, a glob or a switch; and how what was
  found is counted and noted.
- **The agent's loop** (`tests/loop.rs`, one): the model is told it has
  the tool, its `search` reaches the broker, and the broker's answer
  comes back to it.
- **Widened**: the Activity's test of how each answer about the Room
  ends has a search made and one that couldn't be; the spawner's search
  test asks through the trait a session's Room asks through; the `read`
  tool's description test looks for the pointer to `search`.

## Found on the way

- **A test that could wait for ever.** The test that the tool asks
  nothing for a bad argument held the broker's end of its socket open.
  With the tool broken so that it asked anyway, the test waited for an
  answer nobody would send: four breaks by hand ran into their
  fifteen-minute limit in the first run, on tree `1aa59b4c…`. The test
  now closes the broker's end first, so a search asked for by mistake
  fails at once. The older test of the `read` tool is written the same
  way and wasn't changed here.
- **A break that looked half caught.** The runner stopped at the first
  test binary that failed, so the loop's test never ran for a break the
  tool's own tests caught first. The runner now runs every binary.
- **One mutant missed, then caught.** The first mutation run, on tree
  `37896969…`, missed the one line that hands a session's search to the
  spawner (`<impl Rooms for Spawner>::search`, replaced by an empty
  answer). The spawner's test now asks through it. Only tests changed
  between that tree and the tested one, and every run in the bundle is
  of the tested tree.

## Breaks by hand

Forty, each one part of the change taken out or bent, the tests of the
three crates run on each, and each failing test named
(`unit-breaks.log`, `unit-*.log`, `st_break.py`). All forty were caught,
and the tests passed before and after.

| Break | Caught by |
|---|---|
| The search op has another name on the wire | the wire's test |
| No path is written as `null` | the wire's test |
| A switch left off is written | the wire's test |
| A search must say whether it is literal | the wire's test; the tool's asking test; the loop's test |
| What was found has another name | the wire's test |
| Nothing searched has another name | the wire's test |
| The protocol is still 24 | the wire's test |
| A search isn't checked in the broker | the broker's reasons test |
| A path isn't taken under the workspace | the broker's naming test; its reasons test |
| No path is the whole machine | the broker's naming test |
| The broker drops the glob | the broker's naming test; its reasons test |
| The broker drops the case switch | the broker's naming test |
| The broker drops the literal switch | the broker's naming test |
| The broker swaps the switches | the broker's naming test |
| What was found is said of the workspace, wherever it looked | the broker's naming test |
| A search stopped early isn't said | the broker's naming test |
| What was found must be text, or is nothing | the broker's naming test |
| A failed search is said as a failed command | the broker's reasons test |
| No Room to search is said as a failed command | the broker's reasons test |
| A search that was made is a failure in the Activity | the Activity's test |
| Nothing searched is done in the Activity | the Activity's test |
| The tool has another name | the tool's description test; its asks-nothing test; the loop's test |
| The pattern need not be given | the tool's description test |
| The count is of bytes | the tool's asking test; the counting test |
| Nothing matched isn't said | the same two |
| A search stopped early with nothing says nothing matched | the counting test |
| Stopped early isn't told | the tool's asking test; the counting test |
| Where it looked isn't told | the same two |
| The tool drops the path | the tool's asking test; the asks-nothing test |
| The tool drops the glob | the same two |
| The tool drops the case switch | the same two |
| The tool's literal switch is read from the case switch | the same two |
| A switch may be anything | the asks-nothing test |
| A switch given as `null` is an error | the tool's asking test |
| A switch left out is on | the tool's asking test |
| What was found is told as an error | the tool's asking test |
| Why nothing was searched is told bare | the tool's asking test |
| The `read` tool doesn't point at `search` | the `read` tool's description test |
| The tool isn't given to the agent | the loop's test |
| The model isn't told it can search | the loop's test |

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`, `mut-*-outcomes.json`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 0 | 0 | 0 | 0 | 0 |
| `hatchling-agent` | 17 | 15 | 0 | 0 | 2 |
| `hatchling-broker` | 5 | 1 | 0 | 0 | 4 |

Core's change is two types and a constant, with nothing in it to
mutate; `mut-core.log` says so. The broker's change is mostly values
put into a structure and arms of a match, which mutation makes little
of: the fourteen breaks by hand in the broker and the Activity are what
test it.

Each crate's `outcomes.json`, by sha256:

- agent: `297530ea23c5512666f9c0e9cf3a7d8452ced03c6ceaca18366c2994cad34883`
- broker: `a6fe16246a08a1bc23eac3d30ff42f2e8d3a4ef7c1a273402ef25487d217ae72`

The unviable ones:

- `tools.rs:1485:9: replace <impl Tool for SearchTool<'_>>::execute -> Result<ToolOutput, LoopError> with Ok(Default::default())`: unviable, build failure
- `tools.rs:1513:5: replace room_output -> ToolOutput with Default::default()`: unviable, build failure
- `run_trail.rs:619:5: replace room_ending -> Ending with Default::default()`: unviable, build failure
- `room_ops.rs:178:9: replace RoomSession<'r>::handle -> RoomResult with Default::default()`: unviable, build failure
- `room_ops.rs:342:5: replace paged -> RoomResult with Default::default()`: unviable, build failure
- `room_ops.rs:367:5: replace searched -> RoomResult with Default::default()`: unviable, build failure

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3873 passed, 0 failed | exit 0: 135 binaries, 3873 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The tests of what the change touches were run as root too, as CI runs
them: 192 in core, 175 in the agent and 8 in its loop, 32 in the broker
(`root/as-root.log`).

The app's own check (`npm run check`: types, lint, 493 unit tests, and
coverage of the live adapter at 100%) passed on the same tree, on
Windows (`frontend-check.log`). The app's code didn't change: only its
generated protocol types and the version its test expects.

## What wasn't tried

- **A real model asking for a real search of an installed Room.** Every
  test here is against a stand-in: for the model, for the broker, or for
  the spawner. With the change before this one, no run yet has a search
  asked in a chat, carried by the broker and the spawner, and answered
  by ripgrep in a real Room. The first is the first chat that searches
  on an install.
- **Whether a small model reaches for `search`** where it used to write
  a `grep` command, and whether it narrows a search that was stopped
  early as the note tells it to.
- **The app showing a search.** A search is counted in the Activity as
  a use of the Room, as a command is; no page was opened to look at
  one.
- **What a search costs a long chat.** Each can put 8 KiB of matching
  lines into the model's history; nothing here measures a chat with
  many.
