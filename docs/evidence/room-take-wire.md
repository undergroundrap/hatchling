# Evidence: a file read out of the Room whole (WO-041, slice 3, the Room's side)

What was tested, how, and what came back, for the change that lets the
spawner read a file in the Room's workspace out whole, a piece at a
time, for it to be handed to the owner. The broker's store for it, its
checks and the agent's tool are the next change: nothing asks for a
file yet.

| | |
|---|---|
| Base commit | `589aae2c` (`feat(frontend): give Hatchling a file from the chat (WO-041) (#492)`) |
| Tested tree | `1db3bbd558a5818d6626e306a1df90ced4dc8750` |
| Date | 2026-11-17 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the Room's tests as root outside it |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| The supervisor, built static | sha256 `ae6deb64c75bb90fa882700eaf4d893c40e02494edea957c53b244445832b1e6` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **The Room's wire** (`hatchling-core`): `Ask::Take` (a path under
  `/workspace` and the byte to start from), `Say::Took` (at most
  32 KiB, the file's length, inode and change time) and `Say::NoTake`
  (no such file, denied, not a file, a link, outside the workspace,
  changed, failed). `check_take` says what may be asked for, and
  `Took::check` what a piece may be.
- **The supervisor** (`hatchling-room`, `take.rs`): opens the workspace,
  then the file beneath it with `openat2` (`RESOLVE_BENEATH`,
  `NO_SYMLINKS`, `NO_MAGICLINKS`, `NO_XDEV`), without waiting, and reads
  only a file. It looks at the file before and after it reads a piece;
  a file that changed between is said to have. Its read sits behind a
  small trait, so that is tested without a race.
- **The spawner's session** (`room_session.rs`): a piece is asked in its
  turn, as a page is, and held to where it was asked from. A piece that
  can't be the one asked for ends the session; whoever waits is told
  why once the Room is down. Nothing is read for a caller that has gone.
- **The spawner** (`spawner.rs`): `Request::RoomTake` and
  `Reply::RoomTook`. It puts the pieces together as root: at most
  8 MiB, known from the first piece before another is asked; the same
  inode, length and change time, as the supervisor says them, from the
  first piece to the last; all of it within two minutes. Then the file
  goes to the broker in frames, and `Spawner::room_take` reads it at
  the length said, never past the bound.
- `docs/arch/trust-boundary.md`, the changelog and WO-041.

## Before the code, and after it

The design is WO-041's, from the threat model written before slice 1.
An independent review of this diff, before these runs, found:

- **A take had no deadline of its own.** Each piece had its own wait,
  ten seconds, and an 8 MiB file is 256 of them: a supervisor answering
  each just in time could hold a caller for about 25 minutes. A take
  now has two minutes, the Room's start among them, looked at before
  every piece after the first, and the broker waits three.
- **A real file's change time was never tested.** Only a stand-in's
  was; a change time that was always 0 passed every test. A test now
  writes a real file where it lies, at the same length, and sees its
  change time move, then puts another file in its place and sees its
  inode change.
- **The docs said more than the code does.** What is compared is what
  the supervisor says, and a file can change in ways its change time
  doesn't show (a write through a shared mapping is seen only when it
  is written back). The docs now say so, and say what matters after:
  the broker checks and keeps the one copy it was handed.
- Smaller: the broker wasn't tested with a stream short of its length;
  `check_take`'s doc now says it is no boundary; the work order said the
  broker puts the pieces together, where it is the spawner.

Each was changed before the runs below.

## Tests

- The wire (`hatchling-core`, `room::tests`): what may be asked for (a
  path under `/workspace`, never `/workspace` itself, `/workspaces/…`,
  a directory, a relative path, NUL, past 1024 bytes; a start short of
  8 MiB), what a piece may be from where it was asked (too long, past
  the end, short where the file goes on), and the wire form of each,
  with unknown fields refused.
