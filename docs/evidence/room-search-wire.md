# Evidence: the Room's files can be searched from the spawner

What was tested, how, and what came back, for a change that lets the
spawner ask the Room's supervisor for the lines that match a pattern.
Every number here is read from a file in the results bundle, which is
attached to the release that carries this change, unless it says
otherwise.

| | |
|---|---|
| Base commit | `1c303ec2` (`docs(evidence): record unlock and a follow-up message on a real install (#451)`) |
| Tested tree | `346a6e09ec6c5d688d760343a4268bc4613cc537` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Room image | the one built for main `2993ce29`, squashfs sha256 `db6eaf0294fdf5167c7334da4bb740fb4f09eed1b19138a66c6ed8081cd81b7c`, root hash `82e1e2919c155f1c92bf184a4753a1efdb4ee3fe981838b3db60da192dc7bbe8`, with ripgrep 14.1.1 |
| Results bundle | `hatchling-room-search-wire-results.tar.gz`, 2202063 bytes, sha256 `f17ee1e0679faf9741a0c961ae61daed128e1c45994dba196c6d38b0b06b6360` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What this is for

The Room could run a command and hand back a page of a file. Finding
which file holds something meant running a search program as a job: a
name, kept output, a place among the jobs the Room remembers, and a
command line the caller writes. A search is a question about files, and
is now asked as one.

This change is the wire and the Room's side of it. Nothing in a chat
can ask for a search yet: the agent's tool is the next change.

## What changed

- **The Room's wire** (`hatchling-core`): `Ask::Search` carries a
  pattern, a path, perhaps a glob, and two switches (ignore case, the
  very words). `Say::Found` carries what was found and whether there
  was more; `Say::NoSearch` says why nothing was handed back.
  `check_search` says what may be asked for, and `Found::check` what
  an answer may be.
- **The supervisor** (`hatchling-room`, `search.rs`): starts the Room's
  own search program, `/usr/bin/rg` from the read-only image, with
  arguments it fixes itself; reads what it prints for five seconds and
  8 KiB at most; kills its process group and waits for it; and hands
  back whole lines.
- **The spawner's session** (`room_session.rs`): a search is asked in
  its turn, as a page is. Its caller is answered once the Room is down
  if the session ends first, and nothing is searched for a caller that
  has gone.
- **The spawner's own protocol** (`spawner.rs`): `Request::RoomSearch`,
  `Reply::RoomFound`, and `Spawner::room_search`.
- `docs/arch/trust-boundary.md` and the changelog.

## What keeps a search from being anything else

- **The asker's three words can't be options.** The pattern goes as
  `--regexp=<pattern>`, the glob as `--glob=<glob>`, and the path after
  `--`. A pattern of `--pre=/bin/sh` is searched for; it starts nothing.
- **Everything else is fixed** in the supervisor: no settings file is
  read, no link is followed, a repository's own records, files over
  8 MiB and files that aren't text are passed over. No shell is in
  between.
- **It is short.** Five seconds and 8 KiB, whichever comes first, and
  then the program's whole process group is killed.
- **It is confined as everything in the Room is.** The program runs as
  the Room's user, under the Room's filter, with a job's `PATH` and
  `LANG` and nothing else of the supervisor's environment.
- **What comes back is held to its bounds** by the spawner: no longer
  than 8 KiB, and ending at the end of a line. Anything else ends the
  session, as a page that can't be one does.

## Tests

Twenty-six, and three proofs.

- **The wire** (`hatchling-core`, `room::tests`, three): what may be
  asked for, each bound at its edge, and that the most that may be
  asked fits one frame; the wire forms, each echoing its nonce, with a
  field that isn't named never decoding; and what an answer may be.
- **The search** (`hatchling-room`, `search::tests`, ten), against
  stand-in programs: the arguments, word for word, and that words
  beginning with `--` stay values; what was printed is handed back;
  the program gets its arguments and a bare environment; a program
  that ends as no search does, and one that isn't there; more than
  fits is cut at a line and the program stopped; a search out of time
  is stopped with what it had; whatever the program started is ended
  with it; exactly what fits is whole; a read that is interrupted is
  made again and one that fails ends the reading; only whole lines
  that fit are kept.
