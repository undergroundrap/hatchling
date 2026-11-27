# WO-048 slice 6: the recall index, and a recall that reads only what it gives

Part of [#598](https://github.com/undergroundrap/hatchling/issues/598), [WO-048](../workorders/workorder-048-memory-provenance-and-recall.md), building the first half of its recall and search design, which the maintainer approved on 2026-11-27 ([#742](https://github.com/undergroundrap/hatchling/pull/742)). It builds on slices 1 to 5.

## What it is

- **A recall index per space** (`hatchling_memory::recall_index`):
  - one entry per item: id, tags, kind, absence, whether the owner approved it, kept, updated and expiry times, title (the first words, at most 60 characters), terms (its words, why and how to apply, lowercased, deduplicated, at most 32, without the commonest words), and the size of its words as a run is given them;
  - sealed beside the space's items (`memory_sealed.rs`, `recall-index`), through two new `Backing` methods whose defaults keep none;
  - kept current by the store on every write, keep, retag and remove, expiry's removals included. A put changes the index before the item and a remove after it, so a write that fails halfway leaves, at worst, an entry whose item is gone, which recall skips;
  - built once from the items for a space that has none, or whose index can't be read.
- **A budgeted recall** (`MemoryStore::recall_budgeted`; session protocol v38, `MemoryOp::RecallBudgeted`):
  - it reads each scope's index and then only the items it gives whole;
  - it chooses (`recall_index::choose`) guidance first, then the most recently changed, then by id;
  - the index lists as many entries as fit its budget, and counts the rest;
  - the items given whole are those whose words fit what's left, a large one skipped for smaller ones after it.

  The broker bounds the budgets (6 KiB index, 12 KiB whole), answers over the scopes the session may recall, gives the items without the ids of where they came from, and fits the reply to one frame.
- **The run** asks once, for a 3 KiB index and 5 KiB of whole items (`memory_provider::INDEX_BYTES`, `FULL_BYTES`). Its recalled block gives the index, one line per item (id, `KNOWN NOT TO BE SO`, kind, kept day, whether the owner approved it, and first words, all made inert), before the items given whole. Every kept memory is now in front of the agent by its first words, not only the newest ten.
- `MemoryProvider::recall_budgeted` has a default from `recall`, so other providers are as they were.

**What isn't here:** relevance to what the owner asked, and the read and search tools (slice 7).

## Tests

- **`hatchling-memory`:**
  - `recall_index::tests`:
    - a title is the first line cut at a word, counting characters;
    - terms are found words, once each, without the common ones, at most 32;
    - an entry says what its index line and choosing need, and reads back as written;
    - an entry is recalled by its tag until it expires;
    - choosing puts guidance first, then the newest, within each budget, a large item skipped for smaller ones.
  - `store::tests`, over a backing that keeps an index and counts its reads:
    - with 500 items kept, a budgeted recall reads the index once and then only the items it gives whole;
    - the index follows every change: a keep, a retag, a remove. An item gone without the index being told is skipped, and a missing or unreadable index is built once, then read.
  - `prompt::tests`: a budgeted recall gives the index first, one inert line each, saying how many it had no room for, then the items whole.
- **`hatchling-broker`, `memory_ops::paging_tests`:** a budgeted recall answers over the granted scopes, without the ids of where an item came from, within its bounds, and is denied with no recall granted.
- **`hatchling-agent`, `tests/memory_recall.rs`:** against the real broker pipeline, the run's block gives the index line and the item whole.

## The gate

RESULTS