- The supervisor, served over a socket pair (`hatchling-room`): a file
  of three pieces handed back whole; an empty file; `out/../empty`
  beneath the workspace; a start past the end; a link at the last step,
  at a step before it, to a file in the workspace and to itself; `..`
  past the workspace, `/session`, `/workspaces`; a directory and a pipe
  nobody writes to, not waited for; a file its user can't read; a
  workspace that isn't there; every kernel answer it turns into words.
  With a stand-in file: reads that come seven bytes at a time and one
  interrupted, from the very end and past it; not a file (and nothing
  read); another inode, length or change time after the read; a file
  shorter than it said; a read that fails, and a look that does. With a
  real file: its change time and its inode, as above.
- Under the Room's filter, built static as the Room's image builds it:
  the supervisor hands back a file of two pieces whole, and refuses a
  link, a step out of the workspace, a pipe, a directory and a file that
  isn't there. Whatever it asks of the kernel for that, the filter lets
  it ask.
- The session (`room_session::tests`): a piece asked in its turn and
  handed back, before the supervisor is ready and around a job; a piece
  that can't be the one asked for, a wrong nonce, another answer, and a
  closed supervisor, each ending the session with why; pieces owed once
  the Room is down; callers that have gone launch nothing and are read
  for by no one; a failed launch, and a held session, answer with why;
  each refusal's words; a take starting a session that the next uses.
- The spawner (`spawner::tests`): a file of many pieces through the
  stand-in sessions, and one that changes as it is read; a path that
  may not be asked for refused before the spawner is asked; the broker
  reading what the spawner says (the length said, empty, past the bound,
  short, too long, refused, out of step); pieces asked from where the
  last ended, at every length round a piece; the bound, and one byte
  past it after one piece alone; another inode, length or change time;
  a piece of nothing, and a file that comes to more than its length;
  the deadline before each piece after the first.
- **As root, in a real Room** (`room_disk.rs`, `check_room_take`), with
  the static supervisor above: a file of 100003 bytes, every byte value,
  handed back whole by its hash; an empty one; one of exactly 8 MiB, and
  one byte more refused; a link to the image, to a file beside it and
  through a directory, each refused; a pipe and a directory refused; no
  such file; `/workspace/../etc/hostname` and a file in the inbox (a
  mount of its own) refused as out of the workspace; `/etc/hostname`
  refused before the spawner is asked. The Room ran on after it.

## Breaks by hand

**38 breaks, 0 survived**: 36 in the sandbox, 2 as root. The sandbox's ran on tree `98d6a8dafbde1d134dcf293bbb3af8da84ad2f44`, which differs from the tested tree only in one assertion of the spawner's tests (the words of the refusal past the bound, below); the tests passed before and after them. Each named its tests, and each of those had to fail; a
build that failed would have been counted on its own, never as caught.
None did.

