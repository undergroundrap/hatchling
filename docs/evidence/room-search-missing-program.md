# Evidence: the Room's supervisor answers a search whose program can't start (#578)

What was found, what changed, and how it was tested, for
[#578](https://github.com/undergroundrap/hatchling/issues/578), found while
testing #573 for the v0.9.0 release checklist.

| | |
|---|---|
| Base | `main` at `8de0e034` (v0.9.0) |
| Date | 2026-11-22 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, glibc 2.43, no `/usr/bin/rg`), toolchains 1.98.1 and 1.85.0 |

This record was added after the change, in its own commit.

## The problem

On a machine without `/usr/bin/rg`, the supervisor's proof under the
Room's real filter (`crates/hatchling-room/tests/room_supervisor.rs`)
expects a search to be answered `NoSearch::Failed`. Instead the
supervisor's connection closed (`peer closed the connection`).

Under `strace -f`, the supervisor (pid 931) started the search program
with `clone(CLONE_VM|CLONE_VFORK)`, the child's `execve("/usr/bin/rg")`
failed, and the supervisor then called `waitid(P_PID, 935, NULL,
WEXITED)`: glibc's `posix_spawn` reaping the child that failed. The
supervisor was `killed by SIGSYS`; strace shows the killed call's number,
247, as its result, and 247 is `waitid`
(`/usr/include/x86_64-linux-gnu/asm/unistd_64.h`). The Room's filter
allowed `wait4` and not `waitid`. With `rg` there, the child is never
reaped that way, so the image and CI never met it.

## The threat, and the change

What `waitid` could give a job in the Room: it waits for and reaps a
child of the caller's own, as `wait4` does, which the Room already
allows. It can't wait for another process's child, and through a pidfd
only for a child's (`pidfd_open` is already allowed). It reaches no
process `wait4` doesn't. The browser's profile allows it on the same
reasoning.

- `crates/hatchling-sys/src/seccomp.rs`: `waitid` joins `ROOM_TOOL_CALLS`,
  with what it reaches and why it's there. The pinned list and numbers,
  and the browser's six of the Room's calls, are updated.
- The Room's tools probe (`the_rooms_tools_run_under_its_profile`), under
  the Room's profile, now starts a program that isn't there, as the
  supervisor starts its search program: it must fail with `NotFound` and
  the probe go on. It runs wherever the tests run, `rg` or not.
- `docs/arch/trust-boundary.md`: twenty-one calls allowed, `waitid` for
  `wait4` among them.

## Tests

| Check | 1.98.1 | 1.85.0 |
|---|---|---|
| `cargo fmt --all --check` | pass | |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass | pass |
| `hatchling-sys --lib seccomp::` | 29 passed | 29 passed |
| The supervisor's proof, no `rg` | before: the connection closed; after: *three supervisor proofs passed under the room filter* | after: passed |

**Hand-break:** with `waitid` taken back out of the Room's calls, the
tools probe is killed (`signal: 31 (SIGSYS)`) and the supervisor's proof
fails as before; both pass with it in.
