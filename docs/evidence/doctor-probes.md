# Evidence: `doctor` probes the syscall filter, the Room's image and the Windows seal (WO-037 slice 1b)

What was tested, how, and what came back, for WO-037's second slice: `doctor`
probing three protections the inventory found it never checked.

| | |
|---|---|
| Base commit | `bd8dd381` (`feat(doctor): say when a protection is off or weaker (#550)`) |
| Tested tree | `26f7cf33` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The threat

`doctor` said a cell works, or said nothing, where an install lacked these:

- **A kernel without seccomp filtering.** Every cell installs the agent's
  syscall filter before it runs, and fails closed: on such a kernel no cell
  starts. `doctor`'s probe stopped at the namespaces, so it read as fine.
- **The agent's computer, the Room, unable to start.** Its image might not be
  installed, the machine might not be x86_64, or cells might lack a cgroup.
  Nothing checked until a run asked for it. Offering the Room only where it can
  start is a change of behaviour, so it has its own issue, #549.
- **A WSL distro open to Windows.** Windows Setup cuts its distro off from
  the Windows drive and Windows programs, and checks once, at install.
  Nothing checked again.

## What changed

- **The cell probe** (`cell_child`) now takes a cell's last steps after its
  namespaces, as `spawn.rs` does: no new privileges, then the agent's own
  syscall filter (`SeccompProfile::agent`).
  - A refusal is its own step (`Step::Filter`), with its own advice: a kernel
    without seccomp filtering, where no cell starts. It's never put down to
    AppArmor.
  - A cell that works says it runs behind its syscall filter.
- **The Room** (`room_image_said`) is read, never started. It names the first
  thing missing, in order:
  1. x86_64;
  2. an admitted image (`/etc/hatchling/room-image.admission`, read as the Room
     launch reads it);
  3. the image in place;
  4. a cgroup.

  Installed, it says the Room starts when a run asks for it.
- **The Windows seal** (`wsl_seal_said`), under WSL: a note naming what's open,
  the Windows drive (`/mnt/c`) or Windows programs (WSL interop), with how to
  close it. Otherwise it says the distro is cut off.

## Results

| Check | Result |
|---|---|
| `doctor` tests, with `HATCHLING_REQUIRE_CELL=1` (a real cell probed, the filter installed) | 47 passed, on 1.98.1 and on 1.85.0 |
| Hand-breaks | 9 broken, 9 caught, each by the test named for it |
| Targeted mutation (`--in-diff`) | 27 mutants: 20 caught, 0 missed, 0 timeouts, 7 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,269 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |

### Hand-breaks

| Break | Caught by |
|---|---|
| The Room's last missing piece said, not its first | `a_room_that_cant_start_says_why` |
| A Room that can't start, said fine | the same |
| What the Room needs, unsaid | the same |
| A seal open to Windows, said fine | `a_wsl_distro_open_to_windows_is_said` |
| The Windows drive never looked at | the same |
| Windows programs never looked at | the same |
| Somewhere not WSL, its seal checked as if it were | the same |
| A refused filter put down to AppArmor | `a_filter_refused_has_its_own_advice` |
| A refused filter not said to stop every cell | the same |

**Found by these runs:** the first mutation run missed four mutants, all where
`doctor` reads the machine:
- **WSL's interop entry** is now read by a function the tests reach
  (`interop_enabled`).
- **A working cell's words** now come from a pure function
  (`cells_fine`), tested for each mapping.
- **The Room's admission** is now read with the Room launch's own reader
  (`read_admission`), not a new wrapper whose result only a real, root-owned
  record could test.

## Residual

- **A probe that skipped the filter** passes the same tests as one that
  installs it. The real-cell test proves the probe installs the agent's filter
  and still reports, but no test here tells the two apart. Confirming the
  filter's mode from inside would need a call the agent's filter may refuse.
- **An installed Room** is tested only as the words for "installed": no test
  here has a real image to read.
- **The page and the doc** are slice 2. **The Room offered before its image is
  checked** is #549.
