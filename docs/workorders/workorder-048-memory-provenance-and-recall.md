# WO-048: Memory that knows where it came from: provenance, supersession, bounded retrieval, then search

**Status:** CLOSED
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
- **Typed, with why and how to apply.** Each item is one of four kinds:
  - about the owner;
  - the owner's guidance on how the agent should work, a correction or a confirmed approach;
  - a fact about ongoing work;
  - a pointer to where something lives.

  Guidance and work facts carry *why* (the reason the owner gave) and *how to apply* (when it matters), so a rule can be weighed at its edges and not just followed blindly. The kind is part of what the owner approves. Recall gives it with the item. A correction the owner makes in the conversation prompts the agent to propose it as guidance.
- **Dates as dates.** A proposal names any date as a calendar date: "next Thursday" is proposed as 2026-12-03, and the owner approves that date. The broker refuses a proposal whose date isn't one, so a kept memory never means something different next week.
- **True as of.** Each item records when it was kept and when the owner last confirmed it, written by the broker. Recall gives the agent each item's date. An agent can then weigh "as of three months ago" against what it finds now, rather than treating a memory as timeless.
- **A contradiction becomes a proposal.** When what a run finds contradicts a recalled item (a file moved, a plan changed, the owner said otherwise), the agent proposes a correction that names the item and quotes what contradicted it. It's put to the owner as a supersession. The agent is told to do this, not to act silently on either the memory or what it found.
- **The conversation's notes and memory agree** (WO-052): a decision the notes record that contradicts a kept item prompts that proposal too. The notes are the conversation's working record and memory is what the owner approved to keep beyond it, so neither overrides the other unasked.
- **Bounded retrieval.** Recall carries its budget to the store: it never reads more than it can give a run.
- **Then search.** Recall by relevance to what the owner asked, not only the newest tagged items, within the same budget, without sending memory anywhere it doesn't go today. Its design comes first, in this work order, before code.
- **The owner sees it.** The memory list shows where each item came from, what it replaced, when it was kept and last confirmed, and when it's due to be checked.

**Not in scope:** the agent writing memory without approval; a model summarizing memory; a people model; importing memory from another agent (it builds on this provenance); sealing (WO-047).

## Acceptance criteria

- [x] A kept item's provenance is written by the broker alone: a test has the agent's proposal claim another origin and finds the broker's record unchanged. ([slice 1](../evidence/wo048-kept-record.md): `memory_keep::tests::where_a_kept_item_came_from_is_the_brokers_record_not_its_words`; a run can't write the `approved` tag, #736.)
- [x] A correction approved as superseding an item stops that item being recalled, and the chain is shown; a test covers recall before and after. ([slice 5](../evidence/wo048-supersession.md): `a_correction_replaces_what_it_corrects_once_the_owner_approves`; the list shows what it replaced, [slice 8](../evidence/wo048-memory-list.md).)
- [x] A proposal matching a kept item isn't put to the owner again and isn't kept twice (#596); a test proposes the same words twice. ([slice 2](../evidence/wo048-no-duplicates.md).)
- [x] A kept absence is recalled as an absence, marked as one, never as a missing item; a test recalls one beside a query that finds nothing. ([slice 4](../evidence/wo048-absences.md): `prompt::tests::a_kept_absence_is_recalled_as_known_not_to_be_so`.)
- [x] Recall reads no more than its budget from the store; a test with many items checks what's read. ([slice 6](../evidence/wo048-recall-index.md): `store::tests::a_budgeted_recall_reads_only_what_it_gives`, 500 items.)
- [x] Search's design is written and approved by the maintainer before its code; recall by relevance then finds an item outside the newest ten, in a test. (Approved 2026-11-27, [#742](https://github.com/undergroundrap/hatchling/pull/742); [slice 7](../evidence/wo048-recall-search.md): `store::tests::an_ask_finds_a_memory_outside_the_newest_ten`.)
- [x] Each item is kept with its kind, and guidance and work facts with their why and how to apply; recall gives them; a test keeps one of each kind. ([slice 3](../evidence/wo048-typed-items.md): guidance in `memory_keep`, work through a session in `serve`, owner and pointer elsewhere; recall's kinds in `prompt::tests`.)
- [x] A proposal whose date isn't a calendar date is refused, and an approved one keeps the date the owner was shown; a test proposes "next Thursday" as text. ([slice 3](../evidence/wo048-typed-items.md): `memory_proposal::tests`, and `guidance_is_shown_and_kept_with_why_when_and_its_day`.)
- [x] Recall gives each item with when it was kept and last confirmed, from the broker's record; a test has a proposal claim another date and finds the broker's unchanged. ([slice 1](../evidence/wo048-kept-record.md): `prompt::tests::each_item_says_when_it_was_kept_and_on_whose_word`; the provenance test's words claim 2020-01-01, and its record keeps the broker's time. [Slice 8](../evidence/wo048-memory-list.md) confirms an item.)
- [x] A run whose tool results contradict a recalled item proposes a correction naming it, with the contradiction quoted, and the owner's approval supersedes the item. A test runs a scripted model through it, and WO-045 measures how often real models do. ([slice 5](../evidence/wo048-supersession.md): `tests/memory_recall.rs`, a scripted model against the real broker pipeline. How often real models do it is WO-045's to measure.)
- [x] Items from 0.10 load, and say their provenance isn't recorded. ([slice 1](../evidence/wo048-kept-record.md): `item::tests::an_item_from_before_records_reads_as_not_recorded`; the list says so, [slice 8](../evidence/wo048-memory-list.md).)
- [x] Each new guard fails its test when removed. (Each slice's evidence records its hand-breaks, every one caught, and mutation with none missed.)

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Each slice commits its evidence under `docs/evidence/`.

- Slice 1, the record: [wo048-kept-record.md](../evidence/wo048-kept-record.md).
- Slice 2, no duplicates: [wo048-no-duplicates.md](../evidence/wo048-no-duplicates.md).
- Slice 3, typed items and dates: [wo048-typed-items.md](../evidence/wo048-typed-items.md).
- Slice 4, absences: [wo048-absences.md](../evidence/wo048-absences.md).
- Slice 5, supersession and contradictions: [wo048-supersession.md](../evidence/wo048-supersession.md).
- Slice 6, the recall index and a budgeted recall: [wo048-recall-index.md](../evidence/wo048-recall-index.md).
- Slice 7, relevance, search and read: [wo048-recall-search.md](../evidence/wo048-recall-search.md).
- Slice 8, the owner's list: [wo048-memory-list.md](../evidence/wo048-memory-list.md).

## Notes

- *True as of*, contradictions as proposals, agreement with WO-052's notes, typed items and dates as dates were added on 2026-11-25, by the maintainer's decision, while the two were designed together.
- Absences were added to the scope on 2026-11-23, by the maintainer's decision, before any of this work started: a field and a test now, where a change to the sealed store's item format later would be a migration.
- Started on 2026-11-26. The maintainer approved its wire changes for the whole work order that day:
  - the kept item's broker-written record;
  - the proposal's optional fields;
  - the memory list's record and a confirm operation.

  They also decided that dates are checked in both the date fields (`YYYY-MM-DD`) and the memory's words: a relative date ("tomorrow", "next Thursday") is refused, and the agent is told to name the calendar date. The slices are on [#598](https://github.com/undergroundrap/hatchling/issues/598).
- First on v0.11.0, by the maintainer's decision of 2026-11-22. WO-045, the evaluation harness, comes after it and measures recall too.
