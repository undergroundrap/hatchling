# WO-048 slice 7: recall by relevance, search and read

Part of [#598](https://github.com/undergroundrap/hatchling/issues/598), [WO-048](../workorders/workorder-048-memory-provenance-and-recall.md), building the second half of its recall and search design, which the maintainer approved on 2026-11-27 ([#742](https://github.com/undergroundrap/hatchling/pull/742)). It builds on slice 6.

## What it is

- **What the run is for, as the broker received it,** ranks its recall. The broker sets `FollowupScope::ask` when it starts a run, from the run's task: the owner's message for a chat turn, or the scheduled task. Serving the run's `RecallBudgeted`, it ranks by that (`serve.rs`).
  - The run doesn't supply it, so a page the run reads can't steer what it's given.
  - A search's own words rank only what the owner's memory already holds.
- **Relevance** (`recall_index::scores`) is BM25 over each entry's terms, each term once (k1 1.2, b 0.75). Guidance and work facts add the weight of each asked word their *how to apply* names, kept in the entry (`apply_terms`, at most 16). It's deterministic, and needs no network and no model.
- **The order** (`recall_index::choose`):
  - guidance first;
  - then the best match;
  - then the most recently changed, then by id.

  With nothing asked, it's slice 6's order.
- **Search and read** are the same op, narrowed (session protocol v39, `RecallBudgeted`'s `query` and `id`):
  - `memory_search { query }` gives the index lines that best match, within 2 KiB;
  - `memory_read { id }` gives one recalled item whole, within 2 KiB. Otherwise it says the item isn't recalled, or that it's longer than a read gives.

  Both answer in the recalled-memory block, labelled as data. Their 16 uses in a run are shared (`MemoryLooks`), and once used they say so.
- **The agent's guidance** says it's given an index of all its memory and the most relevant items whole, and that it can read and search the rest.

## Tests

- **`hatchling-memory`:**
  - `recall_index::tests`:
    - an ask is matched by its words;
    - entries are scored by how well they match: a holder above a non-holder, a rare word above a common one, and guidance and work boosted by what their *how to apply* names;
    - choosing ranks by the ask after guidance.
  - `store::tests`: what the owner asked finds a memory outside the newest ten, which isn't given unasked; a read gives the one item it names, and nothing for an id not kept. This is the work order's acceptance criterion for search.
- **`hatchling-broker`:**
  - `memory_ops::paging_tests`:
    - a search ranks by its words;
    - what the run is for ranks the rest;
    - a search's own words come before the run's;
    - a read gives one item, and nothing for an id not kept;
    - a malformed id ends the session.
  - `serve::recall_ask_tests`: through a real session, a run's recall is ranked by the ask the broker received, an older match before newer notes.
- **`hatchling-agent`, `tools::tests`:**
  - `memory_search` and `memory_read` send their words and id;
  - they answer as recalled data, and say why when there's nothing to give;
  - they share their bound of 16.

## The gate

RESULTS
