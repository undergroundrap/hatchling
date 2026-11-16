# Evidence: a file put in the Room's inbox, by root (WO-041, slice 1)

What was tested, how, and what came back, for the first slice of files
between the owner and the agent's computer. The spawner puts a file in the
Room's inbox as root, the Room's user can read it and change nothing of it,
and the inbox's name, numbers and bounds are root's to keep. Nothing in the
app reaches it yet: that is the next slice. Every number here is read from
a file in the results bundle, which is attached to the release that
carries this change.

| | |
|---|---|
| Base commit | `1f553635` (`feat(dev): keep the docs and the work-order queue honest in CI (#489)`) |
| Tested tree | `e839f9419d5d0edc169150e9dd0b9acd05a5a517` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 threads, in a sandbox; the installed Room's tests as root outside it |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned); cargo-mutants 27.1.0 |
| Room supervisor | built from the tested tree by `scripts/room-image-supervisor.sh`, sha256 `3c4044d2c5e0d1db6b220f4eae8c82863175ded8baa77a80a5a93b6058579617` |
| Results bundle | `hatchling-room-inbox-put-results.tar.gz`, 2691388 bytes, sha256 `6fa0116bcdbe6854fc2cbbf2def0e5c3ca3f6b301ba34fceb4c4f5b1215a2af8` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## Before any code

