# Evidence: the Room's file tools run under its syscall profile

What was tested, how, and what came back, for a change to the Room's
syscall profile: eight calls its image's own file tools make are allowed
in the Room alone, and one they make and aren't granted is answered with
an error. Every number here is read from a file in the results bundle,
which is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `310aea70` (`test(room): settle the room disk's stand-ins before they are used (#426)`) |
| Tested tree | `3a237bfc9e1ccf39d5b7bc1a4ed5a4fd30773e07` |
| Date | 2026-11-12 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Tools run | GNU coreutils 9.7, tar 1.35, findutils 4.10.0, grep 3.12, sed 4.9, diffutils 3.12, gzip 1.14, gawk 5.3.2, dash 0.5.12 |
| Results bundle | `hatchling-room-filter-image-tools-results.tar.gz`, 204026 bytes, sha256 `d0d8840bc31be677d57492e9d6580c8148600b85dd97678d4fae6edfc331bcb8` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

The Room's syscall profile is the CLI cells' profile, narrowed. It was
never tried with the programs the Room's image is made of. The first
proof that ran one, a job that ran `cat`, was killed: GNU `cat` asks the
kernel for `fadvise64`, a call the profile didn't know, and a call
outside the profile ends the process.

Fifty commands were then traced at ordinary work (the GNU file tools,
and `python3`, `git`, `sh` and `bash` once or twice each) and their
calls compared with the profile. Nine calls were outside it:

| Call | Asked by |
|---|---|
| `fadvise64` | `cat`, `cp`, `cut`, `install`, `md5sum`, `sha256sum`, `shuf`, `sort`, `split`, `tee`, `tr`, `uniq`, `wc` |
| `fchdir` | `find`, `mkdir -p` |
| `creat` | `tar`, making an archive |
| `fstatfs` | `du`, `find` |
| `symlinkat` | `ln -s` |
| `dup3` | `shuf`, `uniq` |
| `openat2` | `tar`, unpacking |
| `llistxattr` | `ls -l` |
| `fsetxattr` | `install` |

## What changed

In `crates/hatchling-sys/src/seccomp.rs`, for the Room's profile only:

- **Eight calls are allowed.** Each is an operation the profile already
  allowed, asked another way (`fchdir` for `chdir`, `creat` for `open`,
  `fstatfs` for `statfs`, `symlinkat` for `symlink`, `dup3` for `dup2`,
  `openat2` for `openat`, `llistxattr` for `listxattr`), or a hint that
  changes nothing (`fadvise64`). The source says of each what it can and
  can't do and which tool asked.
- **`fsetxattr` is answered `ENOTSUP`, and not run.** No call that sets
  an extended attribute is allowed. `install` asks this one of every
  file it copies, and goes on when told the file system doesn't support
  it.
- **The rule for that answer is written down.** A call an ordinary tool
  makes in ordinary use, and that isn't granted, is answered with an
  error the tool already handles. Every other call outside the profile
  still kills the process. A call is answered only once a tool was seen
  making it and seen going on without it.

The agent, harness, CLI and browser profiles are unchanged.
`docs/arch/trust-boundary.md` and the changelog say the new behaviour.

## The tests

- `room_allows_what_its_tools_ask_and_answers_what_it_does_not_grant`:
  each of the eight is allowed in the Room; `fsetxattr` is answered
  `ENOTSUP`; `setxattr`, `lsetxattr`, `removexattr`, `lremovexattr` and
  `fremovexattr` still kill; each name is the kernel's number for it;
  and no other profile allows any of the nine (the browser's has
  `fstatfs` by its own list).
- `the_rooms_tools_run_under_its_profile`: in a fresh process, 46
  commands of the tools are run as they are, then the kernel is given
  the Room's profile and the same 46 are run again. Each must succeed.
  Where a machine keeps the GNU tools under names of their own (`gnucat`)
  those are the ones run.
- `room_refuses_network_listeners_and_anonymous_executable_files`, which
  was there, now says the whole of what the Room allows: the CLI
  profile's calls, less what the Room refuses, and these eight.

## Each part taken out by hand

Each part of the change was taken out in turn and the profile's tests
run. The unbroken tests passed before and after (27 of 27). All 13
breaks were caught, and each failure was read.

| Part taken out | What the tool test said |
|---|---|
| `fadvise64` not allowed | `cat` killed (SIGSYS) |
| `fchdir` not allowed | `mkdir -p` killed |
| `creat` not allowed | `tar -cf` killed |
| `fstatfs` not allowed | `du -s` killed |
| `symlinkat` not allowed | `ln -s` killed |
| `dup3` not allowed | `uniq` killed |
| `openat2` not allowed | `tar -xf` killed |
| `llistxattr` not allowed | `ls -la` killed |
| `fsetxattr` not answered | `install` killed |
| `fsetxattr` answered `EPERM` | `install` exits 1: "setting permissions ... Operation not permitted" |
| `fsetxattr` allowed | not a tool's matter: the unit test and the test of what the Room allows fail |
| The eight given to every CLI cell | the unit test fails ("harness CLI: fadvise64"), with the test of what the Room allows and the browser profile's own test |
| `fchdir` given the number of `chdir` | the unit test fails (80, not 81), and `mkdir -p` is killed |

