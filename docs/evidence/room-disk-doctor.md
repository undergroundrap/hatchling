# Evidence: doctor says the room disk's size, what it holds, and a grow under way (WO-040 slice 3)

What was tested, how, and what came back, for WO-040's third slice:
what `doctor` says of the agent's room disk.

| | |
|---|---|
| Base | `main` at WO-040 slice 2 (`6ebd72a1`) |
| Tested tree | `e08887bb` |
| Date | 2026-11-21 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox: unprivileged in its jail, and as its root for the room disk's real tests |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The problem

`doctor` said the size a room disk is made at (`HATCHLING_ROOM_DISK_SIZE`),
not the size it is: once grown (slice 2), the disk was said at its old size,
in `doctor` and in the app's Settings. It never said what an open disk holds,
or that a grow was under way, or that one hadn't finished.

## What changed

- **The room disk's status says its own size**: its image's, once there is
  one; the size one's made at, before. Settings shows the same number.
- **`room_disk::looked_at`**: the status and the size a grow under way is
  growing the disk to, read in the same turn as the spawner's operations.
  The grow is its mark beside the record, read only when it's this image's.
  Reading it changes nothing: a mark that isn't the image's is undone at the
  next open, as before.
- **`sudo hatchling-broker doctor`** says, of the room disk:
  - none made yet, and the size it's made at;
  - its size, what it holds and what's free in it while it's open, and what
    it takes of the disk below;
  - what's free below;
  - a grow under way, closed: it finishes when Hatchling starts;
  - a grow left unfinished, open (the open's `resize2fs` failed): a note,
    with how to try again and where it says why;
  - less than a tenth of it free, open: a note, with how to grow it.
- **The wire is unchanged:** the grow under way isn't in the status the
  spawner sends; doctor reads it as root, as it reads the status.
- **`docs/storage.md`** says what doctor says.

## Results

| Check | Result |
|---|---|
| The room disk's and doctor's unit tests | 102 passed, on 1.98.1 and on 1.85.0 |
| hatchling-sys's file tests | 33 passed, on both |
| Both, ten more times each | 0 failed |
| The room disk's real tests, as root | 10 passed |
| Hand-breaks | 10 broken, 10 caught by unit tests; 2 of them broken again as root, and caught by the real grow |
| Targeted mutation (`--in-diff`, this slice's lines) | hatchling-broker: 28 mutants, 17 caught, 0 missed, 0 timeouts, 11 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,307 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |

### As root, on a real file system

`a_closed_room_disk_grows_and_keeps_what_it_held_for_real` now also asks
what doctor is told: after the grow, with the disk closed, its size is the
new one and the grow is under way; once opened, it's open at the new size,
holding what it holds, and no grow is under way.

### Hand-breaks

| Break | Caught by |
|---|---|
| A grown disk said at the size it was made at | `a_grow_under_way_is_read_without_changing_anything`, and the real grow |
| Another image's mark read as this one's | the same unit test |
| A grow under way never read | the same, and the real grow |
| Doctor told of a grow, and dropping it | `doctor_keeps_the_status_and_a_grow_under_way` |
| A grow under way never said | `a_grow_under_way_is_said_and_one_left_unfinished_is_a_note` |
| An open disk's unfinished grow said to finish at the next start | the same |
| A nearly full disk never said | `an_open_room_disk_says_what_it_holds_and_nearly_full_says_to_grow_it` |
| A tenth free said to be too little | the same |
| What an open disk holds never said | the same |
| A disk not made yet said as if it were there | `the_room_disk_is_said_in_numbers_and_too_little_room_is_a_note` |

**Found by these runs:**
- **A check that could never matter.** `looked_at` first skipped the mark
  when no disk was made; with no image there's no image to match a mark,
  so it read none anyway. Writing the hand-breaks showed the branch had
  nothing to break: it's gone.
- **What doctor keeps of the answer, untested.** How doctor split the
  status from the grow was inside the part that reads the machine, where
  no test could break it: it's `kept_of` now, tested.
- **Clippy's length limit.** The room disk's finding outgrew 100 lines:
  its numbers are said by a function of their own.

**Unviable mutants:** the 11 that return `Default::default()`, which
`RoomFacts`, `Finding` and `RoomDiskStatus` don't have.

## Residual

- **What a closed disk holds isn't said:** it can't be read without opening
  it.
- **Run as a user, doctor can't look at the room disk** (it's root's), and
  says to run it with sudo, as before.
