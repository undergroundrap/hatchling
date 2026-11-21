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
- runs `hatchling-broker clean` and stops Hatchling. In a distro without
  Hatchling, it says so and skips both;
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

## Residual

- **The administrator half hasn't been run for real here.** It changes a
  Windows service's start, which this record's runs don't do on the
  maintainer's machine unasked. Its logic is covered by the tests above,
  and the real run is the maintainer's: a dry run, then a run on a
  distro of their choosing, reading the size before and after, and
  `sc.exe qc WslService` before and after.
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
