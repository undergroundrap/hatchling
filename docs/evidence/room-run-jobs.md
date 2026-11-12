# Evidence: a command in the Room is a job

What was tested, how, and what came back, for the change that makes a
command in the Room a named job: several at once, each with a limit, a
stop, and what it wrote kept as a head and a tail with the whole in a
file. Every number here is read from a file in the results bundle, which
is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `2e431964` (`fix(room): close the room disk where the root mount isn't shared (#424)`) |
| Tested tree | `9e6d019b040313a8d106c2f2a9eae3e803ee848a` |
| Date | 2026-11-12 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), systemd 259: a sandbox for the gate, and root outside its jail for the Room itself |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-room-run-jobs-results.tar.gz`, 5894606 bytes, sha256 `ed68cf0f2446f9852388caa3a333318f60d545724807f56acd5aaea9069d1d53` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

Before, the Room ran one command at a time. A command that missed its
30-second deadline ended the Room, and the first 4 KiB of each output
stream was all that came back.

- **A command is a job, named by whoever asks for it.** A name the
  spawner knows is never run again: asking by it answers with that job as
  it is. A caller that lost an answer asks, and nothing runs twice.
- **Up to four jobs run at once**, each in a process group of its own.
- **A job has a limit**: 120 seconds unless one is named, never more than
  600. Still running then, it is stopped, which kills its process group.
  A stop can also be asked for. The Room is kept either way; it ends only
  if a stopped job hasn't ended five seconds later.
- **A caller waits only as long as it says**, and is then told the job is
  running. Whoever asks by the job's name later is told how it is.
- **What a job wrote is kept** as the first and the last 16 KiB of each
  stream, with the count of the bytes between them, and the whole of each
  stream, up to 8 MiB, in a file in the Room's session directory.
- **A job is given** a working directory, up to 32 KiB on its standard
  input, and a fixed environment. Its home is in the session directory,
  so nothing a job leaves there is read by a later session's jobs.
- **The wire between the spawner and the Room's supervisor is typed**
  (`hatchling_core::room`): unknown fields never decode, the supervisor
  is asked one thing at a time and must echo each ask's nonce, and the
  one thing it says unasked is that a job it was given has ended.
- **The spawner keeps what is known of the last 64 jobs that are over**,
  across sessions. A job still running when its Room ends is recorded as
  lost with it; one the supervisor was never given didn't start, and its
  name stays free.

Nothing in the app or the agent uses the Room yet. `hatchling-broker`'s
`Spawner::run_room_foreground` and its result types are replaced by
`room_run`, `room_job`, `room_stop` and `room_output`; both the old and
the new are additions of the unreleased 0.9.

## In a real Room, as root

Outside the sandbox's jail, as root, on a clone of the tested tree: a real
keeper, a LUKS2 room disk, the verified image fixture, the Room's cgroup
and its syscall filter, and the supervisor built static from the tested
tree (sha256
`6903def7914338bb47a5682dd4989f3d7241f3b94ca3fe1e03484b480a429acf`).

| Run | Result |
|---|---|
| `an_installed_room_ends_when_its_spawner_is_killed` | passed: a job started over the new wire is in the Room's cgroup, and is gone when the spawner is killed |
| `the_installed_spawner_hands_the_verified_image_to_its_mount_helper` | passed, with everything below |

What the second test does in the Room, each step asserted:

- A job runs, its exit code and both streams come back, and its file is
  on the room disk. Asked for again by the same name with other
  arguments, it is not run: the answer is the first job, marked as not
  started by this ask.
- The next job finds the first one's scratch: same Room. A status, an
  open and a check of the disk leave the Room as it is.
- A job started in `/session` sees exactly
  `HOME=/session/home`, `LANG=C.UTF-8`, `PATH=/usr/bin:/bin`,
  `TMPDIR=/session`, and nothing else. A job reads the bytes it was
  given, a byte that isn't text among them.
- Closing the disk ends the Room; the next job starts another, and how
  the earlier Room's jobs came out is still known.
- A job that writes 131072 bytes to standard error before 4096 to
  standard output: 16384 kept at each end of the first, 98304 counted
  between, and a later job in the same Room finds the whole 131072 in
  the stream's file.
- A program that isn't there is refused, with why.
- The scratch is filled to its cap. A job still runs after that, its
  head is still kept, and its file is said to be cut.
- A job with a 2-second limit is stopped at it, is said to have been
  stopped for its limit, and the Room is kept.
- A job whose first process exits leaving a child holding its output
  ends at once, with the exit code.
- A job its caller doesn't wait for is said to be running; another runs
  beside it in the same Room; a stop ends it and says it was asked for.
- A supervisor that answers something it was never asked ends the Room,
  and the job it was given is recorded as lost.
- A Room that can't be cleared refuses jobs with the reason, and a close
  clears it once it can be.

Neither run left a mapping or a loop device (`root/after.out`).

How long a job takes, in the same Room, from the broker's request to its
answer. The first job launches the Room; the next twenty reuse it. Each
run of the test measures twice.

| Run | First job, ms | Next twenty: median, ms | Best to worst, ms |
|---|---|---|---|
| 1 | 78.8 | 0.73 | 0.68 to 0.90 |
| 1 | 73.3 | 0.71 | 0.69 to 0.93 |
| 2 | 81.2 | 0.70 | 0.66 to 0.84 |
| 2 | 59.1 | 0.73 | 0.67 to 0.94 |

The record of the change before this one (`wo-034-room-session.md`)
measured the one-command wire on this machine at 0.76 to 0.83 ms median:
a job costs no more than a command did. The logs are
`root/latency-candidate-1.log` and `root/latency-candidate-2.log`.

## The supervisor under the Room's filter

The supervisor's own program is built with its tests and run, outside a
Room, under the Room's syscall filter (`tests/room_supervisor.rs`). It is
given a session directory as its one argument for this; the Room's
launcher gives it none. Three proofs pass: a job gets none of the
supervisor's descriptors or environment, a stop kills a running job, and
what a job reads and writes is kept in memory and in its files; it ends
on what it can't read; and it ends its jobs when the spawner goes.
`root/super.log` sha256
`c3af03c441221fa38fbaa8608349e66b1923edc07d53435dcf3aedcd3ca3bb79`.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3749 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3749 | 0 | 0 |

`gate.log` sha256
`9b161fd1fad1a0151b512488170ba4dc1cf5684a7f1792240d633e58877290e8`.

The broker's library tests were then run 15 times over on 1.98.1, every
test in parallel: 15 runs, 1379 passed and 0 failed in each. `stress.log`
sha256 `afef664b8407928eac1320fbb92e6ddcc5dddc0ab6b391cc64c57a36ec179f5b`.

The app is unchanged, so its checks were not run. No install was drilled:
nothing an install runs reaches this code yet.

## Mutation

Every mutant on the lines changed against the base, on 1.98.1 with debug
info off, one run per crate:

```text
cargo mutants --in-place --in-diff <the crate's diff> --timeout 60 -- --lib
cargo mutants --in-place --in-diff <the supervisor's diff> --features room-supervisor --timeout 60 -- --bin hatchling-room
```

| Crate | Mutants | Caught | Missed | Timed out | Unviable | `outcomes.json` sha256 |
|---|---|---|---|---|---|---|
| `hatchling-core` | 86 | 82 | 0 | 0 | 4 | `a595de13649f6e290594fd41370cf13726dce35c4d3809f2d6aa77599baef49c` |
| `hatchling-browser` (the supervisor) | 44 | 33 | 1 | 0 | 10 | `abb1889da4557708887cc67ae4cb0203de6c753d4c40c3b65996a3a03b3b3921` |
| `hatchling-broker` | 159 | 125 | 0 | 0 | 34 | `588c001d2ad224b8a5a44a428f82f489dd62c0224845ace3521041c9c887b851` |

The one missed is the supervisor's entry, which only running the program
reaches:

- `crates/hatchling-browser/src/bin/hatchling-room.rs:54:5: replace main -> ExitCode with Default::default()`

It was run against the program's proof under the Room filter, and the
proof caught it: the supervisor that returns at once never says it is
ready.

```text
cargo mutants --in-place --in-diff <the supervisor's diff> --features room-supervisor --re 'replace main ' --timeout 120 -- --test room_supervisor
```

| Run | Mutants | Caught | Missed | `outcomes.json` sha256 |
|---|---|---|---|---|
| The supervisor's entry, against its proof | 1 | 1 | 0 | `2f50ace1e27f63a294cf32430655f4e8999d49264e3f1b2beb932ceaecaf8bcd` |

No mutant was excluded, so there is no equivalence proof to give.

Unviable (each: unviable, build failure):

- `crates/hatchling-core/src/room.rs:45:33: replace * with /`
- `crates/hatchling-core/src/room.rs:98:9: replace <impl FromStr for JobId>::from_str -> Result<Self, Self::Err> with Ok(Default::default())`
- `crates/hatchling-core/src/room.rs:121:9: replace <impl TryFrom<String> for JobId>::try_from -> Result<Self, Self::Error> with Ok(Default::default())`
- `crates/hatchling-core/src/room.rs:449:9: replace b64::serialize -> Result<S::Ok, S::Error> with Ok(Default::default())`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:83:9: replace Place::room -> Self with Default::default()`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:88:9: replace Place::under -> Self with Default::default()`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:119:5: replace private -> io::Result<File> with Ok(Default::default())`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:244:46: replace + with * in Job::exit`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:250:9: replace Job::ended -> Option<Ended> with Some(Default::default())`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:321:9: replace Room<'_>::wait -> Result<(bool, Vec<Watch>)> with Ok((true, vec![Default::default()]))`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:321:9: replace Room<'_>::wait -> Result<(bool, Vec<Watch>)> with Ok((false, vec![Default::default()]))`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:384:9: replace Room<'_>::finish -> Vec<Say> with vec![Default::default()]`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:417:9: replace Room<'_>::answer -> Say with Default::default()`
- `crates/hatchling-browser/src/bin/hatchling-room.rs:477:9: replace Room<'_>::start -> std::result::Result<Job, NotStarted> with Ok(Default::default())`
- `crates/hatchling-broker/src/room_job.rs:84:5: replace check -> Result<Checked, String> with Ok(Default::default())`
- `crates/hatchling-broker/src/room_session.rs:132:9: replace Over::lost -> Self with Default::default()`
- `crates/hatchling-broker/src/room_session.rs:165:9: replace Ledger::get -> Option<&Over> with Some(Box::leak(Box::new(Default::default())))`
- `crates/hatchling-broker/src/room_session.rs:181:5: replace opened -> MutexGuard<'_, Ledger> with MutexGuard::new()`
- `crates/hatchling-broker/src/room_session.rs:181:5: replace opened -> MutexGuard<'_, Ledger> with MutexGuard::from_iter([Default::default()])`
- `crates/hatchling-broker/src/room_session.rs:181:5: replace opened -> MutexGuard<'_, Ledger> with MutexGuard::new(Default::default())`
- `crates/hatchling-broker/src/room_session.rs:181:5: replace opened -> MutexGuard<'_, Ledger> with MutexGuard::from(Default::default())`
- `crates/hatchling-broker/src/room_session.rs:186:5: replace told -> RoomJobAnswer with Default::default()`
- `crates/hatchling-broker/src/room_session.rs:322:9: replace Clock::due -> (Instant, Due) with (Default::default(), Default::default())`
- `crates/hatchling-broker/src/room_session.rs:322:33: replace + with * in Clock::due`
- `crates/hatchling-broker/src/room_session.rs:328:24: replace + with * in Clock::due`
- `crates/hatchling-broker/src/room_session.rs:485:9: replace Kept<D>::over -> Option<RoomJobState> with Some(Default::default())`
- `crates/hatchling-broker/src/room_session.rs:512:68: replace + with * in Kept<D>::intake`
- `crates/hatchling-broker/src/room_session.rs:578:37: replace + with * in Kept<D>::run`
- `crates/hatchling-broker/src/room_session.rs:591:29: replace + with * in Kept<D>::run`
- `crates/hatchling-broker/src/room_session.rs:742:23: replace + with * in Kept<D>::expire`
- `crates/hatchling-broker/src/room_session.rs:752:26: replace + with * in Kept<D>::expire`
- `crates/hatchling-broker/src/room_session.rs:768:9: replace Kept<D>::next -> Instant with Default::default()`
- `crates/hatchling-broker/src/room_session.rs:770:36: replace + with * in Kept<D>::next`
- `crates/hatchling-broker/src/room_session.rs:775:47: replace + with * in Kept<D>::next`
- `crates/hatchling-broker/src/room_session.rs:824:9: replace Kept<D>::keep -> Result<Infallible, String> with Ok(Default::default())`
- `crates/hatchling-broker/src/room_session.rs:1272:9: replace Sessions::over -> Option<RoomJobAnswer> with Some(Default::default())`
- `crates/hatchling-broker/src/room_session.rs:1313:9: replace Sessions::run -> JobReply with Default::default()`
- `crates/hatchling-broker/src/room_session.rs:1356:9: replace Sessions::job -> JobReply with Default::default()`
- `crates/hatchling-broker/src/room_session.rs:1373:9: replace Sessions::stop -> JobReply with Default::default()`
- `crates/hatchling-broker/src/spawner.rs:782:9: replace Spawner::helper -> Result<SpawnedProcess, String> with Ok(Default::default())`
- `crates/hatchling-broker/src/spawner.rs:891:9: replace Spawner::room_disk -> Result<Option<crate::room_disk::RoomDiskStatus>, String> with Ok(Some(Default::default()))`
- `crates/hatchling-broker/src/spawner.rs:962:9: replace Spawner::room_run -> Result<RoomJobAnswer, String> with Ok(Default::default())`
- `crates/hatchling-broker/src/spawner.rs:973:9: replace Spawner::room_job -> Result<RoomJobAnswer, String> with Ok(Default::default())`
- `crates/hatchling-broker/src/spawner.rs:987:9: replace Spawner::room_stop -> Result<RoomJobAnswer, String> with Ok(Default::default())`
- `crates/hatchling-broker/src/spawner.rs:1023:9: replace Spawner::room_job_asked -> Result<RoomJobAnswer, String> with Ok(Default::default())`
- `crates/hatchling-broker/src/spawner.rs:1096:9: replace SpawnedProcess::watching -> Result<Self, String> with Ok(Default::default())`
- `crates/hatchling-broker/src/spawner.rs:1431:5: replace room_sessions -> Sessions with Default::default()`
- `crates/hatchling-broker/src/spawner.rs:1563:5: replace room_job_reply -> Reply with Default::default()`

