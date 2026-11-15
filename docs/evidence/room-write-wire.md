# Evidence: the Room's files can be written and edited from the spawner

What was tested, how, and what came back, for a change that lets the
spawner ask the Room's supervisor to write a file or to replace one
exact piece of a file's text. Every number here is read from a file in
the results bundle, which is attached to the release that carries this
change, unless it says otherwise.

| | |
|---|---|
| Base commit | `0e8a8b6b` (`feat(loop): a run's last step is an answer, and a chat has 25 steps (#456)`) |
| Tested tree | `2dd18e7b047766b21c7a13da1d078fb88a42779b` |
| Date | 2026-11-14 and 15 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| The supervisor, built static | sha256 `a51a724dbc26629dc3c61378f91174706cc2d67899705ad501e28902a5840cdd` |
| Results bundle | `hatchling-room-write-wire-results.tar.gz`, 2865934 bytes, sha256 `bf9d0a58809ca257af8e2ef7818a201dcc993d4af5b166ba33fdb364dbf17c4c` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What this is for

The Room could run a command, hand back a page of a file, and search.
To put text in a file, a caller ran a program. On a real install a
model ran `echo` with a `>` among its arguments, which prints the words
and writes nothing with no shell in between, and spent its steps
finding out ([the record of the search tool](agent-search-tool.md)).

This change is the wire and the Room's side of it. Nothing in a chat
can ask for a write or an edit yet: the agent's tools are the next
change.

## What changed

- **The Room's wire** (`hatchling-core`): `Ask::Write` (a path, the
  bytes, and whether a file there is to be replaced) and `Ask::Edit`
  (a path, the text to find, what to put in its place, and whether
  every place is meant). `Say::Wrote`, `Say::NoWrite`, `Say::Edited`,
  `Say::NoEdit`. `check_write` and `check_edit` say what may be asked
  for; `Wrote::check`, `Edited::check` and `NoEdit::check` what an
  answer may be.
- **The supervisor** (`hatchling-room`, `write.rs`): writes the file
  itself, as the Room's user. No job is started and no program is run.
- **The spawner's session** (`room_session.rs`): a change is asked in
  its turn, as a page is; its answer is held to what was asked; a
  change for a caller that has gone is never made.
- **The spawner's own protocol** (`spawner.rs`): `Request::RoomChange`,
  `Reply::RoomChanged`, and `Spawner::room_change`. The spawner's
  answers about what is in the Room now go through one helper, to keep
  the function that serves a connection within its length.
- `docs/arch/trust-boundary.md` and the changelog.

## What a change is, and isn't

It adds nothing to what a job could already do to the same files. What
it takes away is the program in between.

- **A file is never changed where it lies.** What it is to hold is
  written beside it, under a name of the supervisor's own, and given
  the file's name in one step. Nothing that reads the file sees half of
  it, and a write that fails leaves the file as it was.
- **A new file goes only where nothing is**, unless the caller says to
  replace what is there. It is given its name by a rename that
  replaces nothing, so a file that appears meanwhile stays.
- **Only a file is written over or edited.** A directory or a link at
  the path is left alone and said. A link is never followed to what it
  names, and the file an edit reads is opened without following one.
- **A replaced or edited file keeps its mode.** A new one can be read
  by anyone and run by nobody.
- **An edit is exact.** The text to find must be in the file byte for
  byte, at one place unless every place is asked for. Text that isn't
  there changes nothing. Text that is there more than once changes
  nothing either, and the places are named: how many, and the lines
  the first eight start on. The file must be UTF-8 text of at most
  4 MiB, before and after.
- **It is bounded.** 32 KiB to a write; 8 KiB to each of an edit's two
  texts. The most of each fits one frame.
- **A change that got no answer is never asked again.** When a session
  ends with a change the supervisor was given and hadn't answered, its
  caller is told the file may or may not have been changed. One the
  supervisor was never given wasn't made, and is said as that.

## What the Room's filter said first

The first version gave a new file its name with a hard link, the
ordinary way to place a file only where nothing is. Under the Room's
filter the supervisor answered every new file with "may not write
there": the filter refuses hard links, by either call, on purpose.
Every unit test had passed, on a machine with no filter. The proof
under the filter is what said so, on tree `4d0acacd…` (not in the
bundle: its log was written over by the runs that followed).

A new file is now placed by `renameat2` with `RENAME_NOREPLACE`, which
the filter allows, and the proof passes.

## Tests

Twenty-four, and three proofs.

- **The wire** (`hatchling-core`, `room::tests`, four): what may be
  asked for, each bound at its edge, and that the most that may be
  asked fits one frame; the wire forms of the two asks, and of what is
  said of them, with a field or a reason that isn't named never
  decoding; and what an answer may be.
- **Writing and editing** (`hatchling-room`, `write::tests`, eight),
  against real files: a new file with its directories; a file there
  that stays unless it is to be replaced, and keeps its mode when it
  is; only a file is written over; each error's word, and that nothing
  is left where the Room's user may not write; an edit at the one
  place; text at many places named and left; what can't be edited left
  as it was; how a line is counted.
