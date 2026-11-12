# Evidence: the room disk's test stand-ins are settled before use

What was tested, how, and what came back, for a change to tests only: the
stand-in programs the room disk's unit tests write are run once before
they are used, so a test no longer fails now and then with "text file
busy". Every number here is read from a file in the results bundle, which
is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `76427c84` (`feat(room): run a command in the Room as a named job (#425)`) |
| Tested tree | `3545293b6fcb020614be7cfd510a7922039caec5` |
| Date | 2026-11-12 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-room-disk-stand-in-settle-results.tar.gz`, 258793 bytes, sha256 `fc3be61f07dd3b211e003ce0b14d76bb5eef789dfbcd812412e46c76796c89dc` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

The room disk's unit tests write two stand-in programs, a `cryptsetup` and
an `mke2fs`, and the code under test runs them. The broker's library tests
run in parallel in one process, and many of them start programs of their
own. Starting a program forks, and the forked child holds a copy of every
descriptor its parent had open at that moment until it goes on to its own
program. If one test forked while another had a stand-in open for
writing, the child held that stand-in open for writing for a moment after
the file was closed. For as long as anything holds a file open for
writing the kernel refuses to run it: "text file busy".

It was seen once: one run in fifteen of the broker's library tests, in the
stress run for an earlier change, failed
`room_disk::tests::a_room_disk_is_made_only_where_there_is_none` that way.
No product code was at fault, and none is changed.

## What changed

- After a stand-in is written it is run once, told to do nothing, and the
  run is tried again for as long as the kernel says the file is busy.
  Once the file has started once nothing holds it open for writing, and
  nothing can come to: the test has closed it.
- A stand-in told so leaves at its first line, before it records
  anything, so the tests see no run of it.
- A new test shows the fault and the wait without depending on timing
  between tests: it holds a stand-in open for writing, sees that it can't
  be run, and sees that settling returns only once the file is let go.

Only `crates/hatchling-broker/src/room_disk/tests.rs` changes.

## The fault, by hand

The wait was taken out by hand and the room disk's tests run; the
unbroken tests passed before and after.

| Defence removed | Test that failed | How it failed |
|---|---|---|
| A busy stand-in is waited for | `a_stand_in_is_settled_before_it_is_used` | `ExecutableFileBusy`, "Text file busy" |

`unit-breaks.log` sha256
`feb5563d2f6c0cb14ac9d3200e9a213a1f24abee1b7d069c67f48b138700fa20`.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3750 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3750 | 0 | 0 |

`gate.log` sha256
`884006bd9de8420865ff9e9090c4e456e8d833506ee0b02c275b8ec65a6a4c4f`.

That is the gate's second run on this tree. Its first run failed one test
on 1.85.0 and none on 1.98.1: `a_child_that_sends_a_held_secret_ends_the_whole_tree`,
in the broker's subagent tests, with "the parent's session is over, and
answered nothing". Nothing this change touches is in that test or in
what it runs. Run by themselves the subagent tests then passed 60 times
out of 60, and the gate was run again whole. The first run is kept in the
bundle as `gate-first.log` and `gate-first-1.85.0-test.log` (sha256
`83e3999cbd29aa3af8eb792454b4a780b8a58192fead6c86d3b5d38f2bb207e4` and
`66418b8572cc32a5f3ce7df16842692fef35372361cada1498fb323d5e5a0aab`).

The broker's library tests were then run 40 times over on 1.98.1, every
test in parallel: 40 runs, 1380 passed and 0 failed in each. `stress.log`
sha256 `aed1541dfe107656ff58573c575874d33b06250d957a5ff15eed3ac0f6e7fb2e`.

## Mutation

The change is in a module that exists only for tests, so there is nothing
to mutate: the tool finds no mutant in the changed lines
(`gate-mutants-broker.txt`).

## What this change does not do

- **Prove the flake gone.** It was seen once in fifteen runs, and forty
  clean runs don't prove a thing that rare is gone. What is proved is the
  cause: the new test makes the fault happen every time, and the wait
  answers it. Other tests in the workspace that write a program and run
  it weren't looked at.
- **Explain the subagent test that failed once in the gate.** It is
  another test, with another cause, and it isn't changed here. Its
  assertion doesn't say what came back in place of the end it expects, so
  one failure can't settle why. It is noted for a change of its own.