## Defences broken by hand

Each defence was removed by hand, one at a time, and the tests that guard
it were run and their failures read. The unbroken tests passed before and
after. 39 breaks, none survived. `unit-breaks.log` sha256
`a61b261bb006cbf54b89317cb81930dcd47af709ca3f7ccc61774ae56b59448b`.

The wire:

| Defence removed | Test that failed | How it failed |
|---|---|---|
| A job's name can't start with a dot | `a_jobs_name_is_a_plain_file_name` | "." was taken |
| A program is named plainly or by an absolute path | `a_command_is_a_plain_or_absolute_program_and_bounded_arguments` | a relative path was taken |
| A tail only past a full head | `kept_counts_must_be_true_together` | a tail without one passed |
| A file is cut exactly when it's short | same | "short, and not said to be cut" |

The supervisor:

| Defence removed | Test that failed | How it failed |
|---|---|---|
| A job gets a cleared environment | `a_job_is_given_its_directory_a_fixed_environment_and_its_input` | it saw the supervisor's own |
| What a job leaves in its group ends with it | `what_a_job_leaves_running_in_its_group_ends_with_it` | "what the job left running outlived it" |
| A stop kills the whole group | `a_stop_kills_everything_in_the_jobs_group` | "the job's child outlived its stop" |
| A job has a process group of its own | same | the stop reached nothing; no end was said |
| A pipe held outside the group is read only so long | `a_pipe_held_outside_the_jobs_group_does_not_hold_its_end` | no end was said |
| A known name isn't started again | `a_name_is_used_once_for_as_long_as_its_job_is_kept` | started twice |
| Only so many jobs run at once | `only_so_many_jobs_run_at_once` | a fifth started |
| A stream's file stops at its cap | `a_streams_file_stops_at_its_cap_and_says_so` | "a job's output file holds more than it can" |
| Only so many ended jobs are kept | `only_so_many_ended_jobs_are_kept` | the oldest was still there |

