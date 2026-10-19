# WO-014: Run the full broker without root

**Status:** DONE
**Area:** `crates/hatchling-isolation` (id maps, spawn), `crates/hatchling-sys` (id maps), `crates/hatchling-broker` (config, API, ingress), `docs/`, `README.md`
**Issue:** [#23](https://github.com/undergroundrap/hatchling/issues/23)
**Trust boundary:** yes

## Problem

The broker runs as root so that each cell can get a uid of its own. Without privilege, Linux lets a process map only its own uid into a user namespace, so an unprivileged broker's cells run as the owner's uid. They're still isolated (their own namespaces, seccomp, no capabilities, no network), but the broker admits callers by uid, so a cell with the owner's uid could drive the local API and the chat ingress. That's why both refuse to start without root today.

So the full product needs root. Many security teams won't run a third-party daemon as root, and a solo user shouldn't have to. A root broker is also the largest target on the machine: if it is ever compromised, everything is.

## Scope

**In scope:**

- **Cells get a distinct uid without root**, from the owner's subordinate uid range (`/etc/subuid` and `/etc/subgid`), written through the setuid helpers `newuidmap` and `newgidmap`, which is how rootless containers do it.
- **The local API and the chat ingress start under an unprivileged broker** when its cells have a distinct uid, and keep refusing when they don't, with a message that says how to configure a subordinate range.
- **Every cell kind** (agent, scheduled run, skill, harness CLI) runs under the unprivileged broker with its distinct uid.
- **The broker refuses an unsafe map:** a range that overlaps the owner's uid, uid 0, or another account's uid; a missing helper; or helpers that aren't setuid root and owned by root.
- **A rootless quickstart** in the README, next to the root one, and the "Running without root" section rewritten to match.

**Not in scope:**

- rootless operation without a subordinate range (it stays the reduced mode it is today);
- changing a root broker's behavior;
- per-cell uid ranges (one distinct uid per cell kind is enough here);
- cgroup limits for rootless cells (WO-004 covers limits).

## Acceptance criteria

- [x] An unprivileged broker with a subordinate range runs each cell kind as a uid that isn't the owner's, shown by the cell's `/proc/self/status` and `uid_map`; tested with real cells.
- [x] Under that broker, the local API and the chat ingress start, and the API refuses a connection from a cell (its uid isn't a caller's); tested.
- [x] Without a subordinate range, the API and the ingress still refuse to start, and the message names `/etc/subuid`; tested.
- [x] Each unsafe map listed in the scope is refused before any cell spawns; tested.
- [x] The rootless quickstart runs as written, end to end, on a fresh Ubuntu machine and on WSL2.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Merged in parts:
- part 1: #113;
- part 2: #120, #121, #126, #128;
- part 2b: #129;
- part 3: #131, with #132 for the gate on newer runner kernels.

Each was gated in the disposable sandbox (WSL2, uid 2001, gid 2501, subordinate ranges uid 200000 and gid 300000). Each gate covered fmt, clippy on 1.98.1 and 1.85.0, the preflight, `check.sh` with `HATCHLING_REQUIRE_CELL=1` on both toolchains, the subordinate tests with `HATCHLING_REQUIRE_SUBID=1` in `sbx-job --idmap`, hand mutants, and mutation on the changed lines (0 missed, 0 timeouts in every part). The criteria:

- **Each cell kind on its own uid, with real cells.**
  - The spawner's `a_cell_on_subordinate_ids_runs_as_them_with_no_groups` reads every process of the cell from the host: all four uids and gids, an empty `Groups:`, and the maps.
  - The broker's real-cell tests cover the agent's cell (`a_subordinate_agent_cell_handshakes_over_its_own_connection`), a skill cell (`a_skill_cell_on_subordinate_ids_runs_as_them`) and a harness (`a_harness_on_subordinate_ids_edits_its_slice_the_same_way`, its work tree given and taken back through the tree helper).
  - A run cell is covered end to end by the quickstart below, whose chat runs are run cells. A subagent's cell is built by the same run-cell path; it has no test of its own on subordinate ids.
- **The endpoints start; the API refuses a cell.**
  - With subordinate ids, the API, the UI and operator sockets, the ingress and the device endpoint all start. The quickstart's app check drives the UI and operator sockets.
  - The API closes any peer that isn't its owner before reading a byte (`a_peer_that_is_not_the_owner_gets_no_byte_read`). A cell's uid is such a peer, and a cell can't reach the socket's path in any case.
- **Without a range they refuse, naming `/etc/subuid`:** the ingress's refusal test checks the message; the API, device and operator endpoints share it (`OWN_UID_HINT`).
- **Unsafe maps refused before a cell spawns:** `subid`'s tests refuse:
  - a range holding uid 0, the owner or any account's id;
  - an empty range;
  - helpers that aren't root's setuid regular files.

  Its hand mutants of each refusal were caught (#113).
- **The quickstart, as written.**
  - *Fresh Ubuntu:* CI's "without root" job, on `ubuntu-24.04` with `apparmor_restrict_unprivileged_userns` at 1, runs the README's section as the runner user (`readme_quickstart.py --rootless`). The README's own setup installs the map helpers, adds the range and loads the broker's AppArmor profile. The stand-in's answer arrives, and the app answers in Chromium.
  - *WSL2:* the section's commands ran as the sandbox's unprivileged user. The broker had no `HATCHLING_AGENT_UID` and took 200000; `first-run.py` got the answer. The `sudo` steps' effects were already in place (the sandbox's range and helpers), and WSL2's kernel has no AppArmor restriction, so that block is skipped there as written.

Left for later, outside the criteria: a run's tree that the cell kept busy past its take-back stays in the broker's temp directory, owned by the cell's ids, and the startup sweep can't remove it as the broker. Sweeping such leftovers through the tree helper is a candidate in the queue.

## Notes

- `newuidmap` writes the map from outside the namespace, so the spawn path needs a parent-side step after `unshare`, before the child continues. If the spawn code already writes a map from the parent anywhere, extend that path rather than adding a second one.
- Ubuntu's AppArmor restricts unprivileged user namespaces by default (`kernel.apparmor_restrict_unprivileged_userns`). The rootless quickstart has to say how to allow them for the broker, or ship an AppArmor profile for it, rather than turning the restriction off for the whole machine.
  The profile Ubuntu documents for a program that builds its own sandbox, at the broker's installed path:

  ```text
  abi <abi/4.0>,
  include <tunables/global>
  profile hatchling-broker /usr/local/bin/hatchling-broker flags=(unconfined) {
    userns,
  }
  ```

  It goes in `/etc/apparmor.d/hatchling-broker`, loaded with `apparmor_parser -r`. It attaches when the broker is next executed, and `/proc/<pid>/attr/current` should then name it. A forked cell keeps it. An exec of `newuidmap`/`newgidmap` moves to their own profile if one is installed, and keeps this one otherwise; they don't need `userns`, only their own map writes. `no_new_privs` anywhere above the helpers' launch would stop their setuid, and no profile undoes that. The quickstart's test must load it on a stock Ubuntu.
- Admitting callers by uid stays the model. What changes is that, under this WO, no cell ever has a caller's uid.
- **A rootless cell drops the owner's supplementary groups, every time.** The helpers leave `setgroups` at `allow` (part 1), so the cell clears its groups (`setgroups(0, NULL)`), then sets its gid and uid, checking each call, before any untrusted code runs. The broker then checks from outside: the cell's `Uid:` and `Gid:` lines in `/proc/<pid>/status` each show the mapped id four times, and `Groups:` is empty. If `setgroups` was already `deny` (inherited from an enclosing namespace), the groups can't be cleared, and the cell must not start.
- **Dropping groups can widen access, so a cell's rights aren't a subset of the owner's.** A file of another user's, with mode `0604` and one of the owner's groups, is closed to the owner (group: nothing) but open to anyone else (other: read), and a cell with no groups is anyone else. The cell's filesystem is what the broker binds into its mount namespace, so that's the boundary: rootless cells get no broader view of the host than root ones do. Don't keep a group (say `docker`) to preserve a deny.
- **Cells of one kind share a uid** (out of scope above), so uid separates a cell from the owner and the other kinds, not from its siblings. And a subordinate range rootless Podman also uses gives Podman's containers the same host ids. The quickstart should give Hatchling a range of its own.
  Part 3 says so in the README: a range is per user, so a Podman user's containers and that user's cells share ids, and the way apart is a user of Hatchling's own.
  - **What a sibling can and can't do through the shared uid.** Signals, `/proc`, abstract sockets, keyrings and inotify don't reach a sibling: each cell has its own pid, mount, network and user namespace, nothing on disk is shared, and the syscalls that would cross are filtered. One thing does cross: the kernel charges pipe buffers to the host uid (`user_struct::pipe_bufs`), whatever the namespace. A cell that grows about 64 pipes to 1 MiB each uses up the default per-user soft limit (16,384 pages). Its siblings' new pipes then get the two-page minimum and can't grow (`EPERM`). That's availability, not access. The README's limitations say so until per-cell uids come.
- **A cell keeps uid 0 inside its namespace.** Mapping it to a nonzero id instead (say 1000) stops no attack we can name. Every capability set is empty, the securebits are locked, `no_new_privs` and seccomp come first, and new user namespaces are filtered. `become_cell_user` still sets the ids with `setresgid` and `setresuid` after the maps are in. Writing a map changes how ids translate, not the process's own credentials: until those calls, the cell would still be the owner's uid underneath, for access checks, file ownership and peer credentials.
- **Done in part 2b, with one change to this plan:** the helper gives the trees to the cell before the run and takes them back (to the owner) after, and the broker then reads and removes them as before, rather than the helper exporting and removing them itself. A walk that follows no symlink and crosses no mount makes the take-back safe: a cell can't hard-link a file into its tree from outside its writable mounts, and a change of owner clears set-id bits. If a process of the cell outlived its run, the take-back or the removal fails, and the run says so. The tree then stays, owned by the cell's ids, in the broker's temp directory: the startup sweep (WO-012) can't remove it as the broker. Sweeping such leftovers through the helper is a follow-up.
- **Done in part 3:** the README's "Quickstart without root", and the CI job "without root" on `ubuntu-24.04` with `apparmor_restrict_unprivileged_userns` left at 1. The job runs the subordinate-id tests (`HATCHLING_REQUIRE_SUBID=1`) on the runner's own distinct ranges (uid 200000, gid 300000) under a profile for the build tree, unloads it and takes the ranges away, then runs the README's section as written as the runner user (`readme_quickstart.py --rootless`, pid 1 of a PID namespace root made). The README's setup adds the range and loads the broker's profile itself. `HATCHLING_AGENT_UID` became optional with subordinate ids: the broker takes the cells' uid, so the quickstart needn't know the range.
- **A hard core limit of 0 stops an unprivileged broker's cells where core dumps go to a crash handler.** A cell holds its core limit at 1 byte, so no core reaches a pipe `core_pattern` (apport on Ubuntu); without root it can't raise a hard limit of 0 to that 1. A login session's hard limit is unlimited, but `sudo` leaves 0, so CI's job restores it with `prlimit --core=0:unlimited` before dropping to the runner. The first CI run of the job found it; the broker reported only "child reported" and nothing, because the spawner held its own end of the report pipe open (fixed here: the reason now arrives at once).
- **The harness's work tree (part 2b) goes through a trusted helper, never a chown back.** A helper process in its own user namespace maps both identities: its 0 to the owner, its 1 to the cell's id, uid and gid both (an inode's capability checks need both mapped). It prepares the tree (ownership set by `fchown` on descriptors under a pinned root), exports the chosen outputs (regular files only, opened beneath the root with `openat2` and `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV`, bounded in count, size and depth, with no FIFOs or hard-linked files), and removes the tree, all after every process that could write it is gone. A recursive chown or chmod would follow a hard link to a file outside the tree. The cell never runs in the helper's namespace, nor gets its descriptor.