- **The supervisor** (`hatchling-room`, `tests`, one), over its
  socket: a file written, edited and read back as a page.
- **The session** (`room_session::tests`, nine): asked in its turn,
  one thing at a time, each change compared with what was asked for;
  an answer that can't be one, or is to the other kind of change, ends
  the session; a change not answered isn't known to have been made;
  nothing is launched for, and nothing changed for, a caller that has
  gone; an answer is held to what was asked; a change may be asked for
  only as a write or an edit may; each reason has its words; a change
  starts a session and the next uses it.
- **The spawner** (`spawner::tests`, two): a change reaches the
  session and starts it; one that may not be asked for never reaches
  the spawner; the request's and the reply's wire forms; the most a
  change may carry fits one frame.

**The proofs**:

- **The supervisor under the Room's filter**
  (`crates/hatchling-browser/tests/room_supervisor.rs`, static,
  release, `super.log`, `super-proof.log`): it makes the directories,
  writes a file, refuses to write over it, names the places of text
  that is there twice, edits one place and then every place, replaces
  the file keeping its mode, leaves nothing beside it, reads it back
  as a page, and goes on to end with exit 0.
- **A real Room** (`crates/hatchling-broker/tests/room_disk.rs`, as
  root, `root/installed.log`): through the spawner's own protocol,
  into a Room on a verified image with this tree's supervisor. A file
  is written on the room disk, edited, read back as a page, and
  replaced. A file in the read-only image isn't written, and that is
  said. A path that isn't absolute never reaches the Room. The Room is
  the same one afterwards.
- **The supervisor's own tests, static and release**, as CI builds
  them: 46 passed (`super-proof.log`).

## Breaks by hand

Fifty-six, each one part of the change taken out or bent, the tests of
the three crates run on each, and each failing test named
(`unit-breaks.log`, `unit-*.log`, `fw_break.py`). All fifty-six were
caught, and the tests passed before and after.

| Break | Caught by |
|---|---|
| A file need not be named by its whole path | the wire's bounds test; the session's and the spawner's tests |
| A file's path may name a directory | the wire's bounds test; the session's |
| A write may carry any length | the wire's, the session's and the spawner's tests |
| An edit may look for nothing | the same three |
| An edit may look for any length | the wire's bounds test |
| An edit may put in any length | the wire's bounds test |
| An edit may change nothing | the wire's, the session's and the spawner's tests |
| A write has another name on the wire | the wire-form test |
| A written file may be any length | the wire's answers test; the session's held-answer test |
| An edit may be at no place | the same two |
| An edit may be on no line | the same two; the session's bad-answer test |
| An edited file may be any length | the wire's answers test; the session's bad-answer test |
| Text at many places may be at one | the same two |
| Any number of places may be named | the wire's answers test |
| More places may be named than there are | the wire's answers test |
| Places may be on lines that go back | the wire's answers test |
| A file that is there is written over | the stays test; the supervisor's socket test |
| A link is followed to what it names | the only-a-file test; the can't-be-edited test |
| A link is written over | the only-a-file test |
| A new file replaces one that appeared | the stays test |
| What was written beside is left there | the stays test |
| A replaced file loses its mode | the stays test; the edit test |
| A new file can be run | the new-file test |
| The directories above a new file aren't made | the new-file test; the supervisor's socket test |
| A write says nothing was replaced | the stays test; the supervisor's socket test |
| A write says another length | the new-file and stays tests; the supervisor's socket test |
| An edit's file is opened through a link | the can't-be-edited test |
| A file with a byte no text has is edited | the can't-be-edited test |
| Text at many places is replaced at each | the many-places test; the supervisor's socket test |
| Every place is named, however many | the many-places test |
| Only the first place is replaced | the many-places test |
| An edit says it was on the first line | the edit, many-places and can't-be-edited tests |
| Lines are counted from nought | five of the supervisor's tests |
| An edit may leave a file of any length | the can't-be-edited test |
| Bytes that aren't text are put in a file | the can't-be-edited test |
| Text of no length is everywhere | the can't-be-edited test |
| A file is changed for a caller that has gone | the session's gone-caller test |
| A write is asked as one that replaces nothing | the session's in-turn test |
| An edit is asked of the one place | the session's gone-caller and in-turn tests |
| A file of another length is taken as written | the session's held-answer and bad-answer tests |
| A file replaced that wasn't to be is taken | the same two |
| An edit at many places is taken for the one | the same two |
| What was written isn't held to what a write may be | the session's held-answer test |
| What was edited isn't held to what an edit may be | the session's held-answer and bad-answer tests |
| What was written isn't checked by the session | the session's bad-answer test |
| What was edited isn't checked by the session | the session's bad-answer test |
| The places text is at aren't checked | the session's bad-answer test |
| An unanswered change is said not to have been made | the session's unanswered and bad-answer tests |
| An unanswered change is told nothing | the same two |
| A waiting change is told nothing when the session ends | the session's unanswered test |
| A change to ask for is nothing in hand | the session's launch test |
| A change is no reason to launch | the session's launch test |
| A failed launch tells no changer | the session's launch test |
| A Room is launched for changers that have all gone | the session's launch test |
| The session takes any change | the session's start test; the spawner's reach test |
| The spawner's client asks for any change | the spawner's refusal test |

