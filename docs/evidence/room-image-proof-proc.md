# Evidence: the image proof has the Room's /proc

What was tested, how, and what came back, for a change to a proof and
its script only: the proof that runs the Room image's programs under the
Room's profile now gives them the `/proc` a Room has. Every number here
is read from a file in the results bundle, which is attached to the
release that carries this change.

| | |
|---|---|
| Base commit | `8079e410` (`feat(agent): run a program in the Room from a chat (#431)`) |
| Tested tree | `223e76d0cdcb5cb2009bf34fec8e1f805507be0e` |
| Date | 2026-11-13 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Room image | Built by `scripts/room-image.sh` from Debian snapshot `20261107T000000Z`, sha256 `2fd340c93b99e146b38b995eec585cd72a76672b8c75f30137c3884cc5e2d927` |
| Results bundle | `hatchling-room-image-proof-proc-results.tar.gz`, 175517 bytes, sha256 `d61bf756e8665eb51ba607c6aa69bb1d7ad95b9225a2e6512fd4ee5ad47e8d72` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

`scripts/room-image-tools-proof.sh` lays out a root as a Room's is and
runs 127 commands of the image's programs in it under the Room's
profile. It mounted a whole `/proc` there. A Room's `/proc` is mounted
with `subset=pid`: it shows processes, and nothing else of the machine.
There is no `/proc/meminfo`, no `/proc/cpuinfo` and no `/proc/sys` in a
Room.

So the proof could pass a program that a Room fails: one that reads the
machine's memory, its processors or the kernel's settings from `/proc`
found them in the proof, and finds nothing in a Room.

## What changed

- `scripts/room-image-tools-proof.sh` mounts `/proc` with `subset=pid`,
  as the Room's mounts do.
- `crates/hatchling-sys/tests/room_image_tools.rs`, the proof, refuses a
  root whose `/proc` isn't mounted, or has `sys` in it. A whole `/proc`
  has the kernel's settings there, and one of processes only doesn't, so
  the script can't go back to a whole one and still pass.

No product code is changed, and the 127 commands are the same.

## What the proof found

Nothing. All 127 commands end as they must with the Room's `/proc`: 124
end well and three are refused and say so, as before. None of the
image's programs, at this work, needed more of `/proc` than a Room has.

Run as root, outside the sandbox's jail, with the repository's script on
the image above:

| Run | Exit | What it said |
|---|---|---|
| Unbroken, before the breaks | 0 | "127 commands of the Room image's programs ended as they must under the Room's profile" |
| Unbroken, after the breaks | 0 | the same |

Nothing was left mounted or mapped after any run.

## Each defence taken out by hand

Two breaks of the script, each run through the whole proof and its
failure read.

| Break | Exit | How the proof failed |
|---|---|---|
| The script mounts a whole `/proc`, as it did | 101 | "the root's /proc has the kernel's settings (`sys`), so it is a whole one: a Room's is mounted with subset=pid" |
| The script mounts no `/proc` | 101 | "the root named has no /proc mounted" |

`root/unbroken-before.log` and `root/unbroken-after.log` sha256
`258ee8ea9e8e1451dac8d27a05787dc2f04d38341efac792234c76599abeeca6`;
`root/break-whole-proc.log` sha256
`84e797cb26cc3d2ba8b0cc70825ce8db79abc4a8327babf09b59a03fab9dba38`;
`root/break-no-proc.log` sha256
`54645fc4d21af565fa940af0dfcdce363620c480a536fe5da2005546bc3fcc26`.

## Mutation

None to run. The change is to a test and a script, and
`cargo mutants --list --in-diff` on the changed lines says so: "No
mutants to filter" (`gate-mutants-sys.txt`).

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3780 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3780 | 0 | 0 |

`gate.log` sha256
`afd41c184b79fa176bd15b08899d7fb3434542b7229223e31fcb344a23cbaa68`.

Without root and with no root named, the proof says it is skipped; the
runs that count are the ones above.

## What this change does not do

- **Give the proof a process namespace of its own.** A Room's `/proc`
  shows the Room's processes; the proof's shows this machine's. What a
  program can read of them is the same.
- **Change what the release's checks run.** They already run this script
  on the image they build, on every pull request that touches the
  profile, and now run it with the Room's `/proc`.
