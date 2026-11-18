# Evidence: the Library's folders through the local API (WO-043, slice 4)

What was tested, how, and what came back, for UI schema v8: the
Library's list carries folders and every item's group, a folder's list
is read in pages, a file of a folder is read out, a folder is removed,
and the app lists folders and every item it can't name, each to be
removed. Downloading a folder as one `.zip` is slice 6. Slices 3 and 4
ship in one release.

| | |
|---|---|
| Base commit | `15576030` (`feat(broker): the agent gives the owner a whole folder (WO-043) (#512)`) |
| Tested tree | `7492573397d4157463059fcb6caef68cdba68209` (the gate); the breaks and the rerun of `api_ops.rs`'s mutants on `6fafa27a`, which differs only in `docs/arch/protocol.md`'s version; the full mutation on `fab9a94e`, before `api_ops.rs`'s hold and check became one function and two tests were split |
| Date | 2026-11-18 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's checks on Windows 11, Node, from Git Bash |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **The wire** (`hatchling-core`, UI v8):
  - `list_library` answers `folders` beside `files`. Each item says its
    `conversation` and `reply`; a folder says its count of `files`, its
    list's `sha256` and what was `left_out`.
  - The standing adds `files_in_all` (of 4096), `read` (of 128 MiB) and
    the open `conversation`.
  - `list_folder { id, from }` answers `folder` with at most 32
    `entries`.
  - `begin_file_out { id, file }` answers `folder_file_out_begun` for a
    folder's file.
  - The refusals add `not_a_folder`, `a_folder` and `out_of_range`.
  - Activity's `file_out` adds a folder's `files`.
- **The broker**:
  - **One read at one moment.** The list, the standing and the bytes read
    come from one read of the Library (`Library::shelf`). A folder's file
    comes with its list entry, the list read once (`Library::entry_out`).
  - **Download order.** A download lets the connection's last one go
    before anything else, refused or not. Then a busy broker reads
    nothing.
  - **Checked before any read.** A folder's file is asked for by its
    place, checked against the folder's count before anything is read.
  - **No copy of a removed item.** A copy is held under its item's id and
    let go if the item was removed while it was read.
  - **A kept folder** tells every app `library_changed`, and its Activity
    line carries its count of files.
- **The Library**:
  - **8 MiB per file.** Each file of a folder is held to a file's 8 MiB,
    at the keep and when its list is read back. A forged list's 32 MiB
    file can't make a download hold more than one file may.
  - **The conversation's number** is held at or under 2^53 - 1 in the
    state and the index. Past it, a new conversation is refused, never
    wrapped, so two conversations never share a number.
- **The app**:
  - **Settings' Library** lists folders by name, files and size, with
    Remove. It lists every item it can't name, a file past 8 MiB among
    them, by its size alone, so nothing the Library counts is out of
    sight. Remove is offered only for an id the broker gives.
  - **The standing's words** count items and files in all, hand-overs,
    and the conversation's bytes read.
  - **Download all** says it takes the files, not the folders.
  - **Activity** says a folder the agent gave, with its files.
- **Untouched:** the frozen adapter contract (`src/protocol/*`,
  `PROTOCOL-LOCK.json`). The app's seam check says so.
- **Docs:** `docs/local-api.md`, `docs/arch/distribution.md` and the
  changelog.

## Before the code, and after it

The design was reviewed independently before any code, and the diff
again before the runs. Neither found a path or name reaching the app or
the disk unchecked.

From the design review, four serious findings, all taken:

- **No 8 MiB bound per file of a folder:** added at the keep and at the
  list's read.
- **An item the app can't name** would be counted and shown nowhere: it
  is listed by its size, to be removed.
- **The conversation's number** had no bound and wrapped: bounded, and
  refused past it.
- **The audit** is a line a call: kept so, never a path or a name; the
  threat model's "a line a folder" is corrected.

From the diff review, no blocking defect; taken:

- one lock for a folder file's entry and bytes;
- a copy begun as its item was removed let go;
- the last download let go first, as the docs say;
- every item shown in the app, removable only by an id the broker gives;
- Activity's words;
- tests for each guard a one-line change would have broken unseen;
- the design note's "saturate" corrected to "refuse".

## Tests

