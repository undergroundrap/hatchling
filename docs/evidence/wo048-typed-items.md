# WO-048 slice 3: typed items, why and how to apply, dates as dates

Part of [#598](https://github.com/undergroundrap/hatchling/issues/598), [WO-048](../workorders/workorder-048-memory-provenance-and-recall.md). It builds on slices 1 and 2. The maintainer approved the wire change, and the dates rule (date fields and the memory's words), on 2026-11-26.

## What it is

- **A proposal says what kind of thing it is** (session protocol v35, `MemoryOp::Propose`'s `memory_kind`). It's one of:
  - `owner`: about the owner;
  - `guidance`: how they want the agent to work, a correction or a confirmed approach;
  - `work`: a fact about ongoing work;
  - `pointer`: where something lives.

  It's required. The field isn't named `kind`, which names the request's family on the wire.
- **Guidance and work facts say why and how to apply**: the reason the owner gave, and when it matters. Each is at most 500 bytes.
- **A day to check it again** (`check_by`) is optional: a real calendar day after today, `YYYY-MM-DD`. A day that doesn't exist (2026-02-30), today, a past day or another shape is refused.
- **Dates as dates.** The words, the reason and when it applies may not use a relative date. That covers:
  - `today`, `tonight`, `tomorrow` and `yesterday`;
  - `next`, `last`, `this` or `coming` before a weekday, `week`, `weekend`, `month` or `year`;
  - `in` with a count and a span ("in two weeks", "in 3 days");
  - a count and a span before `ago` or `from now`.

  The agent is told the phrase it used and to name the calendar date. Words like "weekly", "the next step" or "In Europe" pass.
- **The checks live in `memory_proposal.rs`** and run before anything is put to the owner. A failed check is `Unasked { reason }`, so the agent can say it again properly. Words that aren't fit to keep (empty, too long, control characters) are still `MemoryError`.
- **The owner's card:**
  - a question that fits the kind ("Remember how you want the agent to work?");
  - what kind of thing it is, in the broker's words;
  - the reason, when it applies and the day, each as the agent's words.

  The approval freezes all of them, so the digest the owner approves covers them. Absent ones are left out, since the frozen form has no nulls.
- **Kept**, they're in the item's record (slice 1's fields `kind`, `why`, `how_to_apply`, `check_by`).
- **Recall** gives a kept item's kind in its header, and its reason, when it applies and its day after its words, inside its length frame and with the block's markers made inert.
- **The agent's tool** (`memory_propose`) takes `kind` (required), `why`, `how_to_apply` and `check_by`. Its description and the standing guidance say to propose a correction as guidance and to name dates as calendar dates.

**What isn't here:** absences (slice 4), supersession (slice 5), and asking the owner to confirm an item when its day comes (slice 8, with the memory list).

## Tests

- **`hatchling-broker`, `memory_proposal::tests`:**
  - each kind is checked, and guidance and work need both their reasons;
  - words and reasons are bounded, at the bound and one past;
  - a relative date is refused in the words, the reason and when it applies, and ordinary words with the same tokens pass;
  - a day to check again is a real day after today;
  - days are counted from the epoch, leap days included.
- **`hatchling-broker`, `memory_keep::tests`:**
  - guidance is shown with its kind, reason, when it applies and day, each from its right source, and is kept with them in its record;
  - a relative date isn't put to the owner.

  The earlier tests now find the card's fields by label, and the records carry their kind.
- **`hatchling-memory`, `prompt::tests`:** a kept item says its kind, why, when and day in recall, each kind in its words, and its reasons can't break out of the block.
- **`hatchling-agent`, `tools::tests`:** the tool needs `kind`, and sends `memory_kind` with the reasons and day on the wire.

## The gate

RESULTS
