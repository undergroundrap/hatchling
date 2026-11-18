# Evidence: a folder handed over (WO-043, slice 3)

What was tested, how, and what came back, for the change that gives the
agent `give_folder`: the broker walks a folder in the Room, reads each
file in it, checks every name and byte for the secrets it keeps, and
keeps it in the Library as one item. The app shows and downloads folders
from slice 4, released with this one.

| | |
|---|---|
| Base commit | `42e7079e` (`feat(broker): keep folders as Library items, grouped and bounded (WO-043) (#510)`) |
| Tested tree | `2a865df409aa58d09835144187b936f972eb0a5f` (the gate); the breaks and the full mutation on `8a6971a4`, the rerun of `hand_over.rs`'s mutants on `512783ab`. Since `8a6971a4` only tests changed in the crates, with the protocol schema and its generated types, the app's version pin, `docs/arch/protocol.md`'s version and one `allow` for Rust 1.85 |
| Date | 2026-11-18 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **The tool and the op** (`hatchling-agent`, `hatchling-core`):
  `give_folder { path }`, offered where `give_file` is, and session
  protocol v33: `RoomOp::HandOverFolder`, `RoomResult::FolderHandedOver
  { name, files, bytes, left_out }` and `LeftOut { links, mounts, others
  }`. A refusal is `NotHandedOver`.
- **The walk** (`folder_walk.rs`): breadth first, a folder at a time,
  by the spawner's listings. Folders and files are counted as each
  listing names them: at most 256 folders (the one given among them),
  512 files, 8 MiB a file, 32 MiB in all. Each path is held to the
  Library's rules (`check_paths`) as it is found, and with the folder's
  own path to 1024 bytes. A name that isn't UTF-8 refuses the folder, and
  so do two names in one folder that are one but for case. Each folder's
  listing is held to the inode its parent listed. Links, other mounts and
  devices, pipes and sockets are left out and counted, and never opened.
  So is the workspace's `inbox`, by its name, whatever it is said to be.
  The deadline is checked before each listing.
- **The hand-over** (`Handing::hand_folder`):
  1. As a file's: refused before anything is listed when it isn't the
     chat's run in the open conversation, giving is off, or the
     conversation or the Library has no room.
  2. The path is held to the Room's rules and kept out of the inbox. The
     folder's own name is held to a file name's rules.
  3. The walk. A walk refused spends nothing.
  4. Spent on the listed files and bytes, after the deadline is checked
     again.
  5. Each file read through `take` and held at once to its listed length.
     What a file reads past it counts toward the conversation's bytes.
  6. The name is checked alone. Then the paths, a newline after each,
     another newline, and every file's bytes back to back are checked as
     one text, which is the only copy.
  7. The stale check, then the keep, with what was left out.
  No name reaches Activity or the app before it is checked and found
  clear.
- **The Library** (`library.rs`): an item keeps `left_out`, nothing for
  a file; `read_more` counts bytes read past a spend.