For each of the first ten the unit test failed too, and for the eight
allows so did the test of what the Room allows. The `EPERM` row is why
the answer is `ENOTSUP` and no other error.

`unit-breaks.log` sha256
`42b70b5007b2e568f01df5c05bc2f2374cee04e5822a98264dd677e3c8c15934`.

## Mutation

`cargo mutants --in-place --in-diff ff-slice-sys.diff --timeout 60 -- --lib`,
on the lines changed in `crates/hatchling-sys`, after an unmutated
baseline.

| Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|
| 1 | 1 | 0 | 0 | 0 |

The one mutant replaces the Room's profile with an empty one. The change
is two lists and two loops over them, which the tool has no mutant for;
the hand-breaks above are what tries them. `mut-sys-outcomes.json` sha256
`4fdbde33e210c53e279eecbb9893fc10c6c2a7af5dca2a7aec510bc9799e7b3c`.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3752 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3752 | 0 | 0 |

`gate.log` sha256
`767df05f8c921383a10b6c0612ceb7451a52f76728c0043e41ba7d354d389b51`.

The `hatchling-sys` library tests were then run 25 times over on 1.98.1,
every test in parallel: 292 passed and 0 failed in each. `stress.log`
sha256 `6fd0523b204a1fec2bc2ea875973adff96031fd935760c85fea4c59c41af341e`.

## As root, and in a real Room

CI runs the workspace's tests as root as well, so the profile's tests
were run as root, outside the sandbox: 27 passed, 0 failed
(`root/tools-as-root.log`, sha256
`e028709749df1d7a04fbf4d8bb6259b9bc448d7247292fd4165198f383b619de`).

The first run as root failed, and the test was changed for it twice:

- **Root's `tar` asks for more.** Unpacking, it gives each file its
  owner and mode back (`fchownat`, `fchmodat2`), and was killed. The
  Room's jobs run as the Room's own account and never as root: its
  launcher gives root up before the supervisor starts. So the test, run
  by root, now gives root up first as well. Neither call is granted.
- **A user the machine's files don't name.** Dropped to a user with no
  account entry, `ls -l` was killed: asked whose files those are, the
  machine's systemd user database is tried, and that asks for
  `timerfd_create`. That is the machine's doing and no tool's, so the
  test drops to `nobody`, which every machine's own files name.

The supervisor was built static from this tree with the repository's
script (sha256
`66ac9da79e21f827da4cb0956fac40c32d00237f2b2ad71448fbf61231cb1641`); its
17 tests and its three proofs under the Room's profile pass. Both
installed Room tests were then run as root with it, under the Room's
cgroup and this profile: 2 passed, 0 failed, and nothing was left mapped
(`root/installed.log`, sha256
`71e5b7ba7d4d71f26504e4527a7657779d75929ee3c945e3146780e06fdca109`).
Those tests run fixture programs in the Room, not the image's tools.

## Another machine's tools

The sandbox's default tools aren't GNU's. With the one line that prefers
the GNU tools turned off by hand, the same 46 commands of the default
tools also pass under the profile (`other-tools.log`, sha256
`84af4b42e89c09f8dacfdb3ffa012b61a6b93daa3bb8d65555c8313e58500316`).

## What this change does not do

- **Decide what the tools' other options ask for.** A wider trace, of 73
  everyday commands, found calls outside the profile that this change
  doesn't decide. Each still kills (`scoping/scope2.txt`,
  `scoping/verdicts.txt`):

  | Call | Asked by |
  |---|---|
  | `link` | `git add`, `git commit`, `git clone`, `git stash`, `git gc`, Python |
  | `linkat` | `ln` (a hard link) |
  | `flistxattr` | `cp -a`, `cp -p`, `mv` between file systems |
  | `fgetxattr` | `sed -i` |
  | `fchownat` | `chown`, `cp -a`, `mv` of a directory between file systems |
  | `fchmodat2` | `chmod -R`, `tar` unpacking a directory inside another |
  | `rt_sigsuspend` | `wait` in `sh`, `timeout`, `tail -f` |
  | `timer_create`, `timer_settime` | `timeout`, `tail -f` |
  | `inotify_init` | `tail -f` |
  | `getpriority`, `setpriority` | `nice` |
  | `sync` | `sync` |
  | `sendmsg`, `recvmsg`, `accept4` | Python's `multiprocessing` |
  | `setfsuid`, `setfsgid` | `less` |
  | `landlock_create_ruleset`, `landlock_restrict_self` | `xz` |

  Python's `multiprocessing` also asks to `bind` and `listen`, which the
  Room refuses by design. Each of these calls is a decision of its own,
  and none is made here.
- **Trace the Room's image itself.** The tools traced are the sandbox's
  GNU ones and not the image's own files, and their versions weren't
  compared with the image's. The image's `awk` may be another than the
  one traced. Node isn't on this machine and wasn't traced. Whether the
  image's files name the Room's account, and so what `ls -l` asks there,
  wasn't looked at.
- **Give the agent a tool.** Nothing in the app or the agent reaches the
  Room yet.
