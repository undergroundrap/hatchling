# Evidence: one command frees the disk space WSL keeps (#520)

What was tested, how, and what came back, for
[#520](https://github.com/undergroundrap/hatchling/issues/520): on
Windows, a distro's disk file never shrinks by itself, and compacting it
was a page of manual steps.

| | |
|---|---|
| Base | `main` at `9de4137f` |
| Tested tree | `a0976822` |
| Date | 2026-11-21 (UTC) |
| Machine | x86_64, Windows 11 Home; Windows PowerShell 5.1 |

This record was added after the runs, in its own commit. It changes no code.

## The problem

A WSL distro's `ext4.vhdx` grows to its peak use and never shrinks.
Deleting files inside frees space for the distro, not for Windows. The
fix was manual: find the file in the registry, `wsl --shutdown`, then
diskpart in an administrator PowerShell. On one machine diskpart found
the file in use after every shutdown, because something started WSL
again within a second. What worked was holding `WslService` stopped with
its start disabled, then putting it back.

## What changed

`installer/windows/compact.ps1`, in the Start menu as **Hatchling Tools →
Free up disk space**.

**As the owner,** with no administrator:

- refuses, changing nothing, while `hatchling-broker` runs a store command
  (`backup`, `restore`, `reset`, `lock`, `rekey-vault`, `migrate-vault`,
  `vault-keyring`, `room-disk`, `clean`) in the distro, or Hatchling's
  setup runs (Inno's setup or its `.tmp` copy, or `setup.ps1` for this
  distro);
- says the disk file's size and what's used inside, what will stop, and
  that a protected vault is locked after; waits for `yes`;
- stops Hatchling and runs `hatchling-broker clean`. In a distro without
  Hatchling, it says so and skips both;
- trims the distro's free space with `fstrim`, Hatchling or not (added
  after the maintainer's real run, below);
- asks Windows to run its second half as an administrator, then starts
  Hatchling again, and the process that keeps it running if Setup made
  one, and says the size before and after.

**As an administrator:**

- finds the disk file from the owner's WSL registration
  (`HKCU\...\Lxss`), never from a path it's given. It refuses none or
  two registrations, a folder that isn't a local absolute path, a folder
  or a file that's a link, and a missing file;
- records how `WslService` starts and whether it runs, sets it not to
  start, runs `wsl --shutdown`, stops it, and waits for nothing to have
  the file open;
- compacts that one file: `attach vdisk readonly`, `compact vdisk`,
  `detach vdisk`. If that fails, it detaches the file;
- in a `finally`, puts the service's start back as it was (`auto`,
  `delayed-auto`, `demand`), and starts it again if it was running. If
  that can't be done, it says the command that does it.

`-DryRun` says what it would do, with the sizes, and changes nothing.

## The threat

The second half runs as an administrator, so it must touch only what it
needs: one disk file, and WSL's service, put back. An elevated tool that
took a path could be pointed at any file; this one takes only a distro's
name, and reads the file from WSL's own registration. A link in the
registration's folder is refused, since it could lead to any disk file.
What it does to that file is a compaction, which keeps the disk's
contents. Every early return and failure goes through the `finally` that
puts the service back. The hand-breaks below prove each one.

## Results

| Check | Result |
|---|---|
| `installer/windows/tests/compact.tests.ps1` in Windows PowerShell 5.1 | 19 passed |
| Dry run on a real distro (`Ubuntu`, no Hatchling in it) | the disk file found from the registry (10.25 GB, 3.77 GB used inside), the plan said, nothing changed |
| Dry run on a distro that isn't there | refused: `WSL has no distro named no-such-distro.` |
| `Get-WslStart` on this machine | `auto`, running |
| Every installer script parses (the new CI step, run here) | no errors; a broken file gives one |

The tests run no WSL, service or diskpart: stand-ins record each call,
and the tests check the calls and their order on every path, including
diskpart failing outright.

### Hand-breaks

Each break was run against the tests, then undone. 17 broken, 17 caught:

| Break | Caught by |
|---|---|
| The service never held stopped | the success test and 5 others |
| The service's start never put back | the success test and 5 others |
| A running service not started again | the success test and the failure paths |
| A failed compaction not detached | `a failed compaction detaches the file...`, `diskpart failing outright...` |
| A file still open compacted anyway | the success tests and `a disk file still open...` |
| WSL that won't stop compacted anyway | `WSL that won't stop...` |
| A failed compaction not said | `a failed compaction detaches the file...` |
| No WslService not refused | `with no WslService, nothing is touched` |
| Two registrations taken | `no registration, or two, is refused` |
| A network folder taken | `a folder that isn't a local absolute path is refused` |
| A linked folder taken | `a folder that's a link is refused...` |
| A folder named ext4.vhdx taken as the file | `a missing folder or disk file is refused` |
| A backup not counted as busy | `the broker's store commands are busy...` |
| setup.ps1 without -Distro not counted | `Hatchling's setup is busy...` |
| Inno's .tmp copy not counted | `Hatchling's setup is busy...` |
| A delayed start put back plain | `WslService's start is read as sc.exe names it` |
| The command to put the service back not said | `a service that can't be put back is said...` |

