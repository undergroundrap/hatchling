# Evidence: a real install, rebooted, with a key traced and the Room's bytes kept (WO-033, WO-034)

What was tested on a real install, how, and what came back, for the open
items of [WO-033](../workorders/workorder-033-install-it-and-leave-it-on.md)
and [WO-034](../workorders/workorder-034-run-commands-in-the-agents-encrypted-room.md)
that only an install can prove: surviving a reboot, a hosted key leaving
no plaintext copy, and the Room's workspace kept across restarts, with
its scratch discarded.

| | |
|---|---|
| Build | the release workflow's run for `cb82fea9` (#563), merged as `5ffee395`, tree `5a12f97a`: `main`'s tree at `3a5c4445` |
| Date | 2026-11-21 (UTC) |
| Machine | x86_64, Windows 11 Home, WSL 2, kernel 6.6.87.2 |
| Distro | `hatchling-drill`, Ubuntu 24.04.5: a copy of a prepared distro (systemd, `cryptsetup-bin`, the `owner` account) with its earlier Hatchling removed, deleted after |
| Installed with | `installer/windows/setup.ps1 -BundledRoom -Unattended`, the script Windows Setup runs |
| Model | a scripted stand-in on `localhost:11434`, a systemd unit of its own: it answers a fixed line, or asks for the agent's `run` tool and repeats its result word for word. Nothing here is a model |

This record changes no code.

## The release files

The workflow's `build-x86_64-unknown-linux-gnu-a`, `ui-a` and
`room-image-verified` artifacts, laid out as its *Gather what the setup
holds* step lays them out: the four programs, the app, the installer's
two files (`scripts/release-installer.sh <dir> v0.9.0`, from the same
tree) and the Room's five files. All 12 files' SHA-256 sums were
identical to the `SHA256SUMS` that run printed. `hatchling-broker
--version` on the install said `0.9.0`, commit `5ffee395`.

Setup ended with `doctor`'s *Nothing found wrong.*, *installing the
verified Room image*, and *Hatchling is running, and starts at boot.*

## A restart and a reboot

The same checks were run after the install, after `systemctl restart
hatchling`, and after a reboot of the distro: `wsl --terminate
hatchling-drill`, then started again, so systemd booted from nothing as
at a machine's start (PID 1 started anew at 08:06:52 UTC).

| | After the install | After a service restart | After the reboot |
|---|---|---|---|
| `hatchling.service` | active, enabled | active, a new invocation | active, a new invocation, started at boot |
| `hatchling-host@owner.service` | active, enabled | active | active, a new invocation, started at boot |
| `doctor` | exit 0, *Nothing found wrong.* | exit 0 | exit 0 |
| The app's address (`hatchling-host url`, its token not shown) | HTTP 200 | HTTP 200 | HTTP 200 |
| The agent, over the local API | answered | answered | answered |

The one note `doctor` gave each time is the vault opening by its key file,
as an install without a security key does.

## The Room's workspace kept, its scratch discarded

Through the agent's `run` tool, so through the real broker, spawner,
keeper and the encrypted room disk (`/dev/mapper/hatchling-room-…` on
`/workspace`, ext4 inside dm-crypt), from the release's verified image:

1. A 1 MiB random file was given to the agent over the local API, as the
   app gives one: placed as `0001-payload.bin` in its inbox.
2. In the Room: `sha256sum inbox/0001-payload.bin` said `f40a65fc…`, the
   file's own sum; a 64 KiB random file `made-in-room.bin` was written to
   the workspace (`c71e224e…`), and a 4 KiB one to the session's scratch,
   `/session` (tmpfs).
3. After the service restart, and again after the reboot, the Room said:

| | Before | After the restart | After the reboot |
|---|---|---|---|
| `inbox/0001-payload.bin` | `f40a65fc438d0dc6…` | the same | the same |
| `made-in-room.bin` | `c71e224e748e481a…` | the same | the same |
| The scratch file in `/session` | there | gone: a new session's `/session` | gone (a second one, written before the reboot) |

## The hosted key: what was written while it was sealed

A stand-in key (72 characters, random, made in the shell that piped it,
never in a file) was piped to `hatchling-broker secret add --for
openrouter.ai:443 --replace`. The distro has no `strace`, so the kernel
recorded the calls: kprobes on `do_sys_openat2` (every open to write,
read-write, create, truncate or as a nameless file), `do_renameat2` and
`do_linkat`, with their paths, for every process. None were lost.

Everything opened to write, renamed or linked while the key was sealed:

| Process | What |
|---|---|
| `hatchling-broker` | opened `vault/.egress-openrouter-ai-443.429.0.tmp` (create, exclusive, write) |
| `hatchling-broker` | renamed it onto `vault/egress-openrouter-ai-443` (116 bytes, sealed) |
| `hatchling-broker` | appended to `schedules.activity` (the activity log, which names the secret) |
| `hatchling-broker` | opened the drill's own file for its output (`Read 72 characters.`, `Replaced egress-openrouter-ai-443 in the vault.`) |

Then:

- **Every file of the distro's own filesystems** (its root, every tmpfs,
  and the open room disk; not WSL's share of Windows' drivers, nor the
  root a second time under `/mnt/wslg`): 37,024 files, none holding the
  key. The same search found a control marker written to one file on the
  root and one on a tmpfs, so it finds what is there.
- Not in the activity log, nor in the sealed file. Not in the journal's
  messages. Not in what `secret add` said.
- Asked again with `--if-missing`, `secret add` said it's in the vault
  already.

The key was piped, as CI pipes one. The prompt with echo off, which a
person typing gets, reads the same line by another route; it wasn't run
here.

## The Room's namespaces, live

In the same Room, its own namespaces against the host's (PID 1's) and the
broker's:

| | Host and broker | The Room |
|---|---|---|
| net | `4026531840` | `4026532307` |
| ipc | `4026532206` | `4026532305` |
| pid | `4026532212` | `4026532306` |
| mnt | `4026532209` | `4026532368` |
| uts, user, cgroup | | the host's |

The Room has no `/sys/class/net` and no `/proc/net`, and `ipcs` was
killed by its syscall filter (*Bad system call*). Its user namespace is
the host's by design: it runs as `hatchling-room`, an account of its own.

## Closing the room disk while a job runs

`lock` refused on this install: a vault its key file opens would come
straight back, so only a vault a security key protects can be locked.
The step of `lock` that touches running work is closing the room disk,
and `hatchling-broker room-disk close` was run six seconds into a 30
second job in the Room:

- it refused: *the room disk is in use by another process*, and closed
  nothing;
- the job finished on time and wrote its last line (`started`, then
  `finished`).

The close was run while the agent's turn still waited on the job: a
program still running when its turn ends is stopped, as the `run` tool
is designed, and a control job left running past its turn was stopped
with no close attempted.

## Residual

- The reboot is the distro's own, which is what a Windows restart does
  to it; a physical machine's reboot wasn't done. CI's `service` job
  proves the one-command install on Ubuntu 24.04 and a service restart.
- `lock` with a job running, which needs a security key, wasn't run.
- The scripts and outputs are with the maintainer's notes; the drill's
  distro was deleted after.
