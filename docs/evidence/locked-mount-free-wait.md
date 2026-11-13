# Evidence: a mount test waits for the mount to be free

What was tested, how, and what came back, for a change to tests only: a
test of the locked-down mount waits for the mount to be free before it
takes it down, so it no longer fails now and then. Every number here is
read from a file in the results bundle, which is attached to the release
that carries this change.

| | |
|---|---|
| Base commit | `8079e410` (`feat(agent): run a program in the Room from a chat (#431)`), with the change before this one, `test(room): give the image proof the Room's /proc`, and its record |
| Tested tree | `66f7345b4a6fc852f9a993c694152951d7bde348` |
| Date | 2026-11-13 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-locked-mount-free-wait-results.tar.gz`, 173411 bytes, sha256 `1f94b2b9a269e3cfa1f9d1101fe2ca4a7af7ba2a86bc470ac153c256e7d0a0b4` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

`a_locked_down_mount_holds_and_comes_down_only_when_free`, in
`crates/hatchling-sys/tests/locked_mount.rs`, opens a file in a mount,
sees that the mount can't be taken down while it is open, closes the
file, and at once takes the mount down. A mount is in use until every
copy of a file open in it is closed. The file's four tests run in
parallel in one process, and another of them starts a program, twice.
Starting a program forks, and the forked child has a copy of every file its parent
had open until it goes on to its own program. If another test forked at
that moment, the mount was still in use, the unmount was refused
("Device or resource busy"), and the test failed.

It was seen in CI: the root pass of the pull request for the Room's run
tool failed in this test, on a commit that changed only an evidence
record, and passed when run again. No product code was at fault, and
none is changed: `unmount_strict` refuses a mount in use, which is what
it is for.

## The fault, seen here

As root, outside the sandbox's jail, on two processors (`taskset -c
0,1`), since a CI runner has few. 400 runs of the test file each:

| The tests | How run | Failed |
|---|---|---|
| As they were | All four, in parallel | 9 of 400 |
| As they were | One at a time (`--test-threads=1`) | 0 of 400 |
| As they were | This one test alone | 0 of 400 |
| As they are | All four, in parallel | 0 of 400 |

All nine failures are the one seen in CI, at the same line: `Mount {
operation: "unmount", source: Os { code: 16, kind: ResourceBusy } }`.
The test never failed without another test running beside it.

`root/stress-as-it-was.log` sha256
`1de12d53f872bbce9a702cf47e450718eb3ad202fcc67dbd56c5ce1c17816ff7`;
`root/stress-as-it-is.log` sha256
`01fb78c0fedcbdc5876156406d68b88b975f01f0bd09fa6b7a63d4d615aad4e1`;
`root/cause-one-at-a-time.log` sha256
`e13f04cb53885da318fe165830a65180e7441758083175544065025303fb9a7f`;
`root/cause-alone.log` sha256
`c15efb81f3661622ef39a7a47818b490494dfb1618e3a442af9e5b83b11edc4a`.

## What changed

Only `crates/hatchling-sys/tests/locked_mount.rs`:

- After closing the file, the test takes the mount down with
  `unmount_strict` as before, and tries again for up to ten seconds
  while the answer is that it is in use. Any other refusal, and one
  still in use after ten seconds, is the failure it was.
- The test now makes the fault happen every time, without depending on
  what other tests do: it keeps a copy of the open file, sees that the
  mount still can't be taken down, lets the copy go from another thread
  a moment later, and sees that the mount comes down only once it has.

## The fault, by hand

The wait was made to give up at once and the tests run as root; the
unbroken tests passed before and after (4 passed, 0 failed).

| Part taken out | Test that failed | How it failed |
|---|---|---|
| The wait for the mount to be free | `a_locked_down_mount_holds_and_comes_down_only_when_free` | The unmount refused as busy, as in CI: `code: 16, kind: ResourceBusy` |

`root/break-no-wait.log` sha256
`5427419a4999e6b9df52a1271d42d0e743f8893396ddc63581fa923a0f72be2b`.

## Mutation

None to run. The change is to a test, and
`cargo mutants --list --in-diff` on the changed lines says so: "No
mutants to filter" (`gate-mutants-sys.txt`).

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3780 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3780 | 0 | 0 |

`gate.log` sha256
`1a3e3361d9240cbdd4206651721e19397eb31bb0c1c612fcba8492e0349fe874`.

Without root these four tests say they are skipped; the runs that count
are the ones as root above.

The first tree of this change failed the gate's clippy on both
toolchains: two names in the test were too alike, and a path was spelled
out where it didn't need to be. Both were changed, and every run in this
record is of the tree after that.

## What this change does not do

- **Change `unmount_strict`.** It still refuses a mount in use at once.
- **Stop other tests forking.** A test that closes something and then
  needs it gone has to wait for it, here as in the socket test before.
