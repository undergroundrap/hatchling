# WO-058: The memory loop

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-broker` (the clock's first loop, the seat launch, apply), `crates/hatchling-core` (the loop's claims, the UI), `crates/hatchling-prompt` (the loop's prompt), `crates/hatchling-memory` (a record's fingerprint), `frontend/` (the Memory page)
**Issue:** [#782](https://github.com/undergroundrap/hatchling/issues/782)
**Trust boundary:** yes (the owner's memory changed while nobody is there)

## Problem

WO-057 keeps what the owner tells the agent at once. That makes memory grow, and nothing tends it afterwards:
- **Duplicates pile up:** "Pepper is a cat" kept in March, and again in a new conversation in May.
- **Facts go stale:** "they live in Bath" stays after the owner said, in passing, that they'd moved.
- **Things the owner said are missed:** a small model asks to remember only some of it.

Claude's own memory is tended between sessions. The maintainer wants Hatchling's to beat it on every axis, cleanup included (2026-11-27). The order puts the memory loop next, before links, recall by meaning and compaction.

WO-054 built the parts for background work, none of them connected yet:
- the background clock;
- the seat checks (`hatchling_core::seat`: handles, a snapshot, `finish`, the one repair, the gate record);
- the seat's cell (`hatchling_agent::seat`);
- the review queue.

The memory loop is the first to run on them. It proves them end to end, as WO-054's slice 5 says.

**What's missing, found reading the code (2026-11-28):**
- The broker never launches a seat, builds a snapshot, or applies a seat's output. WO-054's slice 3c was folded into this work order.
- The clock is started with no loops (`run_launcher.rs`: `Clock::new(Vec::new())`, `NoLoops`). Nothing tells it about activity, and the pause is hard-coded to off: the owner's pause (#673) isn't built yet.
- **A seat can't be handed to a cell.** A cell gets its work in its cell config (`ScheduledRun`), as argv and environment, and answers on stdout. Nothing there carries a seat's packet, and the agent has no seat command. `hatchling_agent::seat::run` is called only by its tests.
- **The conversation store keeps one current conversation,** as exchanges (the owner's message and the agent's answer), numbered by slot, with no ids or times. Its taint is per conversation, not per message.
- **There's no memory prompt kind:** `PromptKind` has six, and the memory sections are waiting.
- **A memory record has no fingerprint,** so "changed between snapshot and apply" can't be checked. `updated_at_secs` isn't restamped by `rerecord` or `retag`.
- **The #714 prompt describes `MEMORY.md` and `USER.md` files and dated logs,** which Hatchling doesn't have. It keeps items in a store. The prompt has to be adapted to items and to the claims below. Its rules carry over: evidence only, merge duplicates only, age alone is no reason, and a sensitive value never kept.

## The rule

The model only proposes. Code checks every claim against the run's snapshot and decides what's applied, what waits for the owner, and what's dropped.

**What the loop may propose: four claims, each citing its evidence by handle:**

| Claim | What it does | Applied without asking when | Otherwise |
|---|---|---|---|
| **Merge** | Two or more memories saying the same thing become one. | The kept words are **one of the duplicates' own words, verbatim** (code checks it), and every duplicate is still as snapshotted. The others are retired (`superseded`), never deleted. | Rewording, or merging memories of different kinds: dropped. |
| **Correct** | A memory the owner's later words contradict is replaced. | It cites the owner's own message (origin `Owner`) with a quote, and the memory is about their work or where something is (`work`, `pointer`), or was kept automatically. The old one is retired. | About the owner or how they want it to work (`owner`, `guidance`), and approved by them: waits on the Memory page (`replace`). |
| **Check** | A memory that may no longer be so is put to the owner: "Is this still so?" (WO-048's due check). | It cites why (a later message, its age with a reason, a dated fact). It only sets the day to ask: nothing changes until they answer. | — |
| **Keep** | Something the owner said and the agent didn't ask to keep. | It cites the owner's own message with a quote that says it, and the conversation it's from read nothing from outside (WO-057's taint, per conversation: a conversation that read from outside at any point counts as having read it). Kept as automatic, counted in the day's 20. | From a conversation that read from outside: waits on the Memory page (`remember`). Citing only the agent's words: dropped. |

**Never, by code:**
- **A removal.** The loop retires only what it merges or corrects. Nothing is forgotten without the owner.
- **Anything whose every citation is from outside** (a page, a tool's output, someone else's message). The seat check already sorts these out (`outside_only`), and they're dropped.
- **A sensitive value.** A claim whose words the secret scanner flags is dropped, as a card's are.
- **Words the owner denied** (WO-057's refusals).

## Threat model

WO-054's threats all hold. These are the memory loop's own:

| Threat | What stops it |
|---|---|
| **Poisoning laundered through the loop:** a page the agent read is quoted in its answer, and the loop keeps it as the owner's | Keep and Correct need a quote from a message whose origin is `Owner`, by the conversation store, never the agent's words. The agent's answers are origin `Agent`, and a claim citing only those is dropped. |
| **The loop rewrites what the owner approved** | A Merge keeps one duplicate's own words, verbatim. A Correct of an owner-approved memory about them or their guidance waits for them. |
| **Memory shrinks:** a model "condenses" away what matters | No removals. Retired memories are kept, tagged `superseded`, and shown on the Memory page as replaced, with what replaced them. |
| **A run applies to memory that changed under it** | Each memory in the snapshot carries a fingerprint: SHA-256 over its words, tags and record. Apply recomputes it and skips, counted, any that changed. A waiting item whose target changed is shown `stale`, and approving it does nothing. |
| **A runaway loop** | One run at a time, chat first, and paused with everything else. Each run has a token budget, and Keep shares the day's 20 with WO-057. A run with no new conversation and no memory due spends nothing: the clock's activity gate. |
| **The model invents a memory id or a quote** | The seat check: one unresolved citation refuses the whole output, with one repair. |

## Design

### Running

- **The clock's first loop:** `memory`, cadence `AfterActivity` (an hour after the owner's last chat). Each chat run's end tells the clock there was activity. The pause holds it once #673 builds the pause: the clock's gate already honours it.
- **The snapshot,** taken in one pass when the run starts:
  - every memory the agent is given (the recall tag), each a `Record` with handle `memory:<scope>/<id>`, origin `Owner` for owner-approved memories and `Agent` for automatic ones, its words, and its fingerprint;
  - the conversation since the loop last ran: each exchange's owner's message (`Owner`, `message:<conversation>/<slot>-owner`) and the agent's answer (`Agent`, `message:<conversation>/<slot>-agent`). The loop keeps the last slot it read, under its own state;
  - the conversation's taint.
  - Bounded by the seat's limits (256 records, 32 KiB of brief). Past them, the oldest conversation is left for the next run.
- **The seat:** a cell launched by the broker as a run's is, its packet in its cell config (`ScheduledRun` gains `seat`) and an agent `seat` command that runs `hatchling_agent::seat::run` and answers on stdout. It's given `PromptKind::Memory`, a seventh kind (its sections: the adapted reconciliation section and its four memory siblings). Its tools are the seat's: read the records, read one, and `finish` with the four claims' schema.
- **Apply:** one serial pass in the broker, in the order Merge, Correct, Keep, Check. It rechecks fingerprints and uses the existing writes (`keep_as`, `retire`, `rerecord`). Each change is recorded with the run's id, so the record says the loop made it.
- **The gate record:** counts proposed, applied, waiting, dropped, stale and the tokens spent. Never text.

### The record

- `KeptRecord` gains `merged: Vec<MemoryId>` (optional, left out when empty): the memories a Merge retired into this one.
- A memory the loop keeps or corrects has `kept_by: automatic`, and its `run` names the loop's run. A loop's run is told apart from a chat's by `chat` being none.

### The app

- **The Memory page** says when the memory loop last ran and what it did: "Tidied 2 hours ago: merged 2, corrected 1, asked about 1."
- Each memory it changed is labelled, with the memories it replaced.
- What waits for the owner is on the page already (WO-057's Review section).
- The background setting gains the memory loop's own switch (WO-054's per-loop setting).

## Wire shape (for the maintainer's approval)

```text
UI schema v20:
  a kept memory's record
    + merged: [<listed id>]           what this one replaced as duplicates
    + by_loop: true                   kept or changed by the memory loop
  manage_memory
    + {"op": "loop"}                  -> {"result": "loop", "last_run": <unix secs>|null,
                                          "merged": n, "corrected": n, "kept": n, "asked": n, "waiting": n}
    + {"op": "set_loop", "on": bool}  the owner's alone