The session:

| Defence removed | Test that failed | How it failed |
|---|---|---|
| A known name starts nothing in the session | `a_name_that_is_known_is_never_run_again` | started again; no answer came |
| A name in the ledger starts no session | `a_run_is_checked_and_a_known_name_answered_before_any_session` | a session was started |
| The session runs only so many | `only_so_many_jobs_run_at_once_and_each_ends_as_itself` | a fifth was sent to the supervisor |
| The ledger keeps only so many | `the_ledger_lets_its_oldest_job_go` | the oldest was still there |
| An answer must echo its ask's nonce | `a_supervisor_out_of_step_ends_the_session` | another nonce was taken |
| An end only of a job said to have started | same | an end before its start was taken |
| An end only once | same | a second end, while output was being fetched, was taken |
| An end is checked for sense | `a_jobs_end_is_checked_before_it_is_an_answer` | a signal that isn't one was an answer |
| A piece is the length it was said to be | same | a longer one was taken |
| A job's limit is held | `a_job_past_its_limit_is_stopped_and_the_session_goes_on` | no stop was asked |
| A stopped job must end within its grace | `a_job_that_does_not_end_when_stopped_ends_the_session` | the session went on |
| An ask must be answered in time | `a_supervisor_that_goes_quiet_ends_the_session` | the session went on |
| Not told "running" while being stopped | `a_caller_that_waits_out_the_limit_is_told_how_the_job_came_out` | "told it runs as it was being stopped" |
| Not told "running" before it started | `a_caller_waits_only_so_long_and_its_job_runs_on` | "told it runs before it started" |
| Not told "running" after it ended | `the_next_thing_due_is_the_earliest_in_hand`, `every_kept_piece_of_a_jobs_output_is_fetched` | a wait on an ended job was due |
| A job never given to the supervisor isn't lost | `a_job_never_given_to_the_supervisor_is_not_lost` | it was recorded as lost |
| A job not sent by its limit is let go | `a_job_whose_limit_passes_before_it_is_sent_is_let_go` | no answer came |
| Nothing is started for a caller that went | `nothing_is_launched_for_a_caller_that_has_gone`, `a_caller_that_goes_leaves_its_started_job_running` | its job was still in hand |
| Idleness is counted from the last job | `an_idle_session_ends_at_its_idle_limit_counted_from_its_last_job` | "idleness was counted from before its last job ended" |
| Why a job was stopped is said | `a_stop_is_asked_once_and_says_how_the_job_came_out`, `a_job_past_its_limit_is_stopped_and_the_session_goes_on` | unsaid |
| No launch for callers that have all gone | `nothing_is_launched_for_a_caller_that_has_gone` | a launch went ahead |

