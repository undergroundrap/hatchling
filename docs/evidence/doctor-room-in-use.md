# Evidence: doctor doesn't call a room disk in use a problem

What was tested, how, and what came back, for a change to what `doctor`
says of a room disk that a running Room has. Every number here is read
from a file in the results bundle, which is attached to the release that
carries this change.

| | |
|---|---|
| Base commit | `8079e410` (`feat(agent): run a program in the Room from a chat (#431)`), with the two changes before this one (the image proof's `/proc`, the mount test's wait) and their records |
| Tested tree | `c06309eeb9026c2114e1b6015445712fe727a68e` |
| Date | 2026-11-13 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-doctor-room-in-use-results.tar.gz`, 247096 bytes, sha256 `1fa04654c5fc56c80b4b0cb9e2e04a0b653767cfff1aa38feb2b5d1a2e01183a` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

It was found by the acceptance run of the Room's run tool, on a real
install. After the agent had used its computer, `doctor` run as root
said:

    PROBLEM  room disk      the room disk can't be used: the room disk is in use by another process; stop Hatchling and retry maintenance
                            fix: Check HATCHLING_ROOM_DISK_DIR names a directory of root's alone, apart from Hatchling's other state.

and counted one problem. Nothing was wrong. A Room has its disk's claim
for as long as it runs, so that nothing changes the disk under it, and
`doctor`'s look at the disk asks for that claim. The look was refused,
as it should be, and `doctor` read the refusal as a broken disk and gave
a fix for something else.

The check is older than the run tool. Until the agent could run programs
nothing on an install kept a Room running, so nothing showed it.

## What changed

- `crates/hatchling-broker/src/room_disk.rs`: the opening words of that
  refusal are one constant, `IN_USE`, which the claim writes.
- `crates/hatchling-broker/src/doctor.rs`: a refusal that starts with
  those words is a room disk in use. With the vault unlocked that is
  fine: `doctor` says it is in use, by the agent's computer or by
  maintenance, and that it isn't looked at, and a report with it says
  nothing's wrong. With the vault locked nothing should have the disk,
  so in use is a problem there, with what to do: end any
  `hatchling-broker` command still running, or restart Hatchling. Any
  other refusal is a room disk that can't be used, as before.
- The changelog says it.

What `doctor` doesn't do is look at a disk a Room has. The claim is what
keeps a trusted process from changing the disk under a live Room, and it
is left as it is: `doctor` waits its turn like any other.

## The tests

- `doctor::tests::a_room_disk_in_use_is_fine_unless_the_vault_is_locked`:
  the words said of a disk in use, that a report with it counts no
  problem, that with the vault locked it is a problem with its fix, and
  that a refusal which only goes on to say "in use" is still a disk that
  can't be used.
- `room_disk::tests::room_operations_share_the_kernel_claim`: a look at
  a claimed disk is refused in words that start with the constant.
- `the_installed_spawner_hands_the_verified_image_to_its_mount_helper`,
  as root with a real Room: a look at the disk under a live Room is
  refused in those words.

## Each part taken out by hand

Four breaks. The broker's library tests passed unbroken before and after
(1395 passed, 0 failed). All four were caught by the test named for
each, and each failure was read.

| Break | Test that fails |
|---|---|
| A disk in use is read as a problem again | `a_room_disk_in_use_is_fine_unless_the_vault_is_locked` |
| Any refusal that says "in use" anywhere is fine | `a_room_disk_in_use_is_fine_unless_the_vault_is_locked` |
| In use is fine with the vault locked too | `a_room_disk_in_use_is_fine_unless_the_vault_is_locked` |
| The claim refuses in other words | `room_operations_share_the_kernel_claim` |

`unit-breaks.log` sha256
`22d8c88b2085f7bb2c795fc0c2ed4b5a5058ef1dfc9e6744ec3b9c8fc672089d`.

## Mutation

`cargo mutants --in-place --in-diff <the broker's lines> --timeout 90 -- --lib`,
after an unmutated baseline.

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-broker` | 5 | 3 | 0 | 0 | 2 |

Unviable, one line each:

- `crates/hatchling-broker/src/doctor.rs:569:5: replace room_said -> Finding with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/room_disk.rs:343:9: replace RoomDisk::claim_operation -> Result<Option<hatchling_sys::HeldLock>, String> with Ok(Some(Default::default()))`: unviable, build failure.

`mut-broker-outcomes.json` sha256
`1b4d33d8ddbf2acddf45fdd313adae646e5fa3e3bcd7801b18db1a0fdc425e9d`.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3781 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3781 | 0 | 0 |

`gate.log` sha256
`d1475c6ed8b83c2cfd2ddf15fdfafb70233afc069ea46f138c73e475c894ea4b`.

The first tree of this change failed one broker test before any break
was made: the check that a refusal starts with the constant had been put
on refusals that maintenance wraps in words of its own. It now checks
the look alone, which is what `doctor` reads. Every run in this record
is of the tree after that.

## With a real Room, as root

Both installed Room tests, run as root outside the sandbox's jail, with
the static supervisor built for the run tool's change (sha256
`e1a9be0e3d8f32f0bae3112a8614d7648074b68e356bdaf1f25d486e33d2e42d`): no
source of the supervisor has changed since.

| Run | Result |
|---|---|
| Unbroken, before | 2 passed, 0 failed |
| The claim refuses in other words | 1 failed: the look under a live Room said "stop Hatchling and retry maintenance: the room disk is in use by another process", which doesn't start with the constant |
| Unbroken, after | 2 passed, 0 failed |

Nothing was left mapped after any run. `root/installed-unbroken-before.log`
sha256 `3ad1b7fdfe7214eb70a5129da1cca7395df4e3d5909d8dfed40f09bd38d829d6`;
`root/installed-break.log` sha256
`ac1a600bc981b8c77951f365a9d8de5fce5e21a1ee4b3a1de50af84995d4ab13`;
`root/installed-unbroken-after.log` sha256
`02a2a27fa999ae77df043c188fb31c0db0c3ce8945cb3e65f75ee08c9b7f70a2`.

## What this change does not do

- **Run `doctor` on a real install again.** The tests hold each part:
  what a live Room's claim says to a look, and what `doctor` says of
  that. The whole of it, `doctor` typed on an install whose agent has
  used its computer, is to be seen at the next install drill.
- **Tell the agent's computer from maintenance.** Both have the same
  claim, and `doctor` says it is one or the other.
- **Show the disk's numbers while a Room has it.** The app has them
  from the spawner, which answers from inside the Room's own hold on
  the disk. `doctor` is a process of its own and has no such way in.
