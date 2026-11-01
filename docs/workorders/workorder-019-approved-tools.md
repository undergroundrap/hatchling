# WO-019: Approved tools: what a cell can run, pinned and listed

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-broker` (`harness.rs`, cell configuration, CLI), `crates/hatchling-runtime`, `docs/`
**Issue:** [#47](https://github.com/undergroundrap/hatchling/issues/47)
**Trust boundary:** yes

## Problem

A harness's CLI cell gets a set of programs to run: a shell, git, ripgrep and the coreutils by default, or the absolute paths `HATCHLING_HARNESS_TOOLS` names. They're named by path only. A program replaced on disk after startup is what the next cell runs, with nothing to notice it; and adding one means editing an environment variable by hand, with no record of what was approved, or when.

Hatchling should not pick tools for people, and shouldn't bundle them: every program it ships is one it has to keep up and vouch for. It should make adding one safe: approved by the operator, pinned by hash, visible, and easy to take back.

## Scope

**In scope:**

- **An approved-tools list**, owned by the broker's user and writable by no one else: each tool's path, its SHA-256, and when it was approved.
- **`hatchling tool add <path>`**, **`hatchling tool list`** and **`hatchling tool remove <name>`**: add records the hash, and refuses anything that isn't a regular file at an absolute path.
- **Pinning:** each tool is checked against its hash at startup and before every run that gives it to a cell. One that changed is refused, and the run says which. *Done in part 1, for the tools a deployment has now (`HATCHLING_HARNESS_TOOLS`, or the defaults): pinned when the broker starts.* Its check before a run looks at each tool's file first, and hashes it again only if it changed.
- **Cells:** an approved tool is mounted read-only in the cells that get tools, under the name it was approved with. *Part 1:* what's mounted is a copy of the bytes hashed (see the notes). A tool gets no network: it reaches the outside, if at all, the way everything in a cell does.
- **The defaults** keep working: the default set is approved the first time, and recorded with its hashes, so an existing deployment sees no change.
- **Docs:** adding, listing and removing a tool, and what approving one means.

**Not in scope:**

- The agent installing or approving a tool. It can ask, and the operator decides; delivering that request outside the conversation is the "approvals delivered to the user" candidate.
- Fetching packages from a registry (npm, PyPI) at run time.
- A curated catalog of tools.

## Acceptance criteria

- [ ] `hatchling tool add` records a tool's path and hash; `list` shows each tool and when it was approved; `remove` takes it away, and the next cell doesn't have it.
- [ ] A tool changed on disk after it was approved is refused before the next run that would get it, and the run reports which tool and why; tested.
- [ ] An approved tool is read-only in the cell, and runs; tested in a real cell with `HATCHLING_REQUIRE_CELL=1`.
- [ ] A list file writable by anyone but the broker's user is refused at startup.
- [ ] A deployment with no list starts with the default set approved and recorded, and its cells get the same tools as before.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- **Why a copy, not the tool's path (part 1).** A read-only bind mount blocks writes through the mount, not to the host path: a program rewritten in place after it was hashed is what the cell sees (measured for skill code, `crates/hatchling-broker/src/skill_cell.rs`). So cells are given copies of the bytes hashed, in a directory of the broker's own, as a skill's code and a harness's binary are. Run as root, the spawner allows those copies and no longer the host tool paths.
- **Why once, at start.** The defaults on the sandbox's Ubuntu are 45 files and 416,944,160 bytes, because its coreutils are one 11 MB program under 36 names. Copying and hashing that for every run would cost more than the run's setup. So each file is read once, at start, to pin it. The copy is made once, at the first run that gets tools, one file per program, its other names hard links: by then a root broker runs as its own user (WO-017), whose the copy must be for the spawner to give it. Each program is read again for it, and must still hash to its pin. The copy is named as a staging directory, so one a broker leaves is removed by the next one's sweep.
- **How a run checks.** A tool's file is looked at: its device, inode, size and change time. The kernel sets the change time on every write, and only root can set it back. A file that's the one pinned, unchanged, is the program pinned. Any other is hashed again: a program written back as it was is fine, another is refused. Whatever the check sees, the cell runs the copy.
- **What part 2 can rely on.** No cell is given a tool's host path, so the list can't widen what a cell reaches beyond what the broker's own user can read: the broker makes the copy. A tool added to the list takes effect when the broker next starts, as its copy is made then.
- **Not pinned: a tool's shared libraries.** They're found with `ldd` and mounted from the host, as for every cell's programs. As root, `ldd` runs as whoever owns the tool, never as root on another user's program (WO-017).
- **Why not a bundle.** Everything Hatchling ships, it has to update and answer for in a review. A tool the operator approves is theirs: Hatchling's job is to make sure it's the program they approved, and that a cell can run it but not change it.
- Shares its pinning with WO-018's manifests: one check, run the same way for a harness and a tool.
