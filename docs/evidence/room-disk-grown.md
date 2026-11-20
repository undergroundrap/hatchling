# Evidence: a closed room disk grows, keeping what's on it (WO-040 slice 2)

What was tested, how, and what came back, for WO-040's second slice:
growing the agent's room disk.

| | |
|---|---|
| Base | WO-040 slice 1 (`feat/wo040-room-disk-capacity`, `7413533d`) |
| Tested tree | `4aef9483` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox: unprivileged in its jail, and as its root for the room disk's real tests |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.
`docs/storage.md`'s section on it is its own commit too.

## The threat

The room disk holds the agent's work, encrypted, at a fixed size. An
owner whose agent needed more room had one way to give it more: wipe it.
A grow that loses or corrupts what's there would be worse than none; so
would one that leaves the disk unopenable when cut short, or one that runs
while the disk is in use.

## What changed

- **`sudo hatchling-broker room-disk grow <size>`**, as root, with
  Hatchling stopped. It holds the room disk's claim, as maintenance does,
  so a broker that holds the disk has it refused.
- **What a grow does, in order:**
  1. **Refuses before anything changes:** an open disk (stop Hatchling
     first), a size no larger than now (it only grows), one past
     1024 GiB, or one the disk below hasn't room for, saying what it
     needs (the new space and 1 GiB to spare) and what's free.
  2. **Marks the grow**, in `disk.grow` beside the record: the image and
     the size.
  3. **Reserves the image to the new size** (`hatchling-sys`'s new
     `reserve_file_to`): every block allocated, what's there kept, holes
     an older sparse image had reserved too.
  4. **At the next open:** the image is reserved to the marked size if a
     grow was cut short before it was, the mapping covers it all (LUKS2's
     data segment is dynamic), and once mounted, `resize2fs` grows ext4
     to the device, online, under ext4's journal. Then the record says
     the new size, and the mark goes.
- **A grow cut short anywhere is finished at the next open.** One that
  can't be (the disk below has filled since) is undone, and the disk opens
  at the size it is. A failed `resize2fs` leaves the mark for the next
  open, and the disk works at the size its file system is.
- **The mark is apart from the record:** the record refuses unknown
  fields, and a broker from before this reads it as always (it ignores the
  mark, and opens the disk at its file system's size).
- **`resize2fs`** is run as `cryptsetup` and `mke2fs` are: a trusted
  program at `/usr/sbin`, from an argument list, under a deadline.
- **`docs/storage.md`** says how.

## Results

| Check | Result |
|---|---|
| hatchling-sys's file tests | 33 passed, on 1.98.1 and on 1.85.0 |
| The room disk's and the command line's unit tests | 60 passed, on both |
| Both, ten more times each | 0 failed |
| The room disk's real tests, as root (below) | 10 passed |
| Hand-breaks | 22 broken, 22 caught: 17 by unit tests, 5 by the real grow |
| Targeted mutation (`--in-diff`, this slice's lines) | hatchling-sys: 10 mutants, 10 caught; hatchling-broker: 47 mutants, 46 caught, 0 missed, 1 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,303 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |

### As root, on a real file system

`a_closed_room_disk_grows_and_keeps_what_it_held_for_real`, with the real
cryptsetup, mke2fs and resize2fs, run as the root pass runs it (the Room's
supervisor, a cgroup of its own, `HATCHLING_REQUIRE_ROOM_DISK=1`):

| Step | What happened |
|---|---|
| A 4 MiB canary written, and its SHA-256 kept | |
| Grown while open | refused: the room disk is open |
| Closed, grown to twice its size | image twice its size, the grow marked |
| Opened | its file system grown, the canary's digest the same, the mark gone |
| Cut short before its image grew (the mark alone, for three times its size) | finished at open: image and file system grown, canary the same, mark gone |
| Cut short after its file system grew, before it was recorded (the mark again) | the open grew nothing more, cleared the mark, canary the same |
| Wiped | |

The steps between (image grown, open cut short before `resize2fs`) leave
the same state as a finished grow's first open: the mark, and an image of
the new size. The next open does the same again.

### Hand-breaks

| Break | Caught by |
|---|---|
| A file shrunk when asked | `a_file_grows_reserved_and_keeps_what_it_held` |
| Anyone's file grown | the same |
| A file with another name grown | the same |
| Grown sparse, its space not reserved | the same |
| More than is free, found out piece by piece | the same |
| An open disk grown | `a_grow_is_refused_before_anything_changes`, and the real grow |
| Past a room disk's most | the refusal test |
| Grown to the size it is | the same |
| Past what the disk below has free | the same |
| Not marked for the next open | `a_closed_disk_grows_its_image_reserved_and_marked`, and the real grow |
| Another image's mark taken | `a_marked_grow_is_finished_or_undone_at_the_next_open` |
| A grow cut short before its image grew, left short | the same, and the real grow |
| A mark undone, and kept | the marked-grow test |
| Any program run as resize2fs | `finishing_a_grow_grows_the_file_system_then_records_it` |
| Grown, and not recorded | the same |
| Grown, and the mark kept | the same |
| The open never finishes a grow | the real grow |
| The open never looks at the mark | the real grow |
| `room-disk grow <size>` not taken as a command | `valid_args_preserves_operator_and_helper_commands` |
| `room-disk grow <size>` never routed to the grow | `room_disk_grow_is_asked_for_with_its_size_alone` |
| Anyone may grow the room disk | `the_operators_grow_holds_the_claim_and_is_roots_alone` |
| The operator's grow without the room disk's claim | the same |

**Found by these runs:**
- **A refusal said wrong.** With no room disk at all, a grow said the
  room directory couldn't be looked at, not that there was nothing to
  grow. It checks for the directory first now.
- **Eight mutants missed, in the first mutation run.** The command line's
  route, the free-space edge, the public grow and its command were
  untested, and a check at open (`<` as `<=`) changed nothing. Each is a
  tested function now, and the open always reserves the marked size,
  holes included.
- **A break that fills the disk.** With the free-space check broken, a
  test reserves until the disk is full and leaves the file; the break run
  now clears what the tests left between breaks.

**Unviable mutant:** `RoomDisk::at` returning `Ok(Default::default())`,
which `RoomDisk` doesn't have.

## Residual

- **Not grown while open.** An online grow of an open disk is out of
  scope, as the work order says: stop Hatchling first.
- **`HATCHLING_ROOM_DISK_SIZE`** is the size a new disk is made at;
  changing it doesn't grow one.
- **What `doctor` says of a grow** (its size, what it holds, a grow under
  way) is slice 3.
