# WO-035: The agent's tools in its own computer

**Status:** CLOSED
**Area:** `crates/hatchling-agent` (the tools), `crates/hatchling-core` (`RoomOp`, `RoomResult`, the supervisor's wire), `crates/hatchling-broker` (`room_ops`, `room_session`), `crates/hatchling-room` (the supervisor's file answers)
**Issue:** [#477](https://github.com/undergroundrap/hatchling/issues/477)
**Trust boundary:** yes

This file was written after the work shipped, from its pull requests and evidence records, when the queue was reconciled (2026-11-16). It should have been written first: [AGENTS.md](../../AGENTS.md) now says so.

## Problem

Step 3 of the 0.9 Room work. With the Room built ([WO-034](workorder-034-run-commands-in-the-agents-encrypted-room.md)), the agent had a computer it couldn't use: nothing in a chat could run a command there, read a file, find one, or change one.

## Scope

**In scope:** tools a chat run is offered when the Room is open: `run`, `run_check` and `run_stop` (a command as a named job, asked after or stopped), `read` (a page of a file), `search` (lines matching a pattern, by the Room's ripgrep), `write` and `edit` (a file made, or changed by an exact match). The broker's session ops behind them (`RoomOp`), and the supervisor's answers.

**Not in scope:** the tools for runs with nobody there (scheduled runs, hooks, subagents): Activity doesn't yet keep what a command or a search was, and should before they get them. Files to and from the owner.

## Acceptance criteria

- [x] Only a chat is offered the tools; a scheduled run, a hook's and a subagent are refused and aren't offered them. ([room-run-tool.md](../evidence/room-run-tool.md))
- [x] A command is a program and its arguments with no shell, named by the broker, bounded in time and output; a session asks after and stops only what it ran, and what it left running stops when it ends. ([room-run-jobs.md](../evidence/room-run-jobs.md), [room-run-tool.md](../evidence/room-run-tool.md))
- [x] What a command, a read or a search hands back reaches the model marked untrusted, bounded, with where the rest is kept. ([room-run-tool.md](../evidence/room-run-tool.md), [room-read-tool.md](../evidence/room-read-tool.md), [agent-search-tool.md](../evidence/agent-search-tool.md))
- [x] A search's pattern and paths are values, never options, to the program that runs it. ([room-search-wire.md](../evidence/room-search-wire.md), [agent-search-tool.md](../evidence/agent-search-tool.md))
- [x] An edit changes one exact match or names every place an ambiguous one matched; a write or an edit says what changed, or that nothing did. ([room-write-wire.md](../evidence/room-write-wire.md), [agent-write-tools.md](../evidence/agent-write-tools.md))
- [x] Each use is counted in the owner's Activity by what it was and how it ended (`room` in the run's trail).
- [x] Each defence is hand-broken and its test fails: 38 breaks for `run`, 29 for `read`, and those in each later record.
- [x] A real install ran real tasks through the tools with a local model ([room-run-tool.md](../evidence/room-run-tool.md), its acceptance run).

## What shipped

#425 (named jobs), #431 (`run` from a chat), #436 and #438 (a page of a file, `read`), #452 and #453 (search), #457 and #458 (write and edit), with the agent-loop fixes they surfaced (#446, #456, #461).

## Gates

As [TEMPLATE.md](TEMPLATE.md); each pull request's record gives its gate, mutation counts and breaks.

## Evidence

Each pull request's record in [docs/evidence/](../evidence/), named above, has its tested tree, commands, counts and breaks.
