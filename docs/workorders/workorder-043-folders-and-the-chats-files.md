# WO-043: Folders, and the chat's files: give a project back, keep it in the chat, take what you need

**Status:** QUEUED
**Area:** `crates/hatchling-room` (the supervisor's read of a folder), `crates/hatchling-broker` (the hand-over of a folder, the Library's groups and limits), `crates/hatchling-core` (the wire), `crates/hatchling-agent` (the tool), `crates/hatchling-vault` (its log of the Library's chunks), `frontend/` (the chat's cards, the Library's groups and choosing), `docs/`
**Issue:** [#502](https://github.com/undergroundrap/hatchling/issues/502)
**Trust boundary:** yes (what crosses out of a cell, and the limits on it)

## Problem

[WO-041](workorder-041-files-between-you-and-the-agents-computer.md) lets the agent hand the owner one file at a time, at most 16 a conversation, kept in a Library of at most 64 files and 256 MiB. Real work doesn't come back that way. Asked to build a site or a report with its data, the agent makes a folder of tens or hundreds of files, and the owner wants it back as that folder.

The owner also wants to take files from the chat they came in. Today a file's card in the chat is gone after a restart, and the only way back to a file is the Library's list in Settings, where "Download all" ([#500](https://github.com/undergroundrap/hatchling/issues/500)) takes everything. Scrolling back through a conversation should show what the agent gave in it, where it gave it, ready to download again.

A folder (here) is a directory in the agent's `/workspace`, with the files and directories beneath it.

## Scope

**In scope:**

- **A folder handed over.** The agent gives a folder in `/workspace`. The broker, not the agent, reads it out of the Room beneath that folder, by the same rules as one file (no links, no other mounts, regular files and directories only). It checks each file and each path for the secrets it keeps, cleans each part of each path to plain names, and keeps the whole as one item in the Library, with the list of what is in it. An archive the agent makes stays one opaque file, as now.
- **The chat keeps its files.** Every file or folder the agent gave is shown in the conversation where it was given, with Download, after a restart and when scrolling back. One removed since says so.
- **Taking what you need.** The Library is grouped by the chat and the reply each item came from. The owner can download one item, a reply's items, or any they pick, as one `.zip` with folders as they were.
- **Limits that fit the work.** A conversation's limit counts hand-overs, not files, and the Library's limits on files and bytes are set again from what they cost (memory, disk, backups, and the vault's log of each chunk, which is quietened for the Library).

**Not in scope:** keeping a folder on the PC in step with the agent's (a later step across the PC's boundary); files to or from anyone but the owner; editing a file in the app; files for runs with nobody there; looking inside an archive the agent made.

## Acceptance criteria

- [ ] The agent gives a folder of at least 100 files in nested directories, and the owner downloads it as one `.zip` with its folders as they were, every file byte for byte.
- [ ] A folder holding a link, a device, another mount, a path that leaves it, or a name Windows keeps or acts on when its folder is shown is refused, or that part is left out and said; each is a test.
- [ ] A folder in which any file, or any path, holds a secret the broker keeps isn't handed over, nothing of it is kept, the owner and the agent are told without naming the secret, and giving is off for the conversation.
- [ ] Scrolling back through a conversation, after a restart too, shows each file or folder the agent gave in it, with Download; one removed since says so.
- [ ] The owner downloads one item, everything one reply gave, or any items they pick, as one `.zip`.
- [ ] The limits are counted in hand-overs, and set from measurements recorded in the evidence; a full Library and a spent conversation are said to the owner and the agent.
- [ ] Each crossing is in Activity: a folder by its name, its count of files, its size and how it ended.
- [ ] A real install gives a folder each way with a real model.
- [ ] Each guard is hand-broken and its test fails for the right reason.

## Gates

As [TEMPLATE.md](TEMPLATE.md), with the frontend and protocol gates, and the installed Room tests as root.

## Design

To be settled from an independent threat model of this work order and the code, before any of it is built, as WO-041's was, and agreed with the maintainer.

## Evidence

Each slice's record, as it lands.

## Notes

Raised in WO-041's acceptance run and [#500](https://github.com/undergroundrap/hatchling/issues/500): a browser that asks where to save each download asked once for every file, and the agent couldn't give a project back as one. An archive made by the agent was considered and dropped: it hides its files from the secret check and the app's per-file warnings, and can carry a path that leaves its folder. Here the broker makes the archive.