- **The supervisor** (`hatchling-room`, `tests`, three), over its
  socket: a search is answered with what the program found for the
  arguments made of the ask; one that fails is said and the supervisor
  goes on; a job runs on through a search.
- **The session** (`room_session::tests`, eight): asked in its turn,
  one thing at a time; an answer that can't be one, or is to something
  else, ends the session; a search not answered is answered once the
  Room is down; nothing is launched, and nothing searched, for a caller
  that has gone; each reason has its words; a search starts a session
  and the next uses it; a started session finds its first search
  waiting.
- **The spawner** (`spawner::tests`, two): a search reaches the session
  and starts it; one that may not be asked for never reaches the
  spawner; and the request's wire form refuses a field it doesn't name.

**The proofs**, each as root or under the Room's filter:

- **The supervisor under the Room's filter**
  (`crates/hatchling-browser/tests/room_supervisor.rs`, static, release,
  `super.log`): it answers a search, having started a program, read it,
  killed it and waited for it under the filter, and goes on. This
  machine has no `/usr/bin/rg`, so what it proved is the path where the
  program can't be started, and nothing at a path.
- **A real Room** (`crates/hatchling-broker/tests/room_disk.rs`, as
  root, `root/installed.log`): through the spawner's own protocol, into
  a Room on a verified image with the candidate's supervisor. Nothing
  at a path is said; a search of what is there can't be run, because
  the fixture's image has no search program, and that is said; a path
  that isn't absolute never reaches the Room; and the Room is the same
  one afterwards.
- **The search program itself, in the real image**
  (`crates/hatchling-sys/tests/room_image_tools.rs`, as root,
  `root/image-tools.log`): ripgrep, from the image named above, told
  word for word what a search tells it, under the Room's profile. One
  command finds lines and one finds none and ends with 1. All 138
  commands ended as they must, and the kernel recorded nothing killed
  by the filter.

No one run has all three together: a real search program, started by
the supervisor, in a real Room. The first two prove the supervisor and
the wire without the program; the third proves the program with the
supervisor's words and without the supervisor.

## A flaky test, found and fixed here

The first full run stopped at a search test that failed once, having
passed three times. The tests wrote a shell script and ran it. Another
test, on another thread, starting a program at that moment carries the
script's open file into its child for an instant, and the kernel won't
run a file that is open for writing.

Run 80 times, the supervisor's tests failed 7 times before, in four
different tests, and 0 times after (`flaky-before.log`, of tree
`0e37a302…`; `flaky-after.log`). The stand-in is now written by a
child, so the file is never open for writing in the test's process.
Nothing but tests changed for it.

## Breaks by hand

Twenty-seven, each one part of the change taken out or bent, the tests
of the three crates run on each, and each failing test named
(`unit-breaks.log`, `unit-*.log`, `sw_break.py`). All twenty-seven were
caught, and the tests passed before and after.

| Break | Caught by |
|---|---|
| An empty pattern may be asked for | the wire's test of what may be asked; the session's start test |
| A pattern may be many lines | the wire's test |
| A path need not be absolute | the wire's, the session's and the spawner's tests |
| A glob isn't looked at | the wire's, the session's and the spawner's tests |
| What was found may be any length | the wire's test of an answer; the session's bad-answer test |
| What was found may end in the middle of a line | the same two |
| The pattern is a bare argument | the arguments' test; the supervisor's answer test |
| The glob is a bare argument | the same two |
| Nothing ends the options before the path | the same two |
| A settings file is read | the same two |
| The program keeps the supervisor's environment | the environment's test |
| Time running out isn't said | the out-of-time test |
| A flood is read until its time | the flood's test: it wasn't stopped |
| Only the first process is ended | the test of what is left running |
| A refusal is taken for nothing found | the not-a-search test; the supervisor's failure test |
| A cut line is kept | the whole-lines test; the flood's |
| What was left out isn't said | the whole-lines test |
| A path with nothing at it is searched | the supervisor's answer test |
| What was found isn't checked by the spawner | the session's bad-answer test |
| A search is made for a caller that has gone | the session's gone-caller test |
| A search to ask for is nothing in hand | the session's launch test |
| A Room is launched for seekers that have all gone | the session's launch test |
| A search in hand is dropped when the session ends | the session's owed test |
| A failed launch tells no seeker | the session's launch test |
| The session takes any search | the session's start test |
| Nothing at a path is said as a failure | the words' test; the in-turn test |
| The spawner's client asks for any search | the spawner's test |

