# WO-043: Folders, and the chat's files: give a project back, keep it in the chat, take what you need

**Status:** IN-PROGRESS
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

From an independent threat model of this work order and the code (2026-11-17), agreed with the maintainer before any of it is built.

- **First, [#504](https://github.com/undergroundrap/hatchling/issues/504):** a hand-over the Library can't keep is refused before its file is checked, and a checked file spends the conversation's count, sealed, whatever comes of it. Without that, a larger item would make the count's guard easier to get round.
- **The agent's tool** is a new `give_folder`, apart from `give_file`, so the model and Activity say plainly that a folder crossed.
- **Reading a folder:** the supervisor gains a listing of one directory, by the same rules as reading a file (beneath the workspace, no links, no other mounts), bounded, keeping no state; the broker walks the tree with it and reads each file as one file is read now. What a listing says a thing is, is never trusted on its own.
- **What is left out, and said:** links, devices, pipes, other mounts (the inbox among them). **What refuses the folder:** a secret anywhere, a bound passed, a file changing while it is read, two names that differ only in case, a name Windows keeps for a device or acts on when its folder is shown, at any depth, and a name that isn't plain ASCII.
- **Names inside a folder** are any printable ASCII but Windows' forbidden characters (`< > : " / \ | ? *`), with spaces inside allowed and no leading or trailing space or trailing dot. Files whose names start with a dot (`.gitignore`, `.github/`) come back.
- **The check:** every file and every path is checked for the secrets the broker keeps before anything is kept. One found withholds the whole folder, names nothing, and turns giving off for the conversation.
- **The Library** keeps a folder as one item: its files' bytes, and a sealed list of them (each path, size and SHA-256). Each file is read back to its own hash. Each item remembers the conversation and the reply it came from, sealed and set by the broker.
- **The chat** draws a given item's card from the Library, after a restart too, within the exchanges the conversation keeps; older items are listed under the conversation. A removed item says so.
- **Choosing:** one item, a reply's items, or any the owner picks, downloaded as one `.zip` made in the app from checked files. The app checks every path itself before it writes one, so no path can leave the folder it is unpacked into.
- **Limits to start from, measured before they are fixed:** a folder 32 MiB, 512 files and 16 levels deep; a conversation 16 hand-overs and 128 MiB read out; the Library 64 items, 256 MiB and 4,096 files. The vault's log of each chunk is quietened for the Library.

**Slices:** 0, #504; 1, the Room's listing; 2, the Library's items, groups, limits and log; 3, the folder hand-over and `give_folder`; 4, the local API; 5, the chat's cards across a restart; 6, groups and choosing in the app; 7, docs and the acceptance.

## Evidence

Each slice's record, as it lands.

- Slice 0, a hand-over spent before its file is checked ([#504](https://github.com/undergroundrap/hatchling/issues/504)): [hand-over-count.md](../evidence/hand-over-count.md).
- Slice 1, the Room lists a folder: [room-list-wire.md](../evidence/room-list-wire.md).

## Notes

Raised in WO-041's acceptance run and [#500](https://github.com/undergroundrap/hatchling/issues/500): a browser that asks where to save each download asked once for every file, and the agent couldn't give a project back as one. An archive made by the agent was considered and dropped: it hides its files from the secret check and the app's per-file warnings, and can carry a path that leaves its folder. Here the broker makes the archive.
