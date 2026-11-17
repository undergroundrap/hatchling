# Evidence: the Library in the app, and kept by the install (WO-041, slice 4, the app's side)

What was tested, how, and what came back, for the app's side of a file
the agent gives: the chat's card beside the broker's notice, Settings'
list of the Library, the download, and the packaged install keeping the
Library. The work order's acceptance on a real install, with a real
model, is its own record, next.

| | |
|---|---|
| Base commit | `61f01ac6` (`feat(broker): list the Library, read a file out of it and remove one (WO-041) (#496)`) |
| Tested tree | `7c33133a372535d294d1625699417bcbf96c1544` |
| Date | 2026-11-17 (UTC) |
| Machine | the app on Windows 11 (Node); the Rust gate on x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned); Node for the app |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **The port** (`session/agent-files.ts`), an optional live capability,
  never a Command, so the app's automation can't drive it: the Library as
  read, a file a message showed, a download, removal; what opening a file
  can do, by its last extension, in any case (a program, a document with
  macros, a page that can run script); and the names the app takes, held
  to the broker's own refusals (no trailing dot, no device name, no file
  Windows acts on when its folder is shown).
- **The live side** (`adapters/live/agent-files.ts`): the list, read when
  the connection opens and when the stream says the Library changed, an
  older answer never replacing a newer; a download, one at a time, that
  holds the broker to every bound it said before keeping a byte (the file
  asked for, a name it gives, at most 8 MiB, the right number of parts,
  the hash the list or the message said), takes each part only in order
  and within the file, checks the whole against the hash, and saves it as
  bytes typed `application/octet-stream` by the broker's name, its address
  let go a minute later; a page without WebCrypto downloads nothing.
- **The chat** (`features/chat/file-out.ts`): beside the notice, that the
  file came from the agent's computer, what opening it can do, and
  Download; a file removed since says so.
- **Settings, Data** (`features/settings/live-agent-files.ts`): the
  Library's use against its bounds and the conversation's standing (off,
  all given, full), then each file by its whole name, its size, when it
  was kept, what opening it can do and its SHA-256, Download and Remove
  behind a confirm.
- **The install**: `packaging/hatchling.env` sets
  `HATCHLING_LIBRARY_DIR=/var/lib/hatchling/library`; `install.sh` makes
  the directory (0700) and adds the setting once to a kept configuration;
  CI's upgrade test takes it out and checks it is added back, and the
  directory's mode.
- **Docs**: your security (what a file the agent gives you can still do),
  storage (the install keeps it, the vault's log lines, backups), the
  local API, the style guide (a file the agent gave you is named whole),
  the work order, the changelog.

## Before the code, and after it

The design was written first, with the broker's side, and reviewed
independently against the code; see [the Library on the local
API](library-api.md). A second independent review, of this diff, before
the runs below, found nothing that saves wrong bytes or puts the agent's
bytes in the page, and found:

- **Two downloads in one tab undid each other**: the broker holds one
  download a connection, and the app guarded only the same file twice.
  Now one file downloads at a time.
- **The hash wasn't shown**, though the design said it would be. Settings
  shows it, whole.
- **The table of what runs had gaps**: older Office documents, more
  script and package kinds, XML pages. Added.
- **The app's names were weaker than the broker's**: a name ending in a
  dot read as having no extension, though Windows saves it without the
  dot. The app now refuses what the broker refuses.
- **A late list answer could replace a newer one.** Now it can't.
- **Two guards had tests that couldn't fail**: a part larger than a part,
  and a file short of its length whose missing tail is zeros. Each has
  one now.
- **Docs**: the changelog's earlier line said the install would set it
  next; the storage page didn't say the vault also logs each chunk it
  removes; a download given up on holds its room for a minute.

Left as they are, and said: Settings, like the inbox's list, is drawn
again when it is opened or refreshed, not as the stream changes it; the
app says it gave the browser the file to save, not that the browser saved
it; and whether Windows marks a saved download as from the internet is
for the acceptance run to find.

## Tests

- **The live side** (`live-agent-files.test.mjs`, 19 tests): a file read
  out in order, checked, saved by its name; a file of no bytes; a page
  without a hash downloads nothing; each refusal and no answer, said, and
  nothing saved; a broker that says another file, another name, more than
  8 MiB, other parts or another hash, never downloaded from; a part out of
  step, another download's, not base64, larger than a part, past the file,
  or the file short or another's, nothing saved; the list read on
  connecting and said as answered; removal, and a message's file marked
  removed, even when the list can't be read again; a file given since the
  list was read not said removed; a name the broker would never give,
  never offered, listed or downloaded; one download at a time; an older
  list answer changing nothing; the adapter keeping a file the stream
  gave and reading again when the Library changed; the save typed as
  bytes whatever the name, its address let go after a minute; the
  browser's SHA-256, and none without WebCrypto; what each kind of file
  can do, in any case.
- **The chat and Settings** (`file-out.test.mjs`, 5 tests): the card and
  its download, a program said to be one, one downloading offering no
  second, a failure said as an error; nothing for a removed file, another
  message or no Library; Settings' list, whole names, the hash, Download,
  Remove only after the confirm, Refresh; off, all given, full, empty and
  odd names; not read yet, failed, downloading, and nothing without a
  Library.
- **The app's check** (`npm run check`): 556, 227 and 16 tests passed, 0
  failed. **The live adapter's coverage**, run from Git Bash as CI runs
  it: 100% of lines, branches and functions; the operator's, 100%.

## Breaks by hand

**30 breaks of the app, 0 survived.** Each was built and its tests had to
fail; the tests passed before and after them. A build that failed would
have been counted on its own, never as caught: none did. Each break, by
its name:

- `the-blob-is-typed-as-a-page`.
- `the-address-is-let-go-at-once`.
- `no-hash-check`.
- `no-length-check`.
- `a-part-of-any-size`.
- `a-part-past-the-file`.
- `another-downloads-part`.
- `another-file-begun`.
- `any-name-begun`.
- `any-length-begun`.
- `any-parts-begun`.
- `the-lists-hash-is-ignored`.
- `no-digest-downloads-anyway`.
- `a-given-name-is-any-name`.
- `the-list-takes-any-name`.
- `a-removal-leaves-the-message`.
- `two-downloads-at-once`.
- `an-older-list-is-taken`.
- `a-file-given-since-reads-removed`.
- `extensions-by-case`.
- `a-program-isnt-said`.
- `a-trailing-dot-is-given`.
- `a-device-name-is-given`.
- `a-reaching-name-is-given`.
- `a-removed-file-offers-download`.
- `a-second-download-offered`.
- `settings-removes-without-asking`.
- `settings-lists-any-name`.
- `settings-hides-off`.
- `settings-hides-the-hash`.

An earlier run had one survive, `a-removal-leaves-the-message`: the list
read after a removal marked the file gone anyway. A test now removes a
file when that read fails. Seven breaks had first been written so they
didn't build; each was written again to build.

## Mutation

No Rust code changed in this slice; the app's guards are held by the
breaks above and its coverage gates.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache
cleared between: formatting, clippy with `-D warnings` (and with every
feature), the build, the whole workspace's tests (135 binaries, 4103
passed, 0 failed, on each, the room disk's test of the packaged
configuration among them), the seed check, the protocol schema and the
repository's audit: every step exit 0.

## What wasn't tried

- **A real install, a real model, a real browser**: the acceptance run,
  next, gives a file each way, compares the saved file's hash, and looks
  for the Mark of the Web on it.
- **The installer itself**: CI's install and upgrade tests run it; it
  wasn't run here.
- **A browser's save dialog**: the save was tested to the anchor's click.