```

No session protocol change: the seat's packet rides in its cell config, as a run's task does, and its model is reached through the egress session every run uses.

## Scope

**In scope:** the four claims, the fingerprint, the clock's first loop with activity, launching a seat, apply, the gate record, the adapted prompt, the Memory page's report and labels.

**Not in scope:**
- **The owner's pause** (#673): its own issue. The clock already holds every loop when it's set.
- **Links between memories** (#740): next in the order, on this record.
- **Recall by meaning**: after links.
- **Removing memories**: the owner's alone.
- **The other three loops** (goals, alignment, Ideas): their own work orders, on this runner.

## Slices

1. **The loop runs, and applies nothing:** the fingerprint, the clock's `memory` loop with activity, the snapshot, the seat's cell config and command, `PromptKind::Memory`, the four claims' schema and check, and the gate record. Each run's claims are counted and logged as "would apply" or "would wait". This proves the runner end to end before anything changes memory.
2. **Apply:** each claim's rule above, the queue for what waits, `stale` on a changed target, and the record's `merged`.
3. **The app:** the report, the labels and the switch (UI v20).
4. **A real-model trial on the demo,** with the maintainer's yes: seeded duplicates merge, a moved city is corrected or waits, a stale fact is asked about, a page's injected "remember …" is dropped. Then close.

## Acceptance criteria

- [ ] No seat writes memory: only apply does, after the check.
- [ ] A Merge applies only with one duplicate's words verbatim; a rewording is dropped.
- [ ] A Correct or Keep applies only with a quote from the owner's own message; one citing only the agent's words, or only outside, is dropped.
- [ ] A Correct of an owner-approved `owner` or `guidance` memory waits on the Memory page and is never applied without them.
- [ ] Nothing is removed; what's retired stays, tagged `superseded`.
- [ ] A memory changed between snapshot and apply is skipped and counted; its waiting item turns `stale`.
- [ ] Keep counts in the day's 20, and a conversation that read from outside makes it wait.
- [ ] The loop runs an hour after activity, never during a chat run, and spends nothing with nothing new. (Never when paused comes with #673.)
- [ ] The Memory page says when it last ran and what it did.
- [ ] Each slice has its evidence record, with hand-breaks of each rule above.

## Gates

The usual: fmt, clippy on both toolchains, the whole workspace's tests, the audit, hand-breaks for each rule above, mutation on the changed lines (kill floor 1.0), the frontend check, the wire types regenerated, and the docs in the same change. Then a real-model trial.

## Evidence

Each slice commits its evidence under `docs/evidence/`.

## Notes

- **The maintainer's decisions (2026-11-27): approved as proposed,** with the UI v20 wire shape above:
  1. **Which corrections wait.** As proposed, one of an owner-approved `owner` or `guidance` memory waits. `work`, `pointer`, and anything kept automatically are corrected without asking, from the owner's own quoted words. The owner's own correction in a clean chat is already kept at once (WO-057).
  2. **Keep in the loop at all.** It catches what a small model didn't ask for. It's the most powerful claim, and the one most open to laundering. As proposed, it needs the owner's quoted words from a clean conversation, inside the day's 20.
  3. **The cadence:** an hour after the last chat.
