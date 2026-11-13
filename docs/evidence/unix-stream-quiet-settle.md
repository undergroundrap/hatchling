# Evidence: a socket test waits for the close it checks

What was tested, how, and what came back, for a change to tests only: a
test of `unix_stream_quiet` waits for the other end's close to arrive
before it checks for it, so it no longer fails now and then. Every number
here is read from a file in the results bundle, which is attached to the
release that carries this change.

| | |
|---|---|
| Base commit | `99741e0b` (`fix(room): let the Room's own programs run under its filter (#429)`) |
| Tested tree | `b5eea791e3ec0ea7486aafb20f93f8f73ec9294a` |
| Date | 2026-11-13 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-unix-stream-quiet-settle-results.tar.gz`, 178138 bytes, sha256 `7a7a0f8043ba9ead0182fbea6c939bee1b8098058edafb2b0c804e9453f3efe8` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

`net::tests::unix_stream_quiet_distinguishes_empty_input_data_and_eof`
makes a pair of connected sockets, closes one end, and at once expects
the other end to show the close. The close shows only when every copy of
the closed end is gone. The `hatchling-sys` library tests run in parallel
in one process, and many start programs of their own. Starting a program
forks, and the forked child holds a copy of every descriptor its parent
had open until it goes on to its own program. If another test forked at
that moment, the other end's close had not yet arrived, the socket was
still quiet, and the test failed.

It was seen once: one run in 25 of the library's tests, in a stress run
for an earlier change (`seen-stress.log`, `seen-stress-failed-10.log`).
The two other stress runs of 25 made for that change were clean. No
product code was at fault, and none is changed: `unix_stream_quiet` says
what is so at the moment it is asked.

## What changed

Only `crates/hatchling-sys/src/net.rs`, in its tests:

- After closing the one end, the test waits for the other to have
  something to show, for up to ten seconds, and then checks.
- A new test, `a_close_shows_only_when_the_last_copy_of_the_other_end_is_closed`,
  makes the fault happen every time and without depending on what other
  tests do: it keeps a copy of the closed end, sees that the socket is
  still quiet, lets the copy go from another thread a moment later, and
  sees that the wait ends only once it has.

## The fault, by hand

The wait was made to return at once and the tests of `net` run; the
unbroken tests passed before and after.

| Part taken out | Test that failed | How it failed |
|---|---|---|
| The wait for something to show | `a_close_shows_only_when_the_last_copy_of_the_other_end_is_closed` | "the wait ended with the copy still open" |

`unit-breaks.log` sha256 `daf0b3e78023929b03d0eac5c185ed10fff5004c546856cab292cd98ef573bfd`.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3755 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3755 | 0 | 0 |

`gate.log` sha256 `0ab9951a523f73c474b7ea1a2928872b1e3f4cde66183a8e68efbc3c0ef66bbc`.

The first tree of this change failed the gate's tests on both toolchains,
in the new test, though the test passed run with its own module alone.
Its wait was ended early by a signal meant for another test's work
("Interrupted system call"). The wait now takes itself up again when
that happens. That run's log was not kept.

The `hatchling-sys` library tests were then run 100 times over on 1.98.1,
every test in parallel: 295 passed and 0 failed in each.
`stress.log` sha256 `a6837f5bea1af253a8fe840c2417f405195e59298a2292008a8b9e90aabd6fa6`.

## Mutation

The change is in a module that exists only for tests, so there is nothing
to mutate: the tool finds no mutant in the changed lines
(`gate-mutants-sys.txt`).

## What this change does not do

- **Prove the flake gone.** It was seen once in 75 runs, and a hundred
  clean runs don't prove a thing that rare is gone. What is shown is the
  cause: the new test makes the fault happen every time, and the wait
  answers it.
- **Look at other tests of the same shape.** A test anywhere in the
  workspace that closes a descriptor and expects the other side to see
  it at once has the same fault. They weren't looked for. The subagent
  test that failed once in an earlier gate is another test with, it
  seems, a like cause, and it isn't changed here.
