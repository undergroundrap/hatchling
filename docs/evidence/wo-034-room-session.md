# Evidence: the Room is kept between commands (WO-034)

What was tested, how, and what came back, for the change that keeps one
Room session for the install and runs each command in it. Every number
here is read from a file in the results bundle, which is attached to the
release that carries this change.

| | |
|---|---|
| Base commit | `d352530070b8259b968c8322b96ec732f37b8196` |
| Tested tree | `81d8ed5b97f0a5f824432780818949fb5a3659df` |
| Date | 2026-11-11 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2) |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `wo034-room-session-results.tar.gz`, 3767694 bytes, sha256 `4d096c16bfbdc89073af54f47be9febf27c763628acc94e5f8fe883813afaea4` |

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
| 1.85.0 | 0 | 135 | 3662 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3662 | 0 | 0 |

`gate.log` sha256
`55d49a17403fc7d552c5af8a604a35ddc8b78a631e0b3450093e70e23a399709`.

The broker's library tests were then run 25 times over on 1.98.1, every
test in parallel: 25 runs, 1320 passed and 0 failed in each. `stress.log`
sha256 `2939d4de5afb11be4135bea2adc961a9048e704730ac5181bc4230f047813d7e`.

## The supervisor

The Room's supervisor was built static with `scripts/room-image-supervisor.sh`
from the tested tree (sha256
`7c928534af3a6897de6cbdd24a5b4ae08b263e8b38790121de8a73b9e1f3cd75`), and
its own tests and its proof under the Room's syscall filter were run
static and release, as CI runs them:

```text
cargo test --locked --release --target x86_64-unknown-linux-gnu \
    -p hatchling-browser --features room-supervisor \
    --bin hatchling-room --test room_supervisor
```

4 unit tests passed and the three proofs passed under the filter. The
first proof now sends two commands, one after another, on one connection.
The installed tests below use this binary.

## Installed tests, as root

A Room needs real root, device-mapper and loop devices, so these run as
root outside the unprivileged gate, with `HATCHLING_REQUIRE_ROOM_DISK=1`,
on 1.98.1:

```text
cargo test --locked -p hatchling-broker --test room_disk -- \
    the_installed_spawner_hands_the_verified_image_to_its_mount_helper \
    an_installed_room_ends_when_its_spawner_is_killed \
    --exact --test-threads=1
```

2 passed, 0 failed (100.89 s and 7.08 s). Afterwards: no leftover mapper,
loop device or test cgroup. `root/final.log` sha256
`536540c6d15f7048517fb4a87c8ad21853c995219a0ef025318c3f09f0dbfa3e`.

What the first of them now checks of a kept Room, against the installed
spawner:

- A second command finds the scratch the first one left, which only the
  same Room has. A status, an open of the disk and a mount check in
  between leave the Room as it is.
- Closing the disk ends the Room first: its image mapping is gone, and
  the next command starts another Room with scratch of its own.
- A command sent with the disk closed is refused with the disk's own
  reason, and starts nothing.
- A second command while one runs is refused as busy, and the disk still
  answers a status.
- A command past its deadline ends the Room, and its refusal arrives only
  once the Room's cgroup is gone.
- A Room whose image mapping can't be closed stays in the session's
  charge: a close, a command and a mount check are each refused with the
  reason, and an operator's close from another process is refused as in
  use. Once the mapping can be closed, a close clears the Room and closes
  the disk.
- When the broker goes with a Room idle, the spawner ends the Room, closes
  the disk and exits cleanly, with nothing of the Room left.

## Mutation

Every mutant on the lines changed against the base, per crate, after that
crate's own unmutated baseline, on 1.98.1 with debug info off:

```text
cargo mutants --in-place --in-diff <the crate's diff against the base> --timeout 60 -- --lib
cargo mutants --in-place --in-diff <the supervisor's diff> --timeout 60 \
    --features room-supervisor -- --bin hatchling-room
```

