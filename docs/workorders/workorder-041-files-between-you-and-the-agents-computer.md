# WO-041: Files between you and the agent's computer

**Status:** QUEUED
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

## Gates

As [TEMPLATE.md](TEMPLATE.md), with the frontend and protocol gates, and the installed Room tests as root.

## Evidence

Filled in at close.
