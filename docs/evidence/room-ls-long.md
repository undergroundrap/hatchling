# Evidence: `ls -l` goes on in the agent's computer

What was tested, how, and what came back, for a change to the Room's
syscall filter: two calls that ended `ls -l` are now answered. Every
number here is read from a file in the results bundle, which is attached
to the release that carries this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `90b2f0c2` (`feat(agent): read a file in the Room from a chat (#438)`) |
| Tested tree | `92177322cdd001c86785c29012100d2b9b74bed2` |
| Tree before the fix | `a9fa0de78bb65635c21d9b6726e58f5df6ca5591`: the base, with only the check's new commands |
| Room image | main's, from the base commit's dry run of the release workflow: `room-system-x86_64.squashfs` sha256 `88d19fcf016a95273bc71a7680f0cf5d99a1505e3822bc99a9f73d5689bc25f9` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-room-ls-long-results.tar.gz`, 179639 bytes, sha256 `ff2875318e1c8ef428832ed3578648cb3a2dd1d557549f4c643f814acc30d791` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

On a real install updated to the base commit, `ls -la /workspace` in the
agent's computer printed nothing. The kernel's log there had eleven lines
for `ls`, as the Room's user, ended by the Room's syscall filter on
syscall 192, `lgetxattr` (the update drill's record and bundle have
them). The agent is told a command it ran ended, and what it printed.
Told `ls` printed nothing, a model said a workspace with five files in it
was empty, twice in eleven tasks, and a third task ran out of steps
asking again.

`ls -l` asks which attributes each entry has (`llistxattr`, which the
Room allows). The image's file system answers that it can't say. `ls`
then asks for the entry's security label by name, with `lgetxattr`, or
with `getxattr` when `-L` has it follow links. Neither call was in the
Room's lists, and a call outside them ends the process. With `-a`, `..`
of `/workspace` is the image's root, so the most ordinary listing there
is asked this way.

The check that runs the image's programs under the filter
(`room_image_tools`) had `ls -la .` and `ls -lR .`, both inside the
workspace, whose file system can list attributes.

## What changed

- `getxattr` and `lgetxattr` are answered in the Room with `ENOTSUP`, as
  `fgetxattr` already was. Reading an attribute's value is still not
  granted there: nothing is allowed that wasn't. They are in the Room's
  list alone, and no other profile has them.
- The filter's table knows the two calls' numbers, 191 and 192.
- The check has nine more commands: `ls -la` of the workspace, the root,
  `/tmp` and the session; `ls -ld` of all four; `ls -l` of files of the
  image; `ls -lL /`; `cp -a` of a file out of the image; `stat` of the
  root and `/tmp`.

## The image's programs, under the filter

As root, by `scripts/room-image-tools-proof.sh`, with main's image:
`root/` in the bundle.

| Tree | Result | What the kernel recorded |
|---|---|---|
| Before the fix | 7 of 136 commands ended by the filter: the four `ls -la`, `ls -ld`, `ls -l` of the image's files, `ls -lL /` | `ls` ended 6 times on syscall 192 and once on 191 |
| Tested tree | 136 of 136 ended as they must | nothing ended |
| Tested tree, the two calls answered `EPERM` by hand | 136 of 136 | nothing ended |

`cp -a` out of the image and `stat` ended well before the fix too: they
are in the check so that they stay so.

The last row says `ls` goes on from "not permitted" as well. `ENOTSUP`
is kept because it is what a file system with no attributes says, and
what `fgetxattr` is answered with; the filter's own test pins it.

The first run on the fix failed as it started, with "UnknownSyscall
getxattr": the filter's table of names didn't have the two calls. The
numbers were added, and the tested tree is the tree after that.

## Breaks by hand

Nine, each one part of the change taken out or bent, the filter's tests
run on each, and each failure read (`unit-breaks.log`, `unit-*.log`).
All nine were caught, and the tests passed before and after.

| Break | Caught by |
|---|---|
| `lgetxattr` not answered | `room_allows_what_its_tools_ask_and_answers_what_it_does_not_grant`: the call ends the process |
| `lgetxattr` answered `EPERM` | `the_rooms_lists_are_what_was_decided_by_the_kernels_numbers` |
| `lgetxattr` allowed | `room_allows_…` and `room_refuses_network_listeners_and_anonymous_executable_files`: the Room would allow a call the CLI profile doesn't |
| `lgetxattr` given another call's number | `the_rooms_lists_are_what_was_decided_…` |
| `getxattr` not answered | `room_allows_…` |
| `getxattr` answered `EPERM` | `the_rooms_lists_are_what_was_decided_…` |
| `getxattr` allowed | `room_allows_…` and `room_refuses_network_listeners_…` |
| `getxattr` given another call's number | `the_rooms_lists_are_what_was_decided_…` |
| The two calls given to every CLI cell | `no_other_profile_has_what_the_room_allows_or_answers` and `room_refuses_network_listeners_…` |

## Mutation

`cargo mutants --list --in-diff` on the change to `hatchling-sys`'s
source found no mutants (`gate-mutants-sys.txt`): the change is entries
in two tables, which it has nothing to alter in.

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3804 passed, 0 failed | exit 0: 135 binaries, 3804 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The filter's tests were run as root too, as CI runs them: 29 passed
(`root/as-root.log`).

## What wasn't tried

- The fixed build on the install the fault was found on. Installing it
  restarts that install, and its vault opens only with its owner's key.
  The two tasks that were answered wrongly there are to be asked again
  when it has been.
- aarch64: the Room's profile is x86_64's alone.
- Other programs that might ask for an attribute's value by name. The
  check runs 136 commands; one not among them that asks another way
  would still be ended, and the kernel's log would name it.
