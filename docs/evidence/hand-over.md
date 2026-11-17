# Evidence: the agent hands the owner a file (WO-041, slice 3, the hand-over)

What was tested, how, and what came back, for the agent's `give_file`
tool and the broker's hand-over behind it: the file read out of the Room,
checked, and kept in the Library, or withheld. The app shows a notice of
it; the Library's list, download and removal in the app are the next
change, and the packaged install turns the Library on only with them.

| | |
|---|---|
| Base commit | `0c9f3837` (`feat(broker): the Library, a sealed store for the files handed over (WO-041) (#494)`) |
| Tested tree | `8675d22865fb82d8dd3162687f6f5a5d25382ba0` |
| Date | 2026-11-17 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's tests on Windows 11 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0, Node for the app |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **The wire**: session protocol v32 adds `RoomOp::HandOver { path }` and
  its answers `HandedOver { name, bytes }` and `NotHandedOver { reason }`;
  UI schema v6 adds the events `file_offered` (the Library's id, name,
  length and SHA-256) and `file_withheld` (a name, or none), and the
  Activity line `file_out` (a name or none, a length, and how it ended:
  offered, withheld, unchecked or refused).
- **The hand-over** (`hand_over.rs`), in order: the broker has a Library;
  the session is the broker's own run of its cell, the chat's, in the
  conversation still open; the conversation can give another; the name
  the broker makes from the file's own isn't one Windows keeps for a
  device and doesn't end in a dot. Only then is the Room asked for the
  file, under `/workspace`. The file and its name are checked for the
  secrets the broker keeps, by the gate's scan and by the keeper's check;
  the conversation is asked after again, and then the file is withheld
  (giving files off for the conversation), not given because it couldn't
  be checked, or kept in the Library and offered.
- **The tool group `files`** and the agent's `give_file`: offered only
  with the Room and a Library, and to a chat's run only. The agent's note
  of its tools says how files go each way, and says it can't give one
  where it can't.
- **The keeper** takes `file-out-gate` as a gate's scan, and checks a file
  out as `file-out-check`.
- **A new conversation** gives files again, from none, once it has
  started; what was given stays.
- **The app** shows the agent's file as a notice of its own and a withheld
  one as an error, with the name only when it is one the broker makes;
  Activity has a thread of the files the agent gave.
- The session protocol, local API, trust boundary and security pages, the
  changelog and WO-041.

## Before the code, and after it

