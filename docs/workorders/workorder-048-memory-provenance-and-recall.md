# WO-048: Memory that knows where it came from: provenance, supersession, bounded retrieval, then search

**Status:** IN-PROGRESS
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

## Recall and search design (slices 6 and 7)

Written on 2026-11-27 for the maintainer's approval before any code, as the scope requires. **The maintainer approved it as written on 2026-11-27**, with the budgets below.

**Where recall stands.** A run is given the user and project items tagged `recall`, newest first, cut at 10 items and 8 KiB:
- the store reads and decrypts every item in the scope to find them;
- an item outside the newest ten is never seen, however relevant;
- the agent can't ask for more.

**What it has to do:**
- every kept memory is reachable;
- recall never reads more than it can give;
- relevance to what the owner asked decides what a run sees in full;
- nothing goes anywhere memory doesn't go today: no embeddings service, no model call to rank;
- small local models can use it;
- each part is testable.

### Two tiers: an index always, the full words where they matter

1. **The index.** One line per recalled item, given to every run within 3 KiB (about 60 items):
   - its id;
   - `KNOWN NOT TO BE SO` for an absence;
   - its kind and kept date;
   - its title, the first words of the memory (at most 60 characters), made inert as recalled data is now.

   Guidance comes first, since a rule matters wherever it applies, then items by relevance, then the newest. When there are more items than fit, the index says how many it leaves out and that `memory_search` finds them.
2. **The full items.** Within the remaining 5 KiB, the most relevant items are given whole: words, why, how to apply, check-again day, provenance header. This is today's block, chosen by relevance instead of age.
3. **Two read tools, both owner-scoped and labelled as data:**
   - `memory_read { id }` gives one recalled item whole;
   - `memory_search { query }` gives the index lines of the items that best match `query`, at most 20.

   A run may read or search 16 times in all, each answer at most 2 KiB. Only items recall would give can be read: an id nothing is recalled with is said so, as a correction's is now.

### The store never reads more than it gives (slice 6)

- **A recall index per space.** It's sealed beside each space's existing index of ids, written by the broker on every write, keep, retag and remove, value first and index after, as now. Each entry holds:
  - id, kind, absence, kept and updated times;
  - the title;
  - the item's terms: its words, why and how-to-apply, lowercased, split on non-letters, without a short stop list, deduplicated, at most 64.

  It's split into segments of 64 entries, so a segment always fits one sealed entry, as WO-047 split large items.
- **One recall op carries the budget.** It's a new `MemoryOp::Recall { query, index_bytes, full_bytes }` (session protocol v38). The broker:
  1. reads the recall index segments;
  2. scores each entry;
  3. builds the index within `index_bytes`;
  4. reads only the items it gives whole within `full_bytes`.

  The agent asks once, at the run's start, and the read tools use the same op with a narrower ask. The old `RecallByTag` stays for compatibility, with no change.