An independent threat model of the work order and the code came first. Its
design is in [WO-041's Design section](../workorders/workorder-041-files-between-you-and-the-agents-computer.md#design).
It found that the inbox, mounted read-only in the Room since WO-034, was
root's `0700`: the Room's user couldn't have read a file put there. Every
open of the disk now leaves it `0755`.

A second independent review of this slice's diff found nothing serious
and seven smaller things. Each was changed before the runs below:

- a number given again once its file was removed;
- a full disk said as a failure, not plainly;
- a failed directory sync that left the file;
- a put never answered taken for none;
- a shared temporary name;
- guards no test exercised;
- a put into an inbox not yet readable.

## Runs

**Quick** (`quick.log`): `cargo fmt --check`, then on each toolchain
`cargo clippy -p hatchling-sys -p hatchling-broker --all-targets
--all-features -- -D warnings`. Every step exited 0. The inbox's own tests
passed (13), and so did the broker's Room session, spawner and room disk
tests (174).

**The installed Room, as root** (`root/final.log`): on a real encrypted
room disk, with the supervisor above.

| Test | Result |
|---|---|
| `the_spawner_prepares_layout_only_on_the_verified_room_disk` | ok |
| `the_installed_spawner_hands_the_verified_image_to_its_mount_helper` | ok |
| `an_installed_room_ends_when_its_spawner_is_killed` | ok |

What the first two now hold, beyond what they held before:

- The volume's inbox is root's and `0755`. An inbox made `0700` before
  this is made so as the disk opens.
- **A put with no Room live** is root's, `0444`, numbered `0001-`, whole,
  and alone in the inbox.
  - A link planted where the inbox keeps its numbers isn't written
    through: the put is refused, and no number is spent.
  - No number is given twice, even after the highest file is removed.
  - A stem that isn't one is refused.
  - A put is refused while another process holds the disk's claim; into
    an inbox root doesn't own; and across a tmpfs mounted over the inbox,
    which stays empty.
  - A link in place of `workspace/inbox` isn't written through.
  - A disk that isn't open takes no file.
- **A put while a Room holds the disk** is done by its session. A job in the
  Room reads the file at `/workspace/inbox/`. As that job it can't open it
  to write, remove, rename or chmod it, nor make a file beside it. Through
  the supervisor, nothing is written in the inbox or over the file. The
  Room goes on.

**Hand-breaks** (`unit-breaks.log`, `root/final-break-*.log`): 29 breaks,
each taking out or bending one guard. Every test named for a break failed.

- **23 in the jail**: 23 broken, 0 survived; the tests passed before and
  after.
- **6 as root**: the inbox left unreadable as the disk opens, a put that
  takes no claim, an inbox opened across a mount, and the three
  room-disk functions the jail's mutation run can't reach, each made to
  put nothing. Each failed the test named for it.

The breaks are in `ib_break.py` in the bundle.

**Mutation**, on each crate's changed lines:
`cargo mutants --in-place --in-diff ib-slice-<crate>.diff --timeout <n> -o <out> -- --lib`.

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-sys` (timeout 60) | 67 | 63 | 0 | 0 | 4 |
| `hatchling-broker` (timeout 240) | 48 | 34 | 6 | 0 | 8 |

- `mut-sys-outcomes.json` sha256 `8eddcafa836eb8d000ee947a4b52b186e61c6288ed17f455925273324c822c16`.
- `mut-broker-outcomes.json` sha256 `8faf34b04a4f8935f2bc83c4898a7e75de0d4ccbdc562eb3424bd8eaebdb28e7`.

The broker's run was on tree `468ca60d`. It differs from the tested tree
only by 13 lines of a test in `crates/hatchling-sys/src/room_inbox.rs`
(`broker-mutation-tree-diff.txt`). That test was added for the two
mutants `hatchling-sys`'s first run missed, on the inbox's mode check;
the second run caught both.

The six the broker's run missed are the room disk's put functions, which
only root reaches. The jail's tests can't call them. Each is made to put
nothing by a root hand-break above, and caught:

- `room_disk.rs:1166:5`, `put_inbox` → `Ok(String::new())`, `Ok("xyzzy".into())`: `the-direct-put-says-no-name`
- `room_disk.rs:1176:5`, `put_inbox_held` → the same two: `the-held-put-puts-nothing`
- `room_disk.rs:1183:5`, `put_at` → the same two: `the-put-puts-nothing`

Unviable:

- `room_inbox.rs:29:28: replace * with /`: unviable, build failure
- `room_inbox.rs:116:5: replace readable_inbox -> Result<OwnedFd, Error> with Ok(Default::default())`: unviable, build failure
- `room_inbox.rs:173:5: replace open_inbox -> Result<OwnedFd, Error> with Ok(Default::default())`: unviable, build failure
- `room_inbox.rs:331:5: replace written -> PutError with Default::default()`: unviable, build failure
- `room_disk.rs:1132:5: replace with_open_volume -> Result<T, String> with Ok(Default::default())`: unviable, build failure
- `room_session.rs:2118:9: replace Sessions::put -> PutReply with Default::default()`: unviable, build failure
- `spawner.rs:804:9: replace Spawner::helper -> Result<SpawnedProcess, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:964:9: replace Spawner::room_disk -> Result<Option<crate::room_disk::RoomDiskStatus>, String> with Ok(Some(Default::default()))`: unviable, build failure
- `spawner.rs:1161:9: replace Spawner::room_change -> Result<RoomChanged, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1229:9: replace Spawner::room_job_asked -> Result<RoomJobAnswer, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1310:9: replace SpawnedProcess::watching -> Result<Self, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1819:9: replace Reply::placed -> Self with Default::default()`: unviable, build failure

**The full gate** (`gate.log`), on each toolchain from a clean cache:

- fmt, clippy (default and all features, `-D warnings`), build, the
  workspace's tests, seed-check and protocol-schema: each exit 0.
- 135 test binaries, 3992 passed, 0 failed, on 1.85.0 and on 1.98.1.

## Found on the way

- **The inbox couldn't be read in the Room** (the threat model): it was
  root's `0700`. A put now leaves it `0755` too, so the Room is never told
  of a file it can't read.
- **Clippy 1.85.0 put a lint on `clippy.toml`.** The literal it meant
  was a `format!` inside an `assert!` condition in a test. It was found by
  taking the module's tests out one at a time; the string is made before
  the assert now.

## What wasn't tried

- **Filling the room disk from inside the Room, then a put.** Its plain
  refusal is a unit test of the error it reads (`ENOSPC`, `EDQUOT`).
  Holding room for the inbox is [WO-040](../workorders/workorder-040-room-disk-capacity.md)'s.
- **The syncs.** That the file and the directory are synced before a put
  is said to be done was read, not proven: no test here can lose power.
- **The read-only mount and the modes, each alone in the Room.** Both
  refuse the same changes there. The modes are proved alone by the
  inbox's own tests; the mount by the Room's view check.
- **A put blocks the Room session's thread** while it writes, at most
  8 MiB and two syncs. That was seen in the runs, not measured.
