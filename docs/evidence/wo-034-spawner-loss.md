# Evidence: a Room ends when its spawner is killed (WO-034)

What was tested, how, and what came back, for the change that makes a
Room's first process watch the spawner. Every number here is read from a
file in the results bundle, which is attached to the release that carries
this change.

| | |
|---|---|
| Base commit | `219986c852fbe84e910ef2ef7df5ce0173943b2c` |
| Tested tree | `834416622eba7ff1c65e673518baf2f361d7706b` |
| Date | 2026-11-11 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2) |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `wo034-spawner-loss-results.tar.gz`, 464951 bytes, sha256 `972e5c08282a86e3259d0fcb232a26d7cab7ebe231848856efe81b1e0401dbbf` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## Full gate

Run as an unprivileged user with cgroup delegation,
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, once per
toolchain from a clean build:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked --no-fail-fast
cargo run -q -p hatchling-dev --locked -- seed-check
cargo run -q -p hatchling-dev --locked -- protocol-schema
```

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3627 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3627 | 0 | 0 |

`gate.log` sha256
`b698128e92b2d934855dd80f32ceac478907f538a76f79e9a9a54fbe05526715`.

## Installed tests, as root

The Room's first process needs real root, device-mapper and loop devices,
so these run as root outside the unprivileged gate, with
`HATCHLING_REQUIRE_ROOM_DISK=1`, on 1.98.1:

```text
cargo test --locked -p hatchling-broker --test room_disk -- \
    an_installed_room_ends_when_its_spawner_is_killed \
    the_installed_spawner_hands_the_verified_image_to_its_mount_helper \
    --exact --test-threads=1
```

2 passed, 0 failed (6.40 s and 98.69 s). Afterwards: no leftover mapper,
loop device or test cgroup. `root/final.log` sha256
`ea5a08ad7c629a1109352371ee1b338ebd2dbe58e4d660580201a7df9af78a47`.

## Mutation

Every mutant on the lines changed against the base, per crate, after that
crate's own unmutated baseline, on 1.98.1 with debug info off:

```text
cargo mutants --in-place --in-diff <the crate's diff against the base> --timeout 60 -- --lib
```

| Crate | Mutants | Caught | Missed | Timed out | Unviable | `outcomes.json` sha256 |
|---|---|---|---|---|---|---|
| `hatchling-sys` | 13 | 13 | 0 | 0 | 0 | `5b84b975e1083ca345791a89181b4d44dfa350c9d65820509eccaf664c78091e` |
| `hatchling-broker` | 4 | 3 | 0 | 0 | 1 | `cf1a62197186340b0b40121cd9f7c1af30525fe91a1bac21be45876632611796` |

Unviable:

- `crates/hatchling-broker/src/room_launch.rs:223:5: replace check_mount -> Result<Option<RoomForegroundResult>, String> with Ok(Some(Default::default()))`:
  unviable, build failure.

No mutant was excluded, so there is no equivalence proof to give. Each
caught broker mutant's log was read: each failed the test that guards the
mutated function, for that function's reason.

## Defences broken by hand

The broker's mutants are whole-function replacements. They don't reach
the lines inside the Room's first process that watch the spawner, and no
unprivileged test can run that process. So each defence was removed by
hand, one at a time, and the test that guards it was run and its failure
read. The unbroken tests passed before and after.

| Defence removed | Test that failed | How it failed |
|---|---|---|
| The first process watches the spawner | installed spawner-loss test (root) | "a silent Room workload outlived its killed spawner" |
| Owner loss is reported as itself | installed spawner-loss test (root) | the log read "the Room supervisor exited with status 1" |
| The grace timer starts on owner loss | installed spawner-loss test (root), in the run whose log can't be written | "a silent Room workload outlived its killed spawner" |
| The grace timer ends the process | `end_within_ends_a_busy_process_when_the_grace_is_up` | "the process outlived its grace", wait timed out at 10 s |
| The grace timer waits out the grace | same | "ended before the grace" |
| The program's pidfd is watched | `owner_watched_init_returns_as_its_program_exits_without_waiting_out_a_tick` and `..._ends_when_its_program_was_collected_elsewhere` | "waited out the tick"; wait timed out at 10 s |
| The owner's end is acted on | `owner_watched_init_refuses_a_dead_owner_without_waiting_for_its_program` | wait timed out at 10 s |
| The tick that reaps orphans | `owner_watched_init_reaps_an_orphan_while_its_program_still_runs` | "the orphan was still a zombie" |
| A program collected elsewhere ends the wait | `owner_watched_init_ends_when_its_program_was_collected_elsewhere` | wait timed out at 10 s |

No root run left a mapper, loop device or cgroup behind.

## What this change does not do

- **Speed.** It was measured, and it makes a Room command no faster or
  slower. A timing loop was added to the installed mount-helper test for
  the measurement only (it is not part of this change): two short
  programs, twenty runs each, on the base and on the tested tree in one
  session.

  | Tree | Run | Median, ms | Best to worst, ms |
  |---|---|---|---|
  | Base | 1 | 192 and 206 | 184 to 220 |
  | Tested | 1 | 214 and 200 | 184 to 360 |
  | Tested | 2 | 204 and 206 | 188 to 232 |

  The first tested run had five slow commands (244 to 360 ms) in one of
  its two batches. The repeat had none, so they are read as the machine
  and not the change. About 100 ms of each command is the spawner's own
  wait, which this change doesn't touch. The logs are
  `root/latency-final-*.log` in the bundle.
- **Cleanup.** When a spawner is killed, the read-only image mapping, its
  two loop devices and the empty Room cgroup of that launch are left in
  place. The Room itself, and everything running in it, is gone. Clearing
  what's left at the next start is not part of this change.
