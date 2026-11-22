# Evidence: doctor says what installs the Room's image (#584)

What was found, what changed, and how it was tested, for
[#584](https://github.com/undergroundrap/hatchling/issues/584), found in
the v0.9.0 release candidate's physical-key check.

| | |
|---|---|
| Base | `main` at `8de0e034` (v0.9.0) |
| Date | 2026-11-22 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu), toolchains 1.98.1 and 1.85.0 |

This record was added after the change, in its own commit. It changes no code.

## The problem

With the Room unable to start, `doctor`'s note gave one fix whatever the
cause: *Windows Setup installs it (docs/windows.md); on Linux, it isn't
packaged yet*. Since 0.9.0 the Linux installer adds the image from a
release that has it, on x86_64, so the second half was untrue. The first
half was no help where the cause wasn't a missing image: on a machine
that isn't x86_64, or where cells have no cgroup, installing changes
nothing.

## The change

`RoomMissing::fix` in `crates/hatchling-broker/src/doctor.rs` gives the
fix for each cause:

| Cause | What `doctor` says to do |
|---|---|
| not x86_64 | run Hatchling on an x86_64 machine; everything else works here without it |
| no image admitted, or not in place | install a release that has its image, on x86_64: the installer adds it on Linux (docs/install.md), Windows Setup on Windows (docs/windows.md) |
| no cgroup for cells | give cells a cgroup, as the cell limits line above says |

## Tests

`doctor::tests::a_room_that_cant_start_says_why` now checks each cause's
fix, and that none says the image isn't packaged.

| Check | 1.98.1 | 1.85.0 |
|---|---|---|
| `cargo fmt --all --check` | pass | |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass | pass |
| `hatchling-broker --lib doctor::` | 60 passed | 60 passed |

**Hand-break:** with the old fix line put back, the test fails
(`doctor.rs:2090`), and passes again with the change.
