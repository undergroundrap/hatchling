# WO-048: Memory that knows where it came from: provenance, supersession, bounded retrieval, then search

**Status:** QUEUED
**Area:** `crates/hatchling-memory` (items, the store, recall), `crates/hatchling-broker` (keeping, the owner's memory ops, the decisions history), `crates/hatchling-agent` (recall), `crates/hatchling-core` (the wire), `frontend/` (the owner's memory list), `docs/`
**Issue:** [#598](https://github.com/undergroundrap/hatchling/issues/598)
**Trust boundary:** yes

## Problem

Hatchling's memory has a strong door: the agent can only propose, the owner approves the exact words, a digest binds the approval, and the broker keeps the item. A misled agent can't plant words that stay.

Behind the door there is little:

- **No provenance.** A kept item records nothing of where it came from: no conversation, no run, no mark of whether the words are the owner's or the agent's inference. The approval's record exists, in the decisions history, but nothing links it to the item. Nothing can answer "why do you believe this?"
- **No supersession.** A correction is a new item; the one it corrects stays and is recalled beside it until the owner finds it and forgets it.
- **Duplicates.** A proposal of something already kept is asked again and, if approved, kept twice ([#596](https://github.com/undergroundrap/hatchling/issues/596)).
- **Recall by tag, then cut.** A run is given at most 10 items and 8 KiB, the most recently updated first, from every item tagged for recall, and the store scans and sorts every match before the cap applies. An item outside the newest ten is never recalled, however relevant.

The queue named the first part of this as a candidate: *Memory: bounded retrieval and provenance*. This work order is it, with what's needed beside it.

## Scope

**In scope:**

- **Provenance, attested by the broker.** Each kept item records where it came from: the conversation and run, the approval it was kept by, and whether its words are the owner's or the agent's proposal. The broker writes it; nothing the agent or a cell says can set or change it. Items kept before this say *not recorded*, never a guess.
- **Supersession.** A proposal can name the item it corrects. Approved, the new item records which it supersedes, the old one is no longer recalled, and the owner can see the chain. Only the broker links them, on the owner's approval.
- **No duplicates** (#596): a proposal whose words match a kept item isn't asked again, and `keep` makes no second copy.
- **Absences kept as absences.** An item can record a settled negative: a claim found untrue, something that doesn't exist, a thread closed. Recall gives it as what it is, so "nothing is known" and "it's known not to be so" stay different to the agent: otherwise it works the settled answer out again, and now and then asserts the opposite. It's proposed and approved like any item, with the same provenance, and it can be superseded.
- **When to check again.** An item can carry a date after which it's offered to the owner to confirm, change or forget.
- **Bounded retrieval.** Recall carries its budget to the store: it never reads more than it can give a run.
- **Then search.** Recall by relevance to what the owner asked, not only the newest tagged items, within the same budget, without sending memory anywhere it doesn't go today. Its design comes first, in this work order, before code.
- **The owner sees it.** The memory list shows where each item came from, what it replaced, and when it's due to be checked.

**Not in scope:** the agent writing memory without approval; a model summarizing memory; a people model; importing memory from another agent (it builds on this provenance); sealing (WO-047).

## Acceptance criteria

- [ ] A kept item's provenance is written by the broker alone: a test has the agent's proposal claim another origin and finds the broker's record unchanged.
- [ ] A correction approved as superseding an item stops that item being recalled, and the chain is shown; a test covers recall before and after.
- [ ] A proposal matching a kept item isn't put to the owner again and isn't kept twice (#596); a test proposes the same words twice.
- [ ] A kept absence is recalled as an absence, marked as one, never as a missing item; a test recalls one beside a query that finds nothing.
- [ ] Recall reads no more than its budget from the store; a test with many items checks what's read.
- [ ] Search's design is written and approved by the maintainer before its code; recall by relevance then finds an item outside the newest ten, in a test.
- [ ] Items from 0.10 load, and say their provenance isn't recorded.
- [ ] Each new guard fails its test when removed.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- Absences were added to the scope on 2026-11-23, by the maintainer's decision, before any of this work started: a field and a test now, where a change to the sealed store's item format later would be a migration.
- First on v0.11.0, by the maintainer's decision of 2026-11-22. WO-045, the evaluation harness, comes after it and measures recall too.
