# Evidence: Download all as one .zip (#500)

What was tested, how, and what came back, for downloading every file the
agent gave as one `.zip`, made in the app.

| | |
|---|---|
| Base commit | `60ce8b25` (`docs(evidence): close WO-041 with files each way on a real install (#499)`) |
| Tested tree | `96cbba36c18c1592de2bb2f29c89f865642d230f` |
| Date | 2026-11-17 (UTC) |
| Machine | Windows 11, Node 22, its time zone not UTC |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## Why

In WO-041's acceptance run, a browser set to ask where to save each
download asked once for every file the agent gave. With many files, that
is a prompt each.

## What changed

- **Download all as one .zip**, in Settings, Data's Library, with two
  files or more. Each file is read out of the Library and checked against
  its SHA-256 exactly as a single download is, one at a time, with a count
  of how many are done; the browser is given one `.zip` to save. A file
  that fails anywhere saves nothing, and the owner is told which. While it
  runs, no single download starts, and none runs alongside it.
- **The .zip** (`adapters/live/zip.ts`): each file stored as it is, with its
  CRC-32 and the time it was kept (in this computer's time, as unzip tools
  read it), under the Library's plain name: no folders, so nothing in it
  unpacks outside the folder it's unpacked into. Two of one name are told
  apart, whatever their case (`report-2.pdf`).
- **The toast** says how many of the files can run something when opened.
- **A throw on the way** (memory, a save that fails) says nothing was
  saved, for one file and for all.
- your security, the changelog.

## Before the code, and after it

Planned in the issue first. An independent review of the diff, before the
runs, found nothing that loosens a single download's checks, and the
format and the names sound. It found:

- **Telling the agent to give several files as one archive weakens the
  secret check**: a compressed archive hides a secret from it, and an
  archive the agent made carries neither the app's per-file warnings nor
  the no-folders promise. That part was dropped; Download all needs none
  of it.
- **The security page claimed more than was measured** about Windows'
  mark. It now says what was seen, and for which tools.
- **A thrown error was silent**, there was **no progress** for a long
  download, the toast **didn't say** when files could run, and the times
  were **UTC** where unzip tools read local time. Each was changed.
- **The zip test didn't check every field** of the headers. It does now.

## Tests

- **The .zip** (`live-zip.test.mjs`): read back by its own records, each
  field checked (signatures, versions, the UTF-8 name flag, stored sizes,
  local and central times agreeing, offsets, the end record's counts and
  directory size), each file's CRC-32 against Node's; names distinct in any
  case, before the last extension; times to the two seconds a `.zip` can
  say, and 1980-01-01 for one it can't; CRC-32 by a known answer.
- **Download all** (`live-agent-files.test.mjs`): every file, each checked,
  saved once, with the right count of files that run; two of one name and
  their kept times end to end; the count of files done; a file that fails,
  nothing saved and named; nothing listed, not read yet, no hash to check
  with, or a download under way; a throw, for one file and all.
- **Settings** (`file-out.test.mjs`): offered with two files or more, not
  one; the count while it runs; the toast, and a failure as an error.
- **The app's check**: 565, 235 and 16 tests passed, 0 failed. **The live
  adapter's coverage**, from Git Bash as CI runs it, 100% of lines,
  branches and functions; the operator's, 100%.

## A real .zip, read by others

A `.zip` the app made of three files (two of one name, one of 300,000
bytes, one empty) was read by:

- **Python's `zipfile`**: `testzip()` found nothing wrong; each file whole,
  stored, with its time.
- **PowerShell's `Expand-Archive`**: each file unpacked whole.
- **Windows' Explorer**, through its own zip handling, on the same `.zip`
  marked as from the internet: each file it unpacked was marked
  `ZoneId=3`. `Expand-Archive`, on the marked `.zip`, left the files
  unmarked. The security page says both.

## Breaks by hand

**17 breaks, 0 survived.** Each was built and its tests had to fail; the
tests passed before and after them. None failed to build.

- `another-crc`, `names-not-flagged-utf8`, `a-compressed-size-of-nothing`,
  `two-files-of-one-name`, `names-apart-by-case`, `offsets-wrong`,
  `a-short-count`, `times-in-utc`, `times-of-any-year`.
- `a-failed-file-is-left-out`, `all-while-one-downloads`,
  `progress-isnt-counted`, `running-files-arent-said`,
  `a-thrown-zip-is-silent`, `a-thrown-download-is-silent`.
- `one-file-offers-all`, `progress-isnt-shown`.

`times-in-utc` is caught here because this computer's time zone isn't
UTC; on a computer in UTC the two are the same, as on CI.

## What wasn't tried

- **A full Library** (64 files, 256 MiB) as one `.zip`: its memory and the
  pause while it's put together weren't measured.
- **The browser's own mark on the saved .zip**: the single files saved in
  [WO-041's acceptance run](files-acceptance.md) were marked; the `.zip`
  is saved the same way, but wasn't saved and looked at here.
