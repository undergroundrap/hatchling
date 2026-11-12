# Evidence: the room disk closes where the root mount isn't shared

What was tested, how, and what came back, for the change that makes the
room disk close when Hatchling stops or locks on a machine whose root
mount isn't shared (WSL, and containers). Every number here is read from
a file in the results bundle, which is attached to the release that
carries this change.

| | |
|---|---|
| Base commit | `6d7f487f` (`fix(install): an install on a machine with no cgroup controllers works, and says what it is (#423)`) |
| Tested tree | `efa89af5a073a21bd1e9d6d40b0154001d032e67` |
| Date | 2026-11-12 (UTC) |
| Machines | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), systemd 259: a sandbox for the gate and the root runs, and a separate throwaway install for the drill |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-room-disk-mount-propagation-results.tar.gz`, 587394 bytes, sha256 `0e32ae5d6915f6414fc6f9441cfd9ebd0e48ebee909202bdd9360042f5938b3a` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

A program given a mount namespace of its own takes a copy of every mount
when it starts. The app's own host service is one. It is started while
the room disk is open, at install and again at every upgrade.

On most machines the root mount is shared, so taking the room disk down
takes its copies down with it. Where the root mount isn't shared (WSL,
and containers: systemd leaves the root as it found it), the copy
stayed. The room disk was unmounted in the machine's own namespace,
stayed mounted in the host's, and its encrypted mapping couldn't be
closed: "device is still in use".

What the owner saw: stopping or locking Hatchling left the room disk
open underneath, with its key in the kernel, and `vault-keyring setup`
and `enroll-totp` refused because they couldn't close it. Restarting the
app's host released it. The drill for the change before this one found
it, and its record says so.

## What changed

- Before the broker mounts the room disk, it makes sure the mount will
  come down everywhere (`hatchling_sys::share_below`). Where the room
  directory is on a shared mount, nothing is done. Where it isn't, the
  room directory is bound onto itself and that mount is shared, so the
  room disk is mounted under a shared mount all the same, and its
  unmount reaches every copy.
- After a close, the directory's own bind is taken down again
  (`hatchling_sys::unshare_below`). Only a bind of the directory onto
  itself is taken down, never another file system mounted there, and one
  in use stays.
- A room disk that can't be mounted after the bind was made takes the
  bind down again. A bind that can't be shared is taken down, and the
  open is refused.

`share_below` and `unshare_below` are new public functions of
`hatchling-sys`, additive in the unreleased 0.9. There is no new
setting and no change on the wire. The room disk's mount flags, and the
checks on them, are as they were.

## The drill, on a real install

A throwaway machine with systemd whose root mount is private (WSL2), a
fresh install each time by the documented own-build route against a
stand-in model, with the base's installer (this change doesn't touch
it). After each step the drill records where the room disk is mounted
(in the machine's own namespace, and in the app host's), how the room
directory itself is mounted, and which mappings the kernel holds with
how many holders.

The programs "before" are a build of tree
`6965888948b7f31df7afc9d2629efdacfa7c9a95`: the base's programs (the
base's tree differs from it in one test and the installer, and in no
program). The programs "after" are a build of the tested tree.

| Step | Before | After |
|---|---|---|
| Fresh install | room disk mounted here and in the app host's namespace; mapping held by 1 | the same, and the room directory is a shared mount of its own |
| `systemctl stop hatchling` | unmounted here, **still mounted in the app host's namespace; mapping still there, held by 1** | unmounted in both; **no mapping**; the room directory's bind gone |
| Start again | the old mapping is taken up again | a new mapping, under a new shared bind; the app host has no copy |
| Upgrade over it (restarts the app host while the disk is open), then stop | mapping still there, held by 1 | unmounted in both; no mapping |
| Three restarts | 1 mapping | 1 mapping, 1 mount at the room directory (binds don't stack) |
| `vault-keyring enroll-totp` on the upgraded install | **exit 64: "cryptsetup close failed with status 5: Device ... is still in use"** | exit 0; the vault is at the TOTP tier with 10 backup codes; unlocked with a code |
| `hatchling-broker lock` | not reached: the vault isn't protected | exit 0, "Locked ... its room disk closed"; unmounted in both; no mapping |
| `hatchling-broker unlock` | not reached | exit 0; mounted again |

The way an owner gets this change was drilled too: an install of the
programs as they were, upgraded to the tested ones. The upgrade itself
exits 0 with the room disk mounted under a shared bind; the stop that
follows leaves no mapping and no mount in either namespace; the
enrollment, the lock and the unlock that follow each exit 0, and the
lock leaves no mapping.

The drill's console log is `drill/console-mp.log` in the bundle, sha256
`9ed1aa51a0604d281ecb30f1b44088641fef094a925542d2efced36dca7eaf92`, with
what each command said beside it. The install logs are left out (they
hold the throwaway install's app address), the app's token is masked,
and the authenticator's seed and the backup codes were masked where
they were captured. The drill removes its install when it's done: no
mapping, loop device or mount was left.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3719 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3719 | 0 | 0 |

`gate.log` sha256
`f3ac7373e51e733dc1971d168afd26690cb33996669859f5eded9fa2beaad605`.

The broker's library tests were then run 15 times over on 1.98.1, every
test in parallel: 15 runs, 1360 passed and 0 failed in each. `stress.log`
sha256 `a6fe2fa3e36a1b9e57de2e3c063c0b6361ef25e057eaa217b4dc09b1ca93c179`.

Tests that mount say they are skipped in the sandbox, which may not
mount. They were run as root, below.

## As root, with real mounts

Outside the sandbox's jail, as root, on a clone of the tested tree, on
1.98.1. A mount namespace made with `unshare --mount --propagation
slave` stands for a hardened service's, and a private bind stands for a
root mount that isn't shared.

| Run | Result |
|---|---|
| `hatchling-sys`'s real-mount tests (`--test locked_mount`, with `HATCHLING_REQUIRE_ROOT_MOUNTS=1`) | 4 passed, 0 failed. One of them shows the fault first (without sharing, the copy stays after the unmount), then the fix |
| The broker's real room disk (`--test room_disk`, with `HATCHLING_REQUIRE_ROOM_DISK=1`: a real keeper, LUKS2 and ext4) | 7 passed, 0 failed. The new one starts a service while the room disk is open, closes the disk, and finds no mapping and no bind left |
| The two installed Room tests, with a static supervisor built from the tested tree (sha256 `9438412f60dfd3092e542b373ce367ea5ac99e0e03a75c706cb71542f43d0f78`) | 2 passed, 0 failed: the Room still launches from, and is taken down before, a room disk mounted this way |

The real-mount runs, and the mutation run as root below, left no mount
and no new mapping (`root-breaks.log`, `root/root-mut.log`).

## Mutation

Every mutant on the lines changed against the base, on 1.98.1 with debug
info off, one run per crate, in the sandbox:

```text
cargo mutants --in-place --in-diff <the crate's diff> --timeout 60 -- --lib
```

| Crate | Mutants | Caught | Missed | Timed out | Unviable | `outcomes.json` sha256 |
|---|---|---|---|---|---|---|
| `hatchling-sys` | 30 | 22 | 6 | 0 | 2 | `804a59cc1105254c03ddcf46f507aa5b9ddb5312107b0b7a6963c6241b5c6782` |
| `hatchling-broker` | 2 | 2 | 0 | 0 | 0 | `f31f06c0ef6e1b728a7008b9138e1a929af10f0d9fec09b1505db03dee0e1f6f` |

The six missed there are all in what asks the kernel itself, which no
test in the sandbox may do:

- `locked_mount.rs:333:9: replace <impl Mounts for Kernel>::is_point -> Result<bool, Error> with Ok(false)`
- `locked_mount.rs:337:9: replace <impl Mounts for Kernel>::bind -> Result<(), Error> with Ok(())`
- `locked_mount.rs:341:9: replace <impl Mounts for Kernel>::share -> Result<(), Error> with Ok(())`
- `locked_mount.rs:346:9: replace <impl Mounts for Kernel>::unmount -> Result<bool, Error> with Ok(true)`
- `locked_mount.rs:346:9: replace <impl Mounts for Kernel>::unmount -> Result<bool, Error> with Ok(false)`
- `locked_mount.rs:406:5: replace unshare_below -> Result<bool, Error> with Ok(false)`

So `hatchling-sys`'s mutants were run again as root, where a mount can
be made, against the crate's mount tests in its library and its
real-mount test together:

```text
cargo mutants --in-place --in-diff <the crate's diff> --timeout 60 -- --lib --test locked_mount -- locked_mount:: a_mount_ a_locked_down_mount a_shared_mount
```

| Crate, as root | Mutants | Caught | Missed | Timed out | Unviable | `outcomes.json` sha256 |
|---|---|---|---|---|---|---|
| `hatchling-sys` | 30 | 28 | 0 | 0 | 2 | `83d833944a46d83930379725c45b799d1bc4295ed1962c11108d61725e0a1033` |

All six are caught there. The library's other tests are left out of
that run by name: its cgroup tests need a cgroup of their own as root,
and nothing mutated is theirs. No mutant was excluded, so there is no
equivalence proof to give.

Unviable, in both runs:

- `crates/hatchling-sys/src/locked_mount.rs:259:5: replace mounted_in -> Option<Mounted> with Some(Default::default())`:
  unviable, build failure.
- `crates/hatchling-sys/src/locked_mount.rs:312:9: replace <impl Mounts for Kernel>::mounted -> Result<Mounted, Error> with Ok(Default::default())`:
  unviable, build failure.

## Defences broken by hand

Each defence was removed by hand, one at a time, and the tests that guard
it were run and their failures read. The unbroken tests passed before and
after.

In the sandbox, against a kernel stood in for by a record of what was
asked of it:

| Defence removed | Test that failed | How it failed |
|---|---|---|
| A directory on a mount that isn't shared is bound and shared (the bug) | `a_directory_on_a_mount_that_is_not_shared_is_bound_and_shared` | nothing was done for a private mount |
| A mount that's shared already is left as it is | same | a shared mount was bound over |
| A mount point that's there isn't bound over again | same | asked for a bind and a share, where a share was all |
| A bind that can't be shared is taken down again | `a_bind_that_cannot_be_shared_is_taken_down_again` | the bind was left: no unmount asked |
| Only a bind made here is taken down when sharing fails | same | a mount that was there before was unmounted |
| A share the kernel didn't keep is refused | same | taken as shared |
| Only a directory's own bind is taken down | `only_a_directorys_own_bind_is_taken_down` | another mount at the directory was taken down |
| A directory with nothing mounted on it is left | same | an unmount was asked of a plain directory |
| Another file system at the directory isn't its bind | `a_directory_bound_onto_itself_is_told_from_any_other_mount` | a disk mounted there was taken for the bind |
| Another directory bound there isn't its bind | same | taken for the bind |
| Propagation is read from a mount's own fields only | `a_mount_is_read_from_its_own_line` | a file system's own "shared:" text was read as the mount's |

As root, with real mounts:

| Defence removed | Test that failed | How it failed |
|---|---|---|
| The room disk's directory is shared before the mount (the bug, at the room disk) | `a_service_started_while_the_room_disk_is_open_does_not_keep_it_open` | "cryptsetup close failed with status 5: Device ... is still in use" |
| The room directory's bind is taken down after a close | same | "the room directory's own bind was left after the close" |
| The kernel is asked to share the mount | `a_mount_comes_down_in_a_namespace_made_after_it` | "still isn't a shared mount" |
| It's asked for shared, not private | same | "still isn't a shared mount" |
| The kernel is asked to bind the directory | same | sharing a directory that is no mount point: invalid argument |
| The bind is really taken down | same | the directory was still a mount point after |

`unit-breaks.log` sha256
`1129c9c603d74c1bc7fb823a7bca4e36061489802b28f5acae10ae0f69424849`;
`root-breaks.log` sha256
`44a6522bf52dc3d74687a4ea4ce032f74af4702c631c4801a489a71f3e683161`.

## What this change does not do

- **Reach a copy taken before it was installed.** An install upgraded
  from an earlier build still has the old copy in its app host until the
  host is restarted. The installer restarts it in the same upgrade, and
  the drill's last scenario is that path. Another service of the
  owner's, started with a mount namespace of its own while an earlier
  build had the room disk open, keeps its copy until it is restarted.
- **Change what a Room holds.** A Room's own namespace has the room disk
  for as long as the Room lives. Every Room is taken down before the
  disk is closed, and that order is as it was.
- **A drill on a machine whose root mount is shared, or in a container.**
  On a shared root the change does nothing, which the tests show (a
  shared mount is left as it is, and nothing is mounted). A container
  wasn't drilled: the root tests make a private mount to stand for one,
  and the drill machine's root is private for the same reason a
  container's is.