| Crate | Mutants | Caught | Missed | Timed out | Unviable | `outcomes.json` sha256 |
|---|---|---|---|---|---|---|
| `hatchling-sys` | 15 | 14 | 0 | 0 | 1 | `149aec21a6cc338c47e880faa5bdc11551e5400d942f7d2eaec178de7e26ae53` |
| `hatchling-browser` (the supervisor) | 1 | 1 | 0 | 0 | 0 | `d95337a533bd74dcae8585941bddaa5f0142cca99b7943e0a2dcee5751a1dbb6` |
| `hatchling-broker` | 101 | 80 | 0 | 0 | 21 | `2dbe60b9327f496622367964dfa067550a678bd8faddb587e0cdd571a51bce87` |

No mutant was excluded, so there is no equivalence proof to give.

Unviable:

- `crates/hatchling-sys/src/fds.rs:435:50: replace + with * in wait_readable_for`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_disk.rs:1156:5: replace run -> Result<Option<RoomDiskStatus>, String> with Ok(Some(Default::default()))`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_disk.rs:1190:5: replace run_held -> Result<Option<RoomDiskStatus>, String> with Ok(Some(Default::default()))`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_launch.rs:252:5: replace hand_over -> Result<BorrowedFd<'a>, String> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_launch.rs:334:35: replace + with * in admit`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:119:9: replace Door::pair -> io::Result<(Self, Seat)> with Ok((Default::default(), Default::default()))`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:192:9: replace Clock::due -> (Instant, Due) with (Default::default(), Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:193:24: replace + with * in Clock::due`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:195:27: replace + with * in Clock::due`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:201:32: replace + with * in Clock::due`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:349:48: replace + with * in Kept<D>::intake`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:380:9: replace Kept<D>::keep -> Result<Infallible, String> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:655:9: replace Sessions::ask -> Result<Asked<'_, T>, String> with Ok(Asked::new())`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:655:9: replace Sessions::ask -> Result<Asked<'_, T>, String> with Ok(Asked::from_iter([Default::default()]))`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:655:9: replace Sessions::ask -> Result<Asked<'_, T>, String> with Ok(Asked::new(Default::default()))`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:655:9: replace Sessions::ask -> Result<Asked<'_, T>, String> with Ok(Asked::from(Default::default()))`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:674:9: replace Sessions::ask_or -> Result<T, String> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:687:9: replace Sessions::run -> RunReply with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:738:9: replace Sessions::disk -> DiskReply with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/spawner.rs:1332:5: replace room_sessions -> Sessions with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/spawner.rs:1573:5: replace room_disk_reply -> Reply with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/spawner.rs:1589:5: replace room_disk_done -> DiskReply with Default::default()`:
  unviable, build failure.

Two of those hide a mutant that would build. `Sessions::disk` and
`room_disk_done` return a `Result` through a type alias, so the tool made
only a `Default::default()` of each. What it makes of such a function when
it sees the `Result` is a body that does nothing and returns `Ok(None)`.
Both were applied by hand, and each failed two tests
(`alias-breaks.log`): the answers were `Ok(None)` where
`Err("direct Status")` and the refusal for a spawner with no room disk
were due.

## Defences broken by hand

The tool's mutants are replacements of a function or an operator. They
don't remove a statement, and no unprivileged test can launch a Room. So
each defence was removed by hand, one at a time, and the tests that guard
it were run and their failures read. The unbroken tests passed before and
after.