A first run of these, on tree `7a731d7c…`, let one through: with every
write asked of the supervisor as one that replaces nothing, the tests
passed. The session's in-turn test asked for a write that replaces,
and never looked at what the supervisor was asked. It now compares
every change it asks with what the supervisor hears. Only tests
changed between that tree and the tested one.

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`, `mut-*-outcomes.json`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 60 | 60 | 0 | 0 | 0 |
| `hatchling-browser` (the supervisor) | 41 | 35 | 0 | 0 | 6 |
| `hatchling-broker` | 66 | 55 | 0 | 0 | 11 |

Each crate's `outcomes.json`, by sha256:

- core: `bfb76bcbef437b1742a4ee77cadaf1f43c3febfded8892be380d087e70d8705f`
- browser: `baab6d6a9e970752bc35f2c53159b614926f29c82491ef18aa01a3f58145adf9`
- broker: `06d9b03cf8e4d0adb174c93ff3eb0b564dd70ac85e8ee1b4be60e3add1cd8f04`

The unviable ones:

- `bin/hatchling-room.rs:579:9: replace Room<'_>::answer -> Say with Default::default()`: unviable, build failure
- `bin/hatchling-room/write.rs:48:5: replace there -> io::Result<There> with Ok(Default::default())`: unviable, build failure
- `bin/hatchling-room/write.rs:103:5: replace no_write -> NoWrite with Default::default()`: unviable, build failure
- `bin/hatchling-room/write.rs:140:5: replace no_edit -> NoEdit with Default::default()`: unviable, build failure
- `bin/hatchling-room/write.rs:203:5: replace edited_bytes -> Result<Edited, NoEdit> with Ok(Default::default())`: unviable, build failure
- `bin/hatchling-room/write.rs:216:5: replace edited -> Result<Edited, NoEdit> with Ok(Default::default())`: unviable, build failure
- `room_session.rs:175:9: replace Change::expect -> Expect with Default::default()`: unviable, build failure
- `room_session.rs:1973:9: replace Sessions::change -> ChangeReply with Default::default()`: unviable, build failure
- `spawner.rs:799:9: replace Spawner::helper -> Result<SpawnedProcess, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:914:9: replace Spawner::room_disk -> Result<Option<crate::room_disk::RoomDiskStatus>, String> with Ok(Some(Default::default()))`: unviable, build failure
- `spawner.rs:1107:9: replace Spawner::room_change -> Result<RoomChanged, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1173:9: replace Spawner::room_job_asked -> Result<RoomJobAnswer, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1252:9: replace SpawnedProcess::watching -> Result<Self, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1727:9: replace Reply::output -> Self with Default::default()`: unviable, build failure
- `spawner.rs:1732:9: replace Reply::page -> Self with Default::default()`: unviable, build failure
- `spawner.rs:1737:9: replace Reply::found -> Self with Default::default()`: unviable, build failure
- `spawner.rs:1742:9: replace Reply::changed -> Self with Default::default()`: unviable, build failure

Mutation makes nothing of a statement taken out: the line that removes
what was written beside a file, the one that makes a new file's
directories, the one that keeps a replaced file's mode. The breaks by
hand are what test those.

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3904 passed, 0 failed | exit 0: 135 binaries, 3904 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The supervisor's own tests aren't among those: it is built with a
feature of its own. They were run in the quick look, static and release
as CI builds it, and as root: 46 passed each time.

The tests of what the change touches were run as root too, as CI runs
them: 21 in core, 46 in the supervisor, 118 in the broker
(`root/as-root.log`).

## What wasn't tried

- **A reader in a change's way.** That nothing reading a file sees
  half of it rests on how the file is placed, a rename, and no test
  reads a file while it is written.
- **Two writers.** A job that changes a file between an edit's reading
  it and its being placed loses its change to the edit. The window is
  the supervisor's own, and short; an edit carries nothing of the file
  as its caller last read it, so a file changed since a caller read it
  is edited all the same, wherever the text to find is still there.
- **A file system that can't rename without replacing.** The room disk
  and the session directory can. Where one can't, a new file isn't
  written, and that is said as a failure.
- **A full disk, and a file larger than the room disk has room for.**
  A write that fails removes what it wrote beside the file; that the
  removal itself can fail on a disk that is full wasn't tried.
- **A file with very long lines, or of many megabytes edited often.**
  An edit reads the whole file and writes the whole file, on the
  supervisor's one thread: a job's output waits meanwhile.
- **Text that isn't UTF-8.** Such a file can be written whole, and
  can't be edited: it is said not to be text.