The design was reviewed independently before the code, with the store's
(see [the Library's store](library-store.md)). A second independent
review, of this diff, before the runs below, found no way to get a secret
out through `give_file` beyond the one accepted: a file is one guess at
many secrets at once, so one found turns giving files off for the
conversation. It found six things to change, each changed before the
runs:

- **Refusals filled Activity.** Every refusal, even before anything was
  read, wrote a line, so a session asking over and over could grow the
  log without bound. Now nothing refused before the file is read writes
  a line, and a session writes at most 32.
- **The check that a session is the broker's own run had no test of its
  own.** A session the broker didn't start names any cell in its hello.
  Now a test opens a conversation for a cell and shows a session not
  started as that run refused before the Room is asked, and the run
  itself asking it.
- **A name whose check failed could be shown** on a withheld file. Now a
  name is shown only when it was checked and found clear.
- **The open conversation was asked after once.** The owner may start a
  new one while a file is read: then nothing is kept, told, or turned off.
- **Tests were missing** for the trail's endings, the agent's words for
  each answer, a session reaching its hand-over, and a new conversation
  that fails to start leaving the Library as it was.
- **Words and names**: the security page said off stops many guesses at
  once, where it stops trying again; Activity's row in the app showed a
  name without holding it to the broker's; `COM0` and `LPT0` were taken
  as names.

**The switch.** Giving files is on wherever the operator sets
`HATCHLING_LIBRARY_DIR`, which the packaged install doesn't yet. An
operator who sets it by hand before the next change has a Library the app
can't list or download from, emptied only by a reset. The maintainer
accepted this rather than a second switch.

## Tests

- **The hand-over** (`hand_over::tests`): a file kept, offered and in
  Activity, its bytes read back from the Library and the Room asked under
  `/workspace`; a file holding a secret withheld, giving off, and the next
  refused before the Room is asked; a name holding one never shown or
  kept; a name whose check failed never shown; a file that couldn't be
  checked never told as a secret, and nothing turned off; either check
  finding a secret is one found; seven refusals before the read (no
  Library, no cell, not the chat's, stale, a device name, a trailing dot,
  a conversation that has given 16), each with the Room unasked and no
  Activity line; the Room's refusal and a file past 8 MiB said as they
  are; a conversation that ended while the file was read keeps nothing
  and turns nothing off, whatever the file held; 32 lines a session; the
  broker's name for a file; the broker's own checks against a real vault,
  each check by itself; only the broker's own run of a cell hands a file
  over.
- **Around it**: the Room session asks its hand-over only when attended
  and with a Room (`room_ops`); the trail's endings (`run_trail`); a chat's
  run alone has the Library and the `files` group, and ideas may need
  files only where they can be given (`run_launcher`, `suggestions`); a new
  conversation turns giving back on and leaves the files, and one that
  fails leaves it off (`api_ops`); the keeper scans for `file-out-gate`;
  the Spawner's `take` through `Rooms`.
- **The agent**: the loop asks the broker to give a file only with the
  Room and `files`, and otherwise the call is no tool's (`tests/loop.rs`);
  the tool's description; its answers in the model's words; the note of
  its tools.
- **The wire**: the op and its answers, and the events, in their wire
  form; the schema the app is generated from, checked.
- **The app**: the notices, by the broker's name only when it is one;
  Activity's thread of files given; a Library name's grammar.

## Breaks by hand

**33 breaks of the broker and the agent, 0 survived; 6 of the app, 0
survived.** Each named its tests, and each of those had to fail; the
tests passed before and after them. A build that failed would have been
counted on its own, never as caught: none did. Each break, by its name:

- `any-session-is-the-chats`.
- `any-run-is-the-chats`.
- `a-stale-run-gives`.
- `a-spent-conversation-gives`.
- `a-refusal-before-the-read-is-recorded`.
- `a-device-name-is-kept`.
- `com0-is-a-name`.
- `a-trailing-dot-is-kept`.
- `the-room-is-asked-outside-the-workspace`.
- `the-name-is-the-path`.
- `the-name-isnt-checked`.
- `the-file-isnt-checked`.
- `an-error-outranks-a-secret`.
- `an-unchecked-name-is-shown`.
- `a-held-name-is-shown`.
- `a-conversation-ended-meanwhile-gives`.
- `an-unchecked-file-is-given`.
- `an-unchecked-file-is-told-withheld`.
- `a-secret-found-leaves-handing-on`.
- `a-withheld-file-is-named-anyway`.
- `one-more-line-is-recorded`.
- `lines-arent-counted`.
- `the-gate-scan-finds-nothing`.
- `the-keepers-check-is-skipped`.
- `the-gate-scan-alone`.
- `the-keeper-reads-for-a-file-out`.
- `a-new-conversation-stays-off`.
- `every-run-has-the-library`.
- `files-without-a-library`.
- `files-without-a-room`.
- `ideas-may-need-files-anywhere`.
- `give-file-without-files`.
- `the-note-always-says-give-file`.

The app's: `a-given-row-shows-any-name`, `an-offered-notice-shows-any-name`,
`a-withheld-notice-shows-any-name`, `a-library-name-may-be-hidden`,
`a-library-name-may-be-longer`, `a-withheld-file-is-no-error`.

An earlier run of these breaks had two survive, each a test that couldn't
fail: the check that a session is the broker's own was tested where no
conversation was open, so every session was refused anyway; and the
gate's scan was tested with a secret the keeper's check finds too. The
tests above were written for each; the run on the tested tree is the one
counted.

## Mutation

Every mutant on the lines this change adds or changes, crate by crate,
each with its unmutated baseline first, only that crate's own tests run,
on the tested tree.

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| hatchling-core | 5 | 3 | 0 | 0 | 2 |
| hatchling-agent | 12 | 10 | 0 | 0 | 2 |
| hatchling-broker | 70 | 54 | 0 | 0 | 16 |

Every unviable mutant is a function's answer replaced with its type's
default, which has none. A run on the tree before the tested one missed
five: the tool's description (two) and the Spawner's `take` through
`Rooms` (three). Each has its test now.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache
cleared between: formatting, clippy with `-D warnings` (and with every
feature), the build, the whole workspace's tests (135 binaries, 4086
passed, 0 failed, on each), the seed check, the protocol schema and the
repository's audit: every step exit 0. The app's check (`npm run check`):
531, 207 and 16 tests passed, 0 failed.

## What wasn't tried

- **A real model giving a real file** from a real Room on an install: the
  hand-over was driven with a stand-in Room and stand-in checks, and with
  the broker's own checks against a real vault; the Room's read of a file
  was proven as root in [its own record](room-take-wire.md). The next
  change's acceptance gives a file each way on a real install.
- **A keeper running.** In these tests no keeper runs, so the keeper's
  check is the broker's own; the keeper's side of `file-out-check` is the
  keeper's tests'.
- **The packaged install**, which doesn't set `HATCHLING_LIBRARY_DIR` yet.