One line of one of these logs was changed for the bundle. The break
that leaves the program the supervisor's environment makes the stand-in
print that environment, and it held the machine's name: the name is
replaced in `unit-the-program-keeps-the-supervisors-environment.log`.

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`, `mut-*-outcomes.json`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 31 | 30 | 0 | 0 | 1 |
| `hatchling-browser` (the supervisor) | 38 | 35 | 0 | 0 | 3 |
| `hatchling-broker` | 46 | 41 | 0 | 0 | 5 |

Each crate's `outcomes.json`, by sha256:

- core: `732ac849768621edf10fddc399f3e5c5b8d01dcf916750ea08a1ed3749981e75`
- browser: `f9bbfc15c313c480140f33cab7aaafe4822557c284296af084616a68b0b3cd1e`
- broker: `724de1d0ed2de6bfea261698b4a9eff93fa25e7112be3d664aa8c778f74ebe82`

A run before this one, on a tree without three of the search's tests,
missed four in the supervisor's reading loop: `>` for `>=` where the
reading stops at what fits, and three on the arm that makes an
interrupted read again. The tests of exactly what fits and of a read
that fails were added for them, and every run in the bundle is of the
tree with them.

In that earlier run the broker's mutants weren't run at all: its
unmutated tests failed once, at
`harness_relay::tests::a_handoff_that_never_comes_or_is_no_listener_is_the_runs_problem`,
a test of code this change doesn't touch, which passed in the full gate
on both toolchains before and after. It passed here. It is an older
test that fails now and then, and is not this change's to fix.

The unviable ones:

- `room.rs:81:34: replace * with /`: unviable, build failure
- `bin/hatchling-room.rs:107:9: replace Place::under -> Self with Default::default()`: unviable, build failure
- `bin/hatchling-room.rs:573:9: replace Room<'_>::answer -> Say with Default::default()`: unviable, build failure
- `bin/hatchling-room/search.rs:136:32: replace + with * in read_until`: unviable, build failure
- `room_session.rs:1661:9: replace Sessions::search -> SearchReply with Default::default()`: unviable, build failure
- `spawner.rs:794:9: replace Spawner::helper -> Result<SpawnedProcess, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:907:9: replace Spawner::room_disk -> Result<Option<crate::room_disk::RoomDiskStatus>, String> with Ok(Some(Default::default()))`: unviable, build failure
- `spawner.rs:1124:9: replace Spawner::room_job_asked -> Result<RoomJobAnswer, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1201:9: replace SpawnedProcess::watching -> Result<Self, String> with Ok(Default::default())`: unviable, build failure

Mutation makes nothing of the line that kills the program's process
group: it is a statement, not an operator. The break made by hand there
kills only the first process, the narrower fault, and the test of what
is left running catches it. No break takes the kill out altogether.

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3864 passed, 0 failed | exit 0: 135 binaries, 3864 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The supervisor's own tests aren't among those: it is built with a
feature of its own. They were run in the quick look, static and release
as CI builds it (37 passed, `super.log`), and as root.

The tests of what the change touches were run as root too, as CI runs
them: 17 in core, 37 in the supervisor, 107 in the broker
(`root/as-root.log`).

## What wasn't tried

- **A real search program started by the supervisor in a real Room**,
  all three at once, as said above. The first time that happens is the
  first search asked of an installed Room.
- **A search of a large tree.** What five seconds of ripgrep on one
  thread covers of a big workspace wasn't measured; a search that runs
  out of time says so, and can be asked again of a narrower path.
- **A pattern ripgrep takes long to compile**, and files with very long
  lines beyond what the image proof's own files have.
- **A job's output while a search runs.** The supervisor's one thread
  waits out a search: a job that fills its pipe in those seconds waits
  to be read. That a job runs on through a search and its output is
  whole afterwards is tested; how long a busy job is held up isn't.
