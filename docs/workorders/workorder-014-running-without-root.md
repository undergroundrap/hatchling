# WO-014: Run the full broker without root

**Status:** QUEUED
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

- [ ] An unprivileged broker with a subordinate range runs each cell kind as a uid that isn't the owner's, shown by the cell's `/proc/self/status` and `uid_map`; tested with real cells.
- [ ] Under that broker, the local API and the chat ingress start, and the API refuses a connection from a cell (its uid isn't a caller's); tested.
- [ ] Without a subordinate range, the API and the ingress still refuse to start, and the message names `/etc/subuid`; tested.
- [ ] Each unsafe map listed in the scope is refused before any cell spawns; tested.
- [ ] The rootless quickstart runs as written, end to end, on a fresh Ubuntu machine and on WSL2.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- `newuidmap` writes the map from outside the namespace, so the spawn path needs a parent-side step after `unshare`, before the child continues. If the spawn code already writes a map from the parent anywhere, extend that path rather than adding a second one.
- Ubuntu's AppArmor restricts unprivileged user namespaces by default (`kernel.apparmor_restrict_unprivileged_userns`). The rootless quickstart has to say how to allow them for the broker, or ship an AppArmor profile for it, rather than turning the restriction off for the whole machine.
- Admitting callers by uid stays the model. What changes is that, under this WO, no cell ever has a caller's uid.
- **A rootless cell drops the owner's supplementary groups, every time.** The helpers leave `setgroups` at `allow` (part 1), so the cell clears its groups (`setgroups(0, NULL)`), then sets its gid and uid, checking each call, before any untrusted code runs. The broker then checks from outside: the cell's `Uid:` and `Gid:` lines in `/proc/<pid>/status` each show the mapped id four times, and `Groups:` is empty. If `setgroups` was already `deny` (inherited from an enclosing namespace), the groups can't be cleared, and the cell must not start.
- **Dropping groups can widen access, so a cell's rights aren't a subset of the owner's.** A file of another user's, with mode `0604` and one of the owner's groups, is closed to the owner (group: nothing) but open to anyone else (other: read), and a cell with no groups is anyone else. The cell's filesystem is what the broker binds into its mount namespace, so that's the boundary: rootless cells get no broader view of the host than root ones do. Don't keep a group (say `docker`) to preserve a deny.
- **Cells of one kind share a uid** (out of scope above), so uid separates a cell from the owner and the other kinds, not from its siblings. And a subordinate range rootless Podman also uses gives Podman's containers the same host ids. The quickstart should give Hatchling a range of its own.
- **The harness's work tree (part 2b) goes through a trusted helper, never a chown back.** A helper process in its own user namespace maps both identities: its 0 to the owner, its 1 to the cell's id, uid and gid both (an inode's capability checks need both mapped). It prepares the tree (ownership set by `fchown` on descriptors under a pinned root), exports the chosen outputs (regular files only, opened beneath the root with `openat2` and `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV`, bounded in count, size and depth, with no FIFOs or hard-linked files), and removes the tree, all after every process that could write it is gone. A recursive chown or chmod would follow a hard link to a file outside the tree. The cell never runs in the helper's namespace, nor gets its descriptor.