- **Rebuilt when missing.** A space with no recall index (0.10's, or one a crash left behind) has it built once from its items. That's the only time every item is read, and the next recall reads the index.

### Relevance, without a model (slice 7)

- **The query** is what the run is for: the owner's message for a chat turn, or the task for a scheduled run, as the broker received them. The run doesn't supply it, so a page the run read can't steer what it's given. A read tool's `query` is the run's own, and it only ranks what the owner's memory already holds.
- **The score:**
  - BM25 over the entries' terms, so rare shared words count most;
  - plus a boost for guidance and work facts whose `how_to_apply` terms match;
  - and a small boost for recency.

  It's deterministic, needs no network, and can be tested against fixed fixtures. Ties go to the newest, then the id.
- **Later, optionally:** embeddings from the owner's own local model endpoint, ranked the same way. That's a separate decision, because it sends memory to the model server. It's not in this work order.

### Tests that hold it to this

- **Budget at the store:** with 500 kept items, recall's backing reads are counted. The segments and the items given whole are read, and nothing else.
- **Relevance:** an item outside the newest ten that matches the owner's ask is given whole, and one that doesn't isn't.
- **The index:** every item's line is given while they fit. Past the budget, the count of the rest is given and `memory_search` finds a left-out item by its words.
- **Reads:**
  - `memory_read` gives a recalled item whole;
  - it refuses an id nothing is recalled with, a superseded item and another scope's;
  - the 16-read bound holds.
- **The index is kept current:** a write, a keep, a retag and a remove each change it, and a crash between value and index leaves recall right, either skipping an entry whose item is gone or rebuilding.
- **Compatibility:** a space without a recall index is rebuilt once, and 0.10's items are indexed as not recorded.
- **Quality:** WO-045 measures recall on real models, because the tests above prove the mechanism, not how often a model uses it well.

### What it decides, for approval

- Two tiers: an index always given, and the full words chosen by relevance.
- `memory_read` and `memory_search`, bounded per run.
- A sealed recall index per space, kept by the broker, so recall reads only what it gives.
- A new recall op carrying the budget (session protocol v38).
- Lexical relevance (BM25) on what the broker received, no embeddings.
- The budgets: 3 KiB index, 5 KiB full, 16 reads of 2 KiB.

## Acceptance criteria

- [ ] A kept item's provenance is written by the broker alone: a test has the agent's proposal claim another origin and finds the broker's record unchanged.
- [ ] A correction approved as superseding an item stops that item being recalled, and the chain is shown; a test covers recall before and after.
- [ ] A proposal matching a kept item isn't put to the owner again and isn't kept twice (#596); a test proposes the same words twice.
- [ ] A kept absence is recalled as an absence, marked as one, never as a missing item; a test recalls one beside a query that finds nothing.
- [ ] Recall reads no more than its budget from the store; a test with many items checks what's read.
- [ ] Search's design is written and approved by the maintainer before its code; recall by relevance then finds an item outside the newest ten, in a test.
- [ ] Each item is kept with its kind, and guidance and work facts with their why and how to apply; recall gives them; a test keeps one of each kind.
- [ ] A proposal whose date isn't a calendar date is refused, and an approved one keeps the date the owner was shown; a test proposes "next Thursday" as text.
- [ ] Recall gives each item with when it was kept and last confirmed, from the broker's record; a test has a proposal claim another date and finds the broker's unchanged.
- [ ] A run whose tool results contradict a recalled item proposes a correction naming it, with the contradiction quoted, and the owner's approval supersedes the item. A test runs a scripted model through it, and WO-045 measures how often real models do.
- [ ] Items from 0.10 load, and say their provenance isn't recorded.
- [ ] Each new guard fails its test when removed.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Each slice commits its evidence under `docs/evidence/`.

- Slice 1, the record: [wo048-kept-record.md](../evidence/wo048-kept-record.md).
- Slice 2, no duplicates: [wo048-no-duplicates.md](../evidence/wo048-no-duplicates.md).
- Slice 3, typed items and dates: [wo048-typed-items.md](../evidence/wo048-typed-items.md).
- Slice 4, absences: [wo048-absences.md](../evidence/wo048-absences.md).
- Slice 5, supersession and contradictions: [wo048-supersession.md](../evidence/wo048-supersession.md).

## Notes

- *True as of*, contradictions as proposals, agreement with WO-052's notes, typed items and dates as dates were added on 2026-11-25, by the maintainer's decision, while the two were designed together.
- Absences were added to the scope on 2026-11-23, by the maintainer's decision, before any of this work started: a field and a test now, where a change to the sealed store's item format later would be a migration.
- Started on 2026-11-26. The maintainer approved its wire changes for the whole work order that day:
  - the kept item's broker-written record;
  - the proposal's optional fields;
  - the memory list's record and a confirm operation.

  They also decided that dates are checked in both the date fields (`YYYY-MM-DD`) and the memory's words: a relative date ("tomorrow", "next Thursday") is refused, and the agent is told to name the calendar date. The slices are on [#598](https://github.com/undergroundrap/hatchling/issues/598).
- First on v0.11.0, by the maintainer's decision of 2026-11-22. WO-045, the evaluation harness, comes after it and measures recall too.
