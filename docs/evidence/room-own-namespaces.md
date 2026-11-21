# Evidence: a test fails if the Room shares the host's network or IPC namespace (WO-034)

What was tested, how, and what came back, for an open item of
[WO-034](../workorders/workorder-034-run-commands-in-the-agents-encrypted-room.md):
no test asserted a live Room's own network and IPC namespaces, so taking
`--net` or `--ipc` off the Room's `unshare` would have failed nothing.

| | |
|---|---|
| Base | `main` at `3a5c4445` |
| Tested tree | `67674026` |
| Date | 2026-11-21 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox; the root test as real root in that distro |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned) |

This record was added after the runs, in its own commit. It changes no code.

## What changed

The installed spawner's test (`the_installed_spawner_hands_the_verified_image_to_its_mount_helper`,
in `crates/hatchling-broker/tests/room_disk.rs`, run as root in CI's
`cells as root` job) builds a probe that runs as the Room's first
process. The test now passes the probe its own network and IPC
namespaces' inodes, the host's (`HOST_NET_NS`, `HOST_IPC_NS`). The probe
stats `/proc/self/ns/net` and `/proc/self/ns/ipc` and exits 49 or 50 if
either is the host's, so the Room never says it's ready.

The Room has no network because it has a network namespace of its own,
with no interface in it, and because its syscall filter refuses sockets.
The second was tested; now the first is too.

## Results

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | clean |
| The test, as real root, the release's way (a cgroup of its own, the Room's development supervisor built from this tree) | passed, before and after the breaks |

### Hand-breaks

Each was applied to `crates/hatchling-broker/src/room_launch.rs`, the
broker built, the test run as root, then undone:

| Break | Caught by |
|---|---|
| The Room started without `--net` | the test: *the Room supervisor exited with status 49* |
| The Room started without `--ipc` | the test: *the Room supervisor exited with status 50* |

2 broken, 2 caught, each by the probe's own check (its exit code in the
mount helper's words), not by another failure. The test passed before
and after.

## Residual

- The drill on a real install
  ([real-install-drill.md](real-install-drill.md)) saw the same in a
  release build's Room: its network, IPC, PID and mount namespaces its
  own. This test guards it from here on.
