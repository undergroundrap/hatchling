# Evidence: four of the broker's tests hold on a busy machine

What was tested, how, and what came back, for a change to tests only:
four tests of the broker that failed now and then, or could, no longer
depend on how its threads happen to run. Every number here is read from
a file in the results bundle, which is attached to the release that
carries this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `d704aea0` (`feat(room): hand back a page of a file from the Room (#436)`) |
| Tested tree | `8d7fa7f72e73ee7fcb0363948fc9fadf1ec5ec02` |
| Date | 2026-11-13 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-broker-flaky-waits-results.tar.gz`, 174410 bytes, sha256 `dbb98d8c107449d66057606d26de26f271dd7bf17c629c51697351a6681d9d66` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

Two of these tests failed while the change before this one was being
tested, each once or twice in dozens of runs, in tests that change
didn't touch. Running the session's tests many times over found a
third. The fourth has the first one's fault, by reading it.

- `room_session::tests::a_job_that_does_not_end_when_stopped_ends_the_session`
  checks that a session waits out a stopped job's grace. It started its
  clock when it heard the supervisor asked to stop the job. The session
  starts its own when it takes the stop in, which is earlier. A test
  thread held up for a moment between the two read a wait a little
  shorter than the grace, and failed with "its grace wasn't waited
  out".
- `room_session::tests::an_idle_session_ends_at_its_idle_limit_counted_from_its_last_job`,
  in its second half, started its clock after it had said a job ended.
  The session's idleness starts when it hears that, which may be before
  the test has read its clock. This is the same fault as the first.
- `room_session::tests::a_job_the_supervisor_does_not_start_is_let_go_with_why`
  asked for a job, asked after it, and had its stand-in supervisor
  refuse the job as soon as it was asked to start it. A session that
  heard the refusal before it had taken the second ask in had no such
  job by then, and said "there is no Room job by that name" where the
  test expected why the job wasn't started.
- `spawner::tests::the_spawner_refuses_a_spec_its_helper_does_not_start`
  dropped one end of a socket and expected the other end to see it gone
  at once. Tests run side by side in one process, and a child that
  another test has forked holds a copy of that end until it goes on to
  its own program.

No product code was at fault, and none is changed.

## What changed

Only `crates/hatchling-broker/src/room_session/tests.rs` and the tests
in `crates/hatchling-broker/src/spawner.rs`:

- The first two start their clock before they set the session going, so
  what they read is never shorter than the wait.
- The third asks for the job and asks after it before its supervisor is
  ready. A session asks its supervisor nothing until it is ready, and
  takes in all that was asked of it first, so the second ask is always
  waiting when the job is let go.
- The fourth shuts the socket before dropping it, which every copy of
  it sees.

## The faults, seen here

The broker's library tests were built twice from the tested tree: as
they are, and with the two files as they were at the base commit. Each
build ran the session's tests (`room_session::tests::`, 57 tests) 300
times, then the spawner's (`spawner::tests::`, 39 tests) 300 times, the
old build and the new side by side, on all 28 processors, in the
sandbox.

| Tests | Build | Runs | Runs that failed | Which |
|---|---|---|---|---|
| The session's | as they were | 300 | 31 | 27 in the stopped job's grace ("its grace wasn't waited out"); 4 in the job that isn't started ("there is no Room job by that name") |
| The session's | as they are | 300 | 0 | |
| The spawner's | as they were | 300 | 1 | `the_spawner_refuses_a_spec_its_helper_does_not_start`, at the assertion that failed in the gate where it was first seen |
| The spawner's | as they are | 300 | 0 | |

The runs as they were are this change taken out, so no part is taken
out by hand besides.

`stress.log` sha256
`ae9cf7f13ebc38f4c1bb1216e652ec8d90ec040fdb77ad96b838673d13ce7e09`;
`stress-session-old.log` sha256
`72fef1d5ca7d10df256eb3c8f01d63db4557a50847be058273e3bbbe5fb0828b`;
`stress-session-new.log` sha256
`09faedd44fc307fe23358c6a9b737a087a2f45d208d725c9678cf554783d724a`;
`stress-spawner-old.log` sha256
`213b5a0a28a0a44f1393917b4f6d260f29800df51e0f427558ec2824ad83a54d`;
`stress-spawner-new.log` sha256
`51dd89a4bb77855639be557bc9c7dd686d3556d6e42efd5390a6f03b08fa0402`.

### How the third was found

A first stress of the session's tests, of an earlier tree
(`cf7fdcd8d78b04c26b1a130952ef8ab1f43aff76`) that changed only the
other three tests, was stopped before its 300 runs were done; how many
were done isn't in its logs. By then 14 runs of the old build had
failed, and 2 of the new: both in
`a_job_the_supervisor_does_not_start_is_let_go_with_why`, which that
tree had not changed. Its logs are in the bundle under `first/`.

Before that, and not in the bundle, the same stress with each build
held to one processor ran some ninety times each with no failure at
all. These faults need threads that really run at once.

## What is not shown

- **The idle session test failing.** Its old form did not fail in any
  run here. Its change rests on reading it: it reads its clock after
  the session may have started the wait, as the first test did.
- **That no other test of the broker fails now and then.** Only the
  session's and the spawner's tests were run many times over.

## Mutation

None to run. The change is to tests, and
`cargo mutants --list --in-diff` on the changed lines says so: "No
mutants to filter" (`gate-mutants-broker.txt`).

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3794 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3794 | 0 | 0 |

`gate.log` sha256
`5c5f0af899cf30b64576fb5a760b933c75faaeb48d04781d774b93f1b9f427a3`.
