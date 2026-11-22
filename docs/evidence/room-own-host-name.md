# Evidence: the Room has its own host name and cgroup root (#572)

What was tested, how, and what came back, for
[#572](https://github.com/undergroundrap/hatchling/issues/572), found by
the v0.9.0 release security checklist (item 6: the cell's surface) and
seen in the 2026-11-21 install drill ([real-install-drill.md](real-install-drill.md)).

| | |
|---|---|
| Base | `main` at `40a004cf` |
| Tested tree | `9ee2080f` |
| Date | 2026-11-22 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox; the Room's test as real root |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned) |

This record was added after the runs, in its own commit. It changes no code.

## The problem

The Room was started with `unshare --pid --fork --kill-child --net --ipc`,
and its own mount namespace. It shared the host's UTS and cgroup
namespaces: `uname` gave the agent, and a hosted model reading what the
agent saw, the owner's machine name, and `/proc/self/cgroup` the path of
the Room's cgroup in the host's tree. The admission probe proved the
network namespace by its inode alone, and checked one descriptor closed,
not all.

## What changed

- The Room's helper is started with `--uts` and `--cgroup` too. As root
  in its new UTS namespace, before anything of the Room runs, it names
  the host `hatchling-room` (`hatchling_sys::set_host_name`, through
  rustix's `system` feature, no new crate). It joins its cgroup before
  `unshare` runs, so that cgroup is the root of what the Room sees.
- The admission probe, which stands in for the supervisor in the real
  Room, now refuses to start unless:
  - its UTS and cgroup namespaces aren't the test's (by inode), and its
    host name is `hatchling-room`;
  - `/proc/self/cgroup` is `0::/`;
  - loopback is its only interface (`/proc/self/net/dev`), and an
    `AF_INET`, `AF_INET6` or `AF_NETLINK` socket is refused with
    `EAFNOSUPPORT`, as the Room's filter says;
  - no descriptor above 3 is open but the one it reads them through.
- [The trust boundary](../arch/trust-boundary.md) names the Room's
  namespaces.

## Results

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.85.0 and 1.98.1 | clean |
| `hatchling-sys` lib tests, 1.85.0 and 1.98.1 | 318 passed on each |
| The broker's `room_launch::` and `room_disk::` lib tests, 1.85.0 and 1.98.1 | 57 passed on each |
| `hatchling-broker`'s tests, lib and integration, with the workspace built, 1.98.1 | 2,076 passed, 0 failed |
| `the_installed_spawner_hands_the_verified_image_to_its_mount_helper`, as real root, the real spawner, `unshare`, LUKS2, dm-verity and the probe | passed, before and after the breaks |

The gate's rows ran at tree `c2db60f2`, which differs from the tested
tree only in the probe's C source; the root test ran at the tested tree.

### Hand-breaks, as root

Each was applied, the workspace built, the Room's test run as root, then
undone. 3 broken, 3 caught, each for its own reason:

| Break | Caught by | The probe's status |
|---|---|---|
| The host name not set | `the_installed_spawner_hands_the_verified_image_to_its_mount_helper` | 52: the host name isn't `hatchling-room` |
| No `--cgroup` | the same | 53: the cgroup namespace is the host's |
| No `--uts` | the same | 51: the UTS namespace is the host's |

Without `--uts`, the helper's `sethostname` named the test machine's own
distro, which is why that break ran last and the runner put the name
back after each run. In the product, `unshare` fails rather than start
the helper without the namespaces it was asked for.

Mutation: the one mutant the change has, `set_host_name` returning
`Ok(())`, is the first break above, caught only as root; the jail's run
can't start a Room.

## Residual

- The probe stands in for the supervisor; the real supervisor runs in
  the same namespaces, since they are made before either starts.