## The maintainer's real run, and the trim it showed was missing

The maintainer ran the tool for real on `Ubuntu` (no Hatchling in it),
from an administrator PowerShell, after a dry run. Every step worked:
the plan, `yes`, the administrator half, and `WslService` put back
(`sc.exe qc WslService` after: `AUTO_START`, running, as before). But
the file went from **10.25 GB to only 10.09 GB**, with 3.77 GB in use
inside.

Why, measured rather than guessed. The disk file's own block allocation
table (read with a short script, the file untouched) showed it was at
its floor: 10,318 stored 1 MiB blocks, 10.08 GiB, and the rest of its
1,024 GiB unmapped or never written. So the compaction had done all it
could. Set against ext4's block bitmaps (`dumpe2fs`):

| The stored 1 MiB blocks | Blocks | GiB |
|---|---|---|
| Nothing in them ext4 uses | 3,150 | 3.08 |
| Full | 4,045 | 3.95 |
| Partly used | 3,123 | 3.05, of which 2.20 free |

The root is mounted with `discard`, yet 3 GiB of empty blocks stayed.
A delete discards only the file's own pieces, and the disk file ignores
a discard smaller than its 1 MiB block: a block freed a file at a time
is never given back. `fstrim -v /` discards all the free space at once.
Run on that distro, it took the stored blocks from **10,318 to 7,187
(7.02 GiB)**, 3.06 GiB unmapped against the 3.08 predicted. The 2.20 GiB
free inside partly used blocks only rewriting the filesystem would give
back. `df` also leaves out ext4's own tables, so "compacting can give
back about the rest" promised too much.

**So the tool now trims before it stops WSL:** `Initialize-Distro`
stops Hatchling, cleans its leftovers, and runs
`/usr/sbin/fstrim -v /` as root, in any distro, Hatchling or not. A
failed trim is said, and the compaction goes on. The message now says
compacting "gives back part of the rest". The steps moved out of the
owner half into that function, so the tests can play WSL. Tested at
tree `44f9c238`, that commit's:

| Check | Result |
|---|---|
| `compact.tests.ps1` | 23 passed (4 new) |
| The real `Invoke-InDistro` on `Ubuntu` | trims (`1002.4 GiB trimmed`), returns `0`; `false` and a missing program return `1`, with nothing else in the return value |
| Dry run on `Ubuntu` | the trim in the plan, nothing changed |

| Break | Caught by |
|---|---|
| No trim | `Hatchling is stopped and cleaned, then...`, and 2 others |
| The trim before Hatchling is stopped and cleaned | 4 tests |
| The trim for Hatchling's distro only | `a distro without Hatchling is only trimmed` |
| Hatchling that won't stop, gone on with | `Hatchling that won't stop...` |
| A failed trim stops the compaction | `a clean or a trim that fails is said...` |
| A failed trim not said | the same |
| A failed clean not said | the same |
| Another filesystem trimmed | 3 tests |

8 broken, 8 caught.

**The maintainer's second real run, with the trim,** on the same
`Ubuntu`, from an administrator PowerShell: the plan named the trim,
`fstrim` said `/: 1002.4 GiB (1076307734528 bytes) trimmed`, and the
file went from **10.10 GB to 7.03 GB**, against the 7.02 GiB of stored
blocks the allocation table predicted. `sc.exe qc WslService` after:
`AUTO_START`, running, as before.

## Residual

- **The administrator half has been run for real twice,** by the
  maintainer, on a distro without Hatchling. A run on Hatchling's own
  distro, which also stops and starts Hatchling, is covered by the tests'
  stand-ins, not yet by a real run.
- **A link swapped in between the check and diskpart:** the folder is
  checked, then diskpart opens the path. Something running as the owner
  could swap the folder for a link in between. Compacting keeps a disk's
  contents, so the most it could do is compact another disk file.
- **The script lives in the owner's apps folder,** so anything running as
  the owner could change it before it's run as an administrator. That's
  true of every script an owner elevates, and Windows' prompt names the
  program it runs.
- **Run with another account's administrator** (a standard account), the
  administrator half reads that account's registry, finds no distro, and
  says so: compacting needs administrator rights in the account that has
  the distro.