- **The API** (`api_ops::tests`):
  - a folder listed with its group and counts, its list in pages, a
    file of a folder read out by its place to its own hash, every
    refusal;
  - the standing and each item's conversation;
  - a folder removed with a copy of its file let go;
  - an old item listed with no reply;
  - busy reads nothing, for a damaged file and a damaged folder's file;
  - a copy of an item removed while it was read let go;
  - the largest folder (512 files) read whole through the API, every
    page and every file to its hash;
  - the largest page and list in one frame, by the Library's own bounds.
- **The Library** (`library::tests`):
  - a list with a file past 8 MiB reads as damaged;
  - a folder with one isn't kept, and one of exactly 8 MiB is, and reads
    back;
  - the conversation's bound in the state and the index, and no wrap;
  - the shelf.
- **The wire** (`ui::tests`): the new results, actions and events; the
  largest page and list fit one frame.
- **The app**: 571, 238 and 16 tests. Among them:
  - folders and items it can't name read from the list, each bound at
    its edge;
  - Settings' rows and their confirms;
  - "Nothing yet" only with nothing at all;
  - the standing's words;
  - Activity's folder line.

  The live adapter keeps 100% coverage.

## Breaks by hand

**28 in the broker and the core, 0 survived; 7 in the app, 0 survived.**

- **The broker and the core:**
  - a page past 32;
  - a page from past the end;
  - a file's list asked;
  - a longer page said;
  - any place read;
  - a file's place ignored;
  - a folder read as its first file;
  - another entry said;
  - the first file read;
  - a removed folder's copy kept;
  - folders unlisted;
  - an empty reply said;
  - the reads, the files in all, the standing's or an item's
    conversation, or a folder's left-out unsaid;
  - a listed or a kept file of any size;
  - a state or an item of any conversation;
  - conversations wrapping;
  - a shelf of another moment;
  - a folder kept silently, or its files unsaid;
  - a removed item's copy held;
  - a busy broker reading;
  - the last download held past a refusal.
- **The app:**
  - an unnamed item dropped;
  - any id removable;
  - a folder's name unchecked;
  - a folder of any count;
  - a file of any size;
  - a stuck item removable;
  - "Nothing yet" over folders.

The first runs found three broker breaks whose named tests didn't fail,
each caught by another (the frame tests, the folder test). The 512-file
test now holds each file's path and hash to its entry. Two app breaks
left a constant unused and didn't build. Each was rewritten so it
builds, and is caught.

## Mutation

Every mutant on the lines this change adds or changes, each crate with
its unmutated baseline first, its own library tests run:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 1 | 0 | 0 | 0 | 1 |
| `hatchling-broker` | 68 | 50 | 2 | 0 | 16 |
| `hatchling-broker`, `api_ops.rs` again | 25 | 18 | 0 | 0 | 7 |

Unviable is an answer replaced with a default its type has none of. The
broker's full run missed two: the check that lets go a copy of an item
removed while it was read, at its two call sites. A removal can't be
made to land in that instant from one thread, so the call sites were
untestable. The hold and the check are now one function, `hold`, which
a test calls with a removed item: the copy is let go and the answer is
`gone`. The run after it, of every mutant in `api_ops.rs`, the only file
changed since, is the last row.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache
cleared between: formatting, clippy with `-D warnings` (and with every
feature), the build, the whole workspace's tests, the seed check, the
protocol schema and the repository's audit: every step exit 0, and on
each toolchain 135 test binaries, 4197 tests passed, 0 failed, 0
ignored. The 28 lines of its log that say "skipped" are test names, as
before. The mutant lists on the tested tree: 1 for the core, 62 for the
broker (the full run's 68 less the guards `hold` took in).

The first run of the gate failed on one thing: `docs/arch/protocol.md`
still said the local API was v7, which the audit holds to the code.

The app's checks, on its final code with the protocol schema regenerated
and its types generated from it: 571, 238 and 16 tests passed, none
failed, no lint or design error, and the seam check: the frozen
contract's four files unchanged.

After the record's first push, CI's live-adapter coverage found one
branch of Activity's folder line never taken (a fallback for a count a
folder always has): 99.91% of branches against 100%. The app's local
check had passed it vacuously: run through Windows' `cmd`, its coverage
include matches no file. The line was rewritten without the fallback,
and the same gate, run from bash with CI's quoting, covers 100% of the
live adapter's lines, branches and functions. No Rust changed.