| Defence removed | Test that failed | How it failed |
|---|---|---|
| An idle session ends | `an_idle_session_ends_at_its_idle_limit_counted_from_its_last_command` | "idled for 9.2s" |
| Idleness counts from the last command | same | "ended 383µs after its last command" |
| A session has a time limit | `a_session_ends_at_its_time_limit_whatever_it_is_doing` | "lived 10.0s" |
| A command has a deadline | `a_command_past_its_deadline_ends_the_session` | "took 10.0s" |
| The requester's connection is watched | `a_requester_that_goes_or_says_more_ends_the_session` | ended on the deadline, not on the requester |
| Nothing launches for a requester already gone | `nothing_is_launched_for_a_requester_that_has_gone` | `Ok(())` where the refusal was due |
| The first process's end is acted on | `a_supervisor_out_of_step_ends_the_session` | ended on the time limit instead |
| An idle supervisor's words or closing end the session | same | ended on the time limit instead |
| A supervisor that closes mid-frame ends the session | same, `a_result_is_checked_before_it_is_an_answer`, `listening_is_bounded_by_the_frame_it_is_for` | ended on a deadline; `Ok(())` for a closed socket |
| A command waits for readiness | `a_command_waits_for_readiness_and_its_result_may_come_in_parts`, `a_live_session_says_the_room_works_once_it_is_ready` | "a command went to an unready supervisor"; "an unready Room was said to work" |
| A second command is refused | `a_second_command_is_refused_while_one_runs` | no refusal came |
| A close or wipe ends the session first | `closing_or_wiping_the_disk_ends_the_session_first` | the session itself closed the disk it held |
| A failure is answered only once the Room is down | that test, the deadline test and the time-limit test | "answered before the Room was down" |
| A command waiting on a failed launch is answered | `a_launch_that_fails_at_once_answers_every_waiting_command`, `the_spawners_session_thread_answers_what_started_it` | no answer; "the Room session kept ending" where the launch's reason was due |
| A session that can't be cleared stays held | `a_session_that_cannot_be_cleared_stays_held_and_says_so`, `a_close_makes_a_held_session_try_again_at_once` | no refusal came |
| A held session is left once the broker has gone | `a_held_session_is_left_when_the_broker_has_gone` | 4 tries where 2 were due |
| An ended session is seen out before the Room is reused | `the_disk_and_the_check_go_through_a_live_session_only` | "the session wasn't seen out" |
| No session starts after the broker has gone | `a_shut_spawner_ends_its_session_and_starts_no_other` | a command ran in a new session |
| The broker's going waits for the session to be down | that test, `room_requests_reach_the_session_and_it_ends_with_the_broker` | "the session wasn't seen out"; "the spawner left before its session was down" |
| The spawner ends its session as it leaves | `room_requests_reach_the_session_and_it_ends_with_the_broker` | "the spawner left before its session was down" |
| A disk request goes through the session | same | the spawner's own refusal where the session's answer was due |
| A mount check goes through the session | same | the spawner's own refusal where the session's answer was due |
| A command is checked before it starts a session | same | an invalid command wasn't refused |
| The session can't close the disk under itself | `a_held_disk_answers_its_holder_and_is_not_closed_under_it` | `Ok(None)` where the refusal was due |
| The supervisor goes on after a command | `serve_runs_one_command_after_another` (the supervisor's own) | broken pipe on the second command |
| The Room is taken down when its session ends | installed mount-helper test (root) | "the timeout left a Room cgroup" |
| The launched Room is held | installed mount-helper test (root) | "the Room session kept ending; try again" |

No root run left a mapper, loop device or cgroup behind.

## Speed

A timing loop was added to the installed mount-helper test for the
measurement only (it is not part of this change). After the disk is closed
and opened again it times one short command, which has to launch a Room,
and then twenty more. The test runs the loop twice. The base is
`219986c852fbe84e910ef2ef7df5ce0173943b2c`, the commit before the
spawner-loss change, which was measured not to change a command's time.

| Tree | Run | First command, ms | Next twenty: median, ms | Best to worst, ms |
|---|---|---|---|---|
| Base | 1 | 257.0 | 224.1 | 200.1 to 240.0 |
| Base | 2 | 193.7 | 218.0 | 203.7 to 264.3 |
| Tested | 1 | 85.7 | 0.83 | 0.70 to 2.64 |
| Tested | 2 | 101.3 | 0.76 | 0.66 to 0.98 |

On the base every command launches a Room and takes it down. On the
tested tree only the first does, and it no longer waits for the Room to be
taken down before it answers. The logs are `root/latency-base.log` and
`root/latency-candidate.log` in the bundle.

## What this change does not do

- **Leftovers.** A spawner that is killed outright, as opposed to a broker
  that goes, still leaves the read-only image mapping, its two loop
  devices and the empty cgroup of the Room it had. The Room itself ends,
  and the next launch works. A kept Room lives up to an hour, so a killed
  spawner is now likelier to have one. Clearing what's left at the next
  start is not part of this change.
- **Stray processes.** What a command leaves running in the Room lives
  until the session ends, inside the Room's cgroup limits.
- **Maintenance.** An operator's key maintenance from another process is
  refused while a session lives, as it was during a single command, and so
  for up to fifteen minutes after the last command.
- **The frontend.** Nothing in `frontend/` or the protocol types changed,
  and the protocol schema check passed; the frontend's own checks were
  left to CI.