- `room_ops` dispatch and the no-Room answer; the run trail;
  `docs/session-protocol.md`, `docs/arch/trust-boundary.md`,
  `docs/your-security.md` (and the app's copy), `docs/arch/distribution.md`
  and the changelog.

## Before the code, and after it

The design was reviewed independently before any code, and the diff
again before the runs. Neither found a way to get a secret out
unchecked, a check not paid for by a spend, or a keep into the wrong
conversation.

From the design review, five serious findings, all taken:

- **The pre-check as written refused every folder**: it asks with one
  file.
- **The walk's memory**: folders and files are counted as found, so
  nothing past the bounds is queued.
- **A file listed short and grown** could make the broker hold
  gigabytes: each read is held to its length before the next.
- **A short secret** (under 8 bytes) is found by the keeper only as a
  whole text: a file that is exactly one is withheld by `give_file`,
  but inside a folder's one text it isn't. Checking each short file
  alone would test up to 512 short guesses a hand-over, so it is said
  instead (`docs/your-security.md`) and pinned by a test. A short value
  inside any longer file already passes `give_file`.
- **The folder's name in Activity before its check**: every refusal
  before the check names nothing.
- Also: directory case twins per listing; the whole path's length;
  depth counted as the Library counts it; no rules on what is left
  out; the no-Room answer; a trailing slash refused before naming; the
  deadline checked before the spend; left-out counts kept in the item.

From the diff review, three findings, all taken:

- **Bytes read past a listed length weren't counted**: they are now
  (`Library::read_more`).
- **The inbox was held by name only at the workspace's top**: a folder
  given in the inbox is refused before it is walked.
- **The deadline's real ceiling** is 5 minutes and one ask's time: said.
- And tests for what a one-line break would pass: files and bytes
  counted before a later folder is listed, an unchecked name never in
  Activity, the dispatch to the folder's hand-over, a secret split across
  files through the broker's real checks, a new conversation with a
  secret found, and the inbox said to be a folder.

Filed, older than this slice: `CONIN$`, `CONOUT$` and `CLOCK$` aren't
device names to the shared rule
([#511](https://github.com/undergroundrap/hatchling/issues/511)).

## Tests

- **The walk** (`folder_walk::tests`, 10), through a stand-in
  Room holding a tree:
  - breadth first, nothing read;
  - each kind left out and counted, at any depth, whatever its name;
  - the inbox by name, said to be a mount, a folder or a file;
  - every name rule, at any depth, its words the Library's and never the
    path;
  - case twins of files and folders;
  - a folder swapped;
  - an empty folder;
  - the deadline, before and between listings;
  - each bound at its edge and one past, counted before a later folder
    is listed;
  - depth, and the whole path at 1024 bytes and one past.
- **The hand-over** (`hand_over::folder_tests`, 20):
  - a folder walked, spent, read, checked as one text and kept, its files
    read back byte for byte, Activity by name and size, the app told
    nothing;
  - each refusal before the walk costing nothing (16 spent, off, the
    path's rules, a trailing slash, outside the workspace, the inbox, a
    reserved name, a trailing dot, stale, not the chat);
  - a walk refused, unspent and unchecked;
  - the conversation's bytes passed, refused at the spend, unread;
  - a file longer or shorter than listed stopping the reads, with the
    overshoot counted;
  - a take refused;
  - a secret in the last file, a deep path, or split across two files,
    withholding all and turning giving off for a file and a folder;
  - the folder's name holding one, never named;
  - unchecked;
  - a file withheld turning off a folder;
  - the deadline at the walk, before the spend and between reads;
  - a new conversation while reading, with and without a secret;
  - a run ended while reading, its unchecked name never shown;
  - the workspace given without its inbox;
  - a short secret found alone but not in a folder;
  - the largest text within one check;
  - the broker's real checks finding a secret split across files.
- **Around it:** the wire form (`messages::room_tests`), the tool's words
  and result (`tools::tests`), the loop offering `give_folder` only where
  `give_file` is (`tests/loop.rs`), the no-Room and dispatch
  (`room_ops::tests`), the run trail, and a file item with anything left
  out read as damage (`library::tests`).

The walk's Room in these tests is a stand-in. What a real Room's
listings say of links, pipes, sockets and the inbox, and its takes, were
proved as root in slice 1 ([room-list-wire.md](room-list-wire.md)). The
whole path on a real install with a real model is the work order's
acceptance (slice 7).

## Breaks by hand

**48 breaks, 0 survived.** Each named its tests, and each of those had
to fail; the tests passed before and after them. None failed to build.

- The walk (17): links, mounts or others uncounted; the inbox walked,
  the inbox left out anywhere, or counted as nothing; names unruled;
  twins unfolded; a folder unpinned; whole paths unbounded; one folder
  too many; folders, or files, counted only after the walk; one file too
  many; a file or a folder of any size; a walk without end; an empty
  folder given.
- Before the walk (6): no pre-check; the path unchecked before naming;
  no name rules; the inbox given; a folder in it given; the inbox by
  prefix (`inboxes` refused).
- The spend and the reads (8): spent on nothing; a spend past the
  deadline; reads without end; lengths unheld, or held only at the end;
  the overshoot uncounted; a name in Activity at the spend, or at a
  read.
- The check and the keep (11): paths, files, the last file or the name
  unchecked; a held name shown, or an unchecked one; unchecked kept; no
  stale check; left out unkept or unsaid; a file with anything left out.
- Around it (5): no Room a failure; a folder a failed step; a folder told
  without what was left out; another wire name; the folder handed over
  as a file.

The first runs of the breaks found three that weren't caught, all for
the breaks' own reasons, not the code's: the workspace test listed the
inbox as a mount, so its name guard went untried (it is said to be a
folder now); a break named a type the file doesn't import, and failed
to build; and one named, as its guard, that same test, which no longer
reaches the mounts' count. Each was fixed and run again, caught.

## Mutation

Every mutant on the lines this change adds or changes, each crate with
its unmutated baseline first, its own library tests run:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 0 | | | | |
| `hatchling-agent` | 14 | 11 | 0 | 0 | 3 |
| `hatchling-broker` | 80 | 60 | 6 | 0 | 14 |
| `hatchling-broker`, `hand_over.rs` again | 30 | 25 | 0 | 0 | 5 |

The core's change is types and their words: no mutant. Unviable is an
answer replaced with a default its type has none of, or arithmetic in
a constant that no longer builds. The broker's full run missed six, two
tests missing:

- `5 * 60` in the folder's time, to `5 + 60`: a test pins it to 300 s
  and to its words.
- Five in the one reservation a folder's text is read into (the paths'
  lengths and their newlines): reserving less still works, but the text
  is copied as it grows. A test reads a folder and holds its text's
  capacity to its length, and each file's place in it.

The run after those tests, of every mutant in `hand_over.rs`, is the
last row. Nothing else in the broker changed but tests.

## Measured

A folder's cost is its walk, its reads and its keep. The keep of 32 MiB
took about 8.5 s on ext4 (slice 2, [library-items.md](library-items.md)).
The walk and reads go through the spawner a listing and a file at a time.
Their time on a real install is recorded with the acceptance.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache
cleared between: formatting, clippy with `-D warnings` (and with every
feature), the build, the whole workspace's tests, the seed check, the
protocol schema and the repository's audit: every step exit 0, and on
each toolchain 135 test binaries, 4186 tests passed, 0 failed, 0
ignored. The 28 lines of its log that say "skipped" are test names, as
before. The mutant lists on the tested tree are the same 14 and 80 as
the runs', and none for the core.

The first run of the gate failed on two things, both fixed: Rust 1.85
called the folder check's bound unused (its uses are a constant
assertion and a test), and `docs/arch/protocol.md` still said v32, which
the audit holds to the code.

The app's checks, with the protocol schema regenerated and its types
generated from it: 565, 235 and 16 tests passed, none failed, no lint
or design error. Its one change of substance is the session protocol's
version, 33, which its test pins.
