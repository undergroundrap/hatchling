# Evidence: a new room disk's space is reserved on the disk below (WO-040 slice 1)

What was tested, how, and what came back, for WO-040's first slice: the
agent's room disk holds its space from the moment it's made.

| | |
|---|---|
| Base commit | `4d442312` (`feat(install): install the agent's computer's image from a release on Linux; close WO-038 (#555)`) |
| Tested tree | `e25b6080` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox: unprivileged in its jail, and as its root for the room disk's real tests |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The threat

The room disk was a sparse file of a fixed size: it held only what had
been written. Nothing kept the rest of its size free on the disk below, so
anything else on the host could fill it, and the agent's writes would then
fail inside its encrypted disk, with ext4 remounting read-only
(`errors=remount-ro`) and the owner told nothing. That's a way to take the
agent's computer away without touching it.

## What changed

- **`hatchling-sys::make_reserved_file`** (was `make_sparse_file`) makes the
  image as before (a new file, never over one or through a link, 0600) and
  allocates every block of it (`fallocate`, mode 0): its space is the
  disk's from then on.
  - **More than is free is refused before anything is made,** saying what
    was asked for and what's free.
  - **A piece at a time**, 8 MiB each. A signal can interrupt a
    reservation: tmpfs gives back that call's part and says EINTR. Only the
    piece interrupted is asked again, up to 100 times, so what's reserved
    stays and it always gets on.
  - **A file whose reservation fails is removed,** so a failure keeps no
    space.
- **The room disk's create** uses it, and says "the room disk's space
  can't be reserved here" when it can't. The check that a new disk's size
  and 1 GiB are free is as before.
- **Docs:** `docs/storage.md`, the module's own and the setting's help say
  the space is reserved.

**Breaking for `hatchling-sys`:** `make_sparse_file` is now
`make_reserved_file`.

## Results

| Check | Result |
|---|---|
| hatchling-sys's file tests (`ownership::`) | 32 passed, on 1.98.1 and on 1.85.0 |
| The room disk's unit tests (`room_disk::`) | 35 passed, on both |
| Both, ten more times each | 0 failed |
| The room disk's real tests, as root (below) | 9 passed |
| Hand-breaks | 9 broken, 9 caught |
| Targeted mutation (`--in-diff`) | hatchling-sys: 19 mutants, 18 caught, 0 missed, 1 unviable; hatchling-broker: 1, caught |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,293 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |

### As root, on a real file system

`crates/hatchling-broker/tests/room_disk.rs`, built unprivileged and run as
the sandbox's root as `scripts/root-pass.sh` runs it: the real cryptsetup
and mke2fs, the Room's development supervisor
(`scripts/room-image-supervisor.sh`), a cgroup of its own, and
`HATCHLING_REQUIRE_ROOM_DISK=1`, so a skip is a failure. All nine passed.
Among them, `a_room_disk_is_made_opened_closed_and_wiped_for_real` makes a
room disk and checks that its image holds at least its whole size on the
disk below (it checked the opposite, that it was sparse, before).

### Hand-breaks

| Break | Caught by |
|---|---|
| The file sized, not reserved: sparse again | the reserved file's test, and the room disk's status test |
| Every piece reserved at the file's start | the same two |
| The space reserved, the file left at no size | the same two |
| The pieces never moving on | `a_reservation_goes_a_piece_at_a_time_and_ends_at_a_failure` |
| A failed piece not ending it | the same |
| Too little free, found out only piece by piece | the reserved file's test |
| An interrupted piece not asked again | `an_interrupted_call_is_asked_again_up_to_its_tries` |
| An interrupted piece asked again for ever | the same |
| A file whose reservation failed, left behind | `a_file_whose_filling_fails_is_removed` |

**Found by these runs:**
- **Interrupted reservations.** The first runs reserved the whole file in
  one call. It passed once, then failed at random in the gate and a
  mutation baseline: on tmpfs, under the tests' signals, the call was
  interrupted (EINTR), and gave back what it had. Asked again whole, it
  still failed every one of 100 times. Reserved a piece at a time, with
  only the interrupted piece asked again, ten more runs of each suite
  failed none.
- **Filling the disk to find out.** Reserved in pieces, a size bigger than
  the disk would have filled it before failing: what's free is now checked
  first.
- **An edge no test could reach.** Mutation found `free < size` as
  `free <= size` (exactly what's free refused) untested: the comparison is
  now `fits`, tested at the edge.

**Unviable mutant:** `again_if_interrupted` returning
`Ok(Default::default())`, which its result type doesn't have.

## Residual

- **A room disk made before this** keeps its sparse file. Growing it
  (slice 2) reserves all of it.
- **The free-space check and the reservation are two steps:** something
  else can take space between them. The reservation then fails, and the
  file made is removed.
- **Slices 2 and 3** (growing a closed disk, and `doctor`) are still to
  come.