- `any-path-may-be-taken`: a path outside the workspace may be asked for.
- `the-bound-takes-its-own-byte`: a take may start at the bound itself.
- `a-long-piece-is-one`: a piece one byte longer than a piece is taken.
- `a-piece-may-go-past-the-end`: a piece past the file's end is taken.
- `a-short-piece-is-one`: a piece one byte short where the file goes on is taken.
- `links-are-followed`: the supervisor follows links.
- `out-of-the-workspace`: the supervisor opens beyond the workspace.
- `other-file-systems-are-crossed`: the supervisor crosses onto another file system (as root, above).
- `any-room-path-is-under-the-workspace`: any path in the Room is opened as if under the workspace.
- `a-pipe-is-waited-for`: the supervisor waits on a pipe (caught by the proof under the filter: a bench would wait for ever).
- `what-isnt-a-file-is-read`: what isn't a file is read.
- `past-the-end-is-read`: a piece from past the end is read.
- `a-short-read-is-a-piece`: a file shorter than it said is handed back.
- `an-interrupted-read-fails`: an interrupted read ends the piece.
- `every-read-is-from-where-it-was-asked`: every read of a piece starts where the piece does.
- `a-changed-file-is-a-piece`: a file that changed while it was read is handed back.
- `a-link-is-said-as-a-failure`: a link is said as a failure, not a link.
- `the-workspace-is-the-root`: the supervisor opens from `/`, not the workspace.
- `the-rooms-workspace-is-the-sessions`: the Room's workspace is left as a session's (as root, above).
- `a-piece-that-cant-be-is-taken`: the session takes a piece that can't be the one asked for.
- `a-gone-caller-is-read-for`: a caller that has gone is read for.
- `a-gone-caller-launches-a-room`: a caller that has gone launches a Room.
- `a-taker-isnt-wanted`: a piece to ask for doesn't want a Room.
- `a-taker-isnt-busy`: a piece to ask for isn't something in hand.
- `a-taker-isnt-owed`: a piece not handed back is never answered.
- `a-failed-launch-forgets-a-taker`: a failed launch leaves a caller unanswered.
- `a-failed-launch-forgets-a-late-taker`: a failed launch leaves a late caller unanswered.
- `a-held-session-says-nothing`: a held session leaves a caller unanswered.
- `the-session-takes-any-path`: the session asks for a path that may not be asked for.
- `a-changed-file-is-put-together`: the spawner puts together a file that changed.
- `the-bound-takes-a-byte-more`: the spawner takes a file one byte past the bound.
- `a-piece-of-nothing-is-asked-again`: a piece of nothing is asked for again.
- `more-than-the-file-is-handed-back`: a file that comes to more than its length is handed back.
- `the-broker-reads-past-the-bound`: the broker reads a file past the bound.
- `a-piece-goes-unsent`: the spawner leaves a piece unsent.
- `the-broker-asks-for-any-path`: the broker asks for a path that may not be asked for.
- `the-deadline-isnt-looked-at`: a take has no deadline.
- `a-real-file-never-changes`: a real file's change time is always 0.

The breaks of the supervisor that only a real Room shows were broken in
a supervisor built static with the break, and the Room's test run as
root against it: with `NO_XDEV` taken out, the inbox's file was handed
back (`/workspace/inbox/0002-given.txt was handed back`); with the
Room's workspace left as a session's, the file couldn't be read.

## Mutation

Every mutant on the lines this change adds or changes, crate by crate,
each with its unmutated baseline first, only that crate's own tests run,
on tree `98d6a8da` (above):

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| hatchling-core | 28 | 26 | 0 | 0 | 2 |
| hatchling-room | 29 | 22 | 1 | 0 | 6 |
| hatchling-broker | 69 | 58 | 2, both caught on the tested tree | 0 | 9 |

- **hatchling-room's one miss** is the Room's own workspace (`Place::room`
  without its `workspace` field): the supervisor's tests run it in a
  directory of their own, never as the Room. It was broken by hand as
  root, above, and caught.
- **hatchling-broker** missed two on the first run: the words a file
  past the bound is refused with were tested against themselves. The
  test now holds the words, and the two were run again on the tested
  tree: 2 caught.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache cleared between: formatting, clippy with `-D warnings` (and with every feature), the build, the whole workspace's tests (135 binaries, 4043 passed, 0 failed, on each), the seed check, the protocol schema and the repository's audit: every step exit 0. The supervisor's own 52 tests and its three proofs under the Room's filter passed built static. As root, the room disk's and the installed Room's tests passed (5 tests, `check_room_take` among them).

## What wasn't tried

- **A file rewritten while it is read, in a real Room.** The supervisor's
  look before and after a piece, and the spawner's across pieces, are
  tested with stand-ins and with a real file between takes, not with a
  job writing the file while a take runs.
- **A file changed in a way its change time doesn't show.** Said in the
  docs; nothing here can stop it, and it doesn't need to: the file
  handed over is the one the broker checks and keeps.
- **An older Room image's supervisor**, which doesn't know `Ask::Take`:
  it can't read the ask, and the session ends, as for any ask it can't
  read. The image and the broker ship together.