What may be asked:

| Defence removed | Test that failed | How it failed |
|---|---|---|
| A limit is at most 600 seconds | `a_run_that_may_not_be_asked_says_why` | 601 was taken |
| A caller waits no longer than the limit | `a_run_starts_in_the_workspace_with_the_ordinary_limit` | a longer wait was kept |
| Input is at most 32 KiB | `a_run_that_may_not_be_asked_says_why` | more was taken |
| An ask fits one frame | same | one too long to send was taken |
| A wait asked of the spawner is bounded | `room_requests_reach_the_session_and_it_ends_with_the_broker` | a longer wait reached the session |

## What the runs found before this tree

Kept here because each changed the code or the tests.

- **A caller that waited exactly as long as its job's limit was told
  "running"** at the moment the job was being stopped. The installed test
  found it in the real Room. A job being stopped, or one that has ended
  while its output is fetched, is no longer said to be running; both have
  unit tests.
- **A full scratch would have refused every later job**, the one that
  clears it among them, because a job's output files couldn't be made.
  The files are now kept if they can be: the job runs, its head and tail
  are kept in memory, and its file is said to be cut.
- **The first mutation run, on an earlier tree, missed 17 mutants and
  timed out on 6 of 295.** None was a check on what the supervisor says.
  They were bounds no test pinned, timer arithmetic that only changed
  when the thread woke, one helper identical to its mutant, two loops a
  mutant could spin, and tests that waited on a session nothing ended.
  The bounds are now pinned, the loops are single steps, the helper is
  gone, the timers and idleness have tests of their own, and a test that
  fails closes its session instead of waiting for it.
- **One hand-break survived the first run of them**: a job's end said a
  second time while its output was being fetched. The check was there;
  no test reached it. One does now.

## What this change does not do

- **Give the agent a tool.** Nothing the model can call reaches the Room
  yet. That is the next change, with the tool's description tried on the
  default local model as part of its acceptance.
- **Stop what leaves a job's process group.** A process that starts a
  session of its own outlives its job's stop and its job's end. It is
  held by the Room's cgroup and ends with the Room.
- **Keep a job's output past its Room.** The files are in the session
  directory and go with it. The kept head and tail of the last 64 jobs
  stay with the spawner, in memory, until it restarts.
- **Survive a spawner restart.** The ledger is in memory: after a
  restart no earlier job is known by name.
- **Bound a job's memory or CPU by itself.** The limits are the Room's,
  shared by its jobs.
