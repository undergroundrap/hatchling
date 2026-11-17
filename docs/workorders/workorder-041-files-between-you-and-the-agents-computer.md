# WO-041: Files between you and the agent's computer

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-core` (the app's protocol, the Room's wire), `crates/hatchling-broker` (a file's way into and out of the Room, the Library), `crates/hatchling-room` (the supervisor's file answers), `crates/hatchling-agent` (a tool to hand a file over), `frontend/` (attaching a file, the Library, downloading), `docs/`
**Issue:** [#486](https://github.com/undergroundrap/hatchling/issues/486)
**Trust boundary:** yes (files crossing into and out of a cell)

Step 4 of the 0.9 Room work. The scope below is as the repo states it ([the scorecard](../scorecard.md): "Read files you attach" and "Give you files, in chat and the Library", next for 0.9; [WO-034](workorder-034-run-commands-in-the-agents-encrypted-room.md) left "owner file transfers" to a later step). It is confirmed with the maintainer before work starts.

## Problem

The agent has a computer of its own with files in it, and nothing moves a file between it and its owner. You can't hand the agent a document to work on, and it can't hand you what it made: a file it wrote stays in the Room, and the only way out is the agent reading it back into the chat.

## Scope

**In scope:**

- **A file in:** the owner attaches a file in the app; the broker puts it in the Room's workspace (an inbox the agent can read but the file's name can't escape), bounded in size, and the chat says where it is. The file's bytes are the owner's; to the model they are untrusted data, as any file it reads.
- **A file out:** the agent asks to hand the owner a file from its workspace; the broker copies it out of the Room, bounded in size, and offers it to the owner in the chat and in the Library, labelled as the agent's. Nothing leaves the Room except to the owner.
- **What crosses is checked:** a file out goes past the same checks as an answer (the output gate's held secrets), and a hit isn't handed over and says why. A file's name and type are the broker's reading, never the agent's word for them.
- **The app** downloads a file as a file, never renders what the agent made as a page.

**Not in scope:** files from anywhere but the owner's app (a channel, an email); sharing a file with anyone else; files for runs with nobody there (scheduled runs, hooks, subagents); editing a file in the app.

## Acceptance criteria

- [ ] A file the owner attaches is in the agent's workspace, by a name the broker chose, and the agent reads it with its tools; one past the size bound is refused before anything is written.
- [ ] No name, link or path a file brings can put it anywhere but the inbox; each is a test.
- [ ] A file the agent hands over reaches the owner's chat and Library, labelled as the agent's, byte for byte; a link, a device or a file past the bound is refused.
- [ ] A file holding a secret the broker keeps isn't handed over, and the agent and the owner are told why.
- [ ] The app offers a file the agent made only as a download, with its name and size, never inline.
- [ ] Each crossing is in Activity: which way, the name, the size, and how it ended.
- [ ] A real install moves a file each way with a real model.
- [ ] Each guard is hand-broken and its test fails for the right reason.

## Design

Settled from an independent threat model of this work order and the code (2026-11-16), before any of it was built. The attackers it answers: the agent itself, injected or hostile (every process of the Room's user); a hostile file someone sent the owner, its name and its content; a page or a model steering the agent to carry something out; another local user or web page; and, as the spawner sees it, a broker that isn't to be trusted with a path.

**A file in.**

- The owner's name for a file is never used on disk. The broker makes a stem of it from `[A-Za-z0-9._-]`, at most 64 bytes, not starting with a dot. The spawner checks the stem again, and names the file `NNNN-<stem>`, numbered past the highest number the inbox has given. That number is kept in a root file of the inbox's, so no number is given twice and a name the model or Activity was told always means the same bytes.
- The spawner writes it as root into the volume's own `inbox` directory, never through `workspace/inbox`, which is a name the Room can reach. It opens the inbox from the pinned volume root without following a link or crossing a mount. It writes an unnamed `O_TMPFILE`, makes it `0444`, syncs it, and links it in by its name, which never replaces anything. A put that fails leaves no file.
- A put the spawner never answered is said not to be known either way, and isn't sent again.
- The Room's own files can fill the disk the inbox shares. A put then says the disk is full; holding room for the inbox is [WO-040](workorder-040-room-disk-capacity.md)'s.
- The owner removes a file through the spawner too, under the same turn: the files are root's, and the broker can't. That comes with the app's side of a file in.
- The Room sees the inbox read-only, `noexec`, at `/workspace/inbox`. The inbox directory is root's, `0755`, so the Room's user can read what is in it and change none of it.
- The put holds the room disk's turn, as a disk operation does: through the Room's session when one is live, or directly when none is. A disk that isn't open refuses it.
- **Bounds:** 8 MiB a file; the inbox holds 64 files and 256 MiB. A full inbox refuses a file; nothing is evicted. The owner removes files.
- The model is told of a file by a line the broker writes, with its place, its size, and that its name and what is in it are untrusted. The line is given in the next run's task alone, never kept as the owner's words. The owner's original name is shown only in the app, as text; the inbox's name is made of it in plain ASCII.
- A file in that holds a secret the broker keeps gets a warning in the app, not a refusal: it is the owner's file.
- **In the app** (no change to the frozen protocol): an optional live capability sends the file straight to the broker, never as a Command, so automation can't send one. The main chat's + gives a file; it is sent at once, one at a time for the app. Up to its end, a file not answered wasn't put; after its end, it isn't known, the inbox's list says, and the stream's word of the file settles it; it is never sent again unasked. The chat's notice is the app's, by the inbox's name; the owner's name is shown beside it inert, in an isolated direction, its hidden characters as code points. A file holding a secret, or not checked, is followed by a warning with Remove. Settings › Data lists the inbox and removes a file.

**A file out.**

- The agent names a file in `/workspace`. The supervisor reads it as the Room's user, beneath `/workspace`, with no links, and only a regular file. It reads in pieces, each no more than a frame. A file that changes size or identity while it is read is refused.
- The spawner puts the pieces together, as root, holds them to the inode, length and change time the supervisor said staying the same, to the bound and to a deadline, and hands the broker the file. The broker hashes it, checks it, and keeps that copy in a sealed store of its own, the Library. A download is always that copy, never a fresh read of the Room.
- **The check** is the output gate's held secrets and the keeper's whole set. A hit withholds the file, tells the agent and the owner why without naming the secret, and turns file out off for that chat: a file is large enough to test many guesses at once.
- **The name** is the broker's: the basename through the same allowlist, refusing Windows' reserved names and a trailing dot. The app shows the whole name, flags an extension that runs, and says the file is the agent's.
- **The app** downloads it as `application/octet-stream`, whatever its extension, and never previews or renders it.
- **Bounds:** 8 MiB a file; the Library holds 64 files and 256 MiB; 16 hand-overs a chat. A full Library refuses; nothing is evicted. The Library keeps a file until the owner removes it or Hatchling is reset.
- Only a chat with the owner there hands a file over: the tool isn't offered to a run with nobody there.

**What it doesn't stop**, said in [your security](../your-security.md) when it ships:

- A secret encoded, compressed, encrypted or split isn't found by the check. The check catches accidents; the real control is that the vault's secrets are never in the Room.
- A file the owner attaches can steer the agent like any page it reads.
- A file the agent made can be hostile to whatever opens it.

**Activity** keeps each crossing's direction, the broker's name for the file, its size, and how it ended. That departs from "the trail names tools, never what they carried", and the docs say so. Never the contents, and never a path in the Room.

**Slices:**

1. The inbox put: the spawner and the trusted mount code, with the inbox's mode, proved as root.
2. A file in from the app: the upload in parts, the stem, the line to the model, and Activity. It lands in two pull requests: the broker's side and the local API first, then the app's own attach and inbox list.
3. A file out to the Library: the supervisor's read, assembly, the checks, the sealed store, and the agent's tool. It lands in two pull requests: the Room's side first (the supervisor's read, and the spawner putting the pieces together), then the broker's store, the checks and the agent's tool.
4. A file out in the app: the card, the Library, the download, the docs, and the evidence.

## Gates

As [TEMPLATE.md](TEMPLATE.md), with the frontend and protocol gates, and the installed Room tests as root.

## Evidence

Each slice's record, as it lands:

- Slice 1, the inbox put: [room-inbox-put](../evidence/room-inbox-put.md).
- Slice 2, a file in through the local API: [room-file-in](../evidence/room-file-in.md).
- Slice 2, a file in from the app: [room-file-in-app](../evidence/room-file-in-app.md).
- Slice 3, a file read out of the Room: [room-take-wire](../evidence/room-take-wire.md).

The whole is filled in at close.
