# WO-019: Approved tools: what a cell can run, pinned and listed

**Status:** QUEUED
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
- **Pinning:** each tool is checked against its hash at startup and before every run that gives it to a cell. One that changed is refused, and the run says which.
- **Cells:** an approved tool is mounted read-only in the cells that get tools, under the name it was approved with. A tool gets no network: it reaches the outside, if at all, the way everything in a cell does.
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

- **Why not a bundle.** Everything Hatchling ships, it has to update and answer for in a review. A tool the operator approves is theirs: Hatchling's job is to make sure it's the program they approved, and that a cell can run it but not change it.
- Shares its pinning with WO-018's manifests: one check, run the same way for a harness and a tool.
