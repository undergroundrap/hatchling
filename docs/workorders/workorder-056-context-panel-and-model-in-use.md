# WO-056: The context panel, and the model in use

**Status:** CLOSED
**Area:** `crates/hatchling-broker` (run trail, usage), `crates/hatchling-core` (activity and UI protocol), `crates/hatchling-prompt` and `crates/hatchling-agent` (the model fact), `frontend/` (composer ring, model chip, context panel)
**Issue:** [#535](https://github.com/undergroundrap/hatchling/issues/535)
**Trust boundary:** yes (a protocol change, and what the owner is told the agent's window holds)

## Problem

Under the composer, a line and a bar say "Context: 9.9K of 64K tokens (15%) on the last answer". It doesn't say:
- which model answered;
- what fills the window;
- how close a chat is to its limit, at a glance.

Asked which model it is, the agent answers from its training, often wrongly, because nothing tells it.

After a broker restart, the meter repaints with older runs' windows as the stream replays, before it settles. And it describes the last answer anywhere, not the chat in view.

The maintainer's direction (2026-11-19 and 2026-11-27, on #535):
- a small ring for how full the context is, with the model in use named beside it;
- the ring opens a panel with plain categories, counted by the broker and never estimated by the app;
- the agent says truly which model runs it.

Decided by the maintainer (2026-11-27): **the broker splits the model's exact total by bytes.** The categories are shown as "about"; the total and the free space are exact.

## Threats, and what stops each

| Threat | What stops it |
| --- | --- |
| **A cell misreports what fills its window**, hiding a prompt stuffed with injected text, or inflating memory to look benign. | The broker counts from the request it forwards to the model, not from anything the cell says. The total is the model server's own `prompt_tokens`. |
| **The panel claims a precision it doesn't have.** | Only the total and the free space are shown as exact. Each category is "about", and the panel says how it's counted. |
| **The agent misstates which model runs it**, or where the conversation goes. | The model fact comes from the broker's settings (the model's name and endpoint, local or hosted), given to each run as a fact, never from the model's training. For a hosted model, it says where the conversation is sent. |
| **The meter describes the wrong chat** after a replay. | A run's summary names its chat. The meter and the panel show the chat in view, and a replay repaints once it has caught up. |

## Scope

**In scope:**
- **The broker** (`run_trail.rs`, `usage.rs`): for each model call it forwards, the request body's bytes by part:
  - **System:** the system messages;
  - **Tools:** the `tools` definitions;
  - **Memory:** the user-role message that carries the recalled memory block, found by its label;
  - **Conversation:** everything else.

  The model's reported `prompt_tokens` is split in proportion, for the call with the largest prompt, which is what the meter already shows. The run summary carries that split, the model's name, and its chat.
- **`hatchling-core`:** `RunSummary` gains `context_parts` (system, tools, memory, conversation, in tokens), `model` and `chat_id`. This is a new UI schema version; see the wire shape below.
- **The model fact** (`hatchling-prompt`, `hatchling-agent`): each run's instructions name the model, and whether it runs on this machine or at a named host (with "this conversation is sent to <host>" for a hosted one), from the settings the broker gives the run. Asked which model it is, the agent answers with that, as Hatchling.
- **The app:**
  - The composer's line becomes a small ring, with the exact numbers on hover, on focus and in its accessible name, and the model's name beside it as a chip.
  - The chip opens the model setting.
  - The ring opens the panel:
    - a bar and the total ("91% used, 912K of 1M");
    - System, Tools, Memory and Conversation as "about" numbers, and Free;
    - a line saying how it's counted.
  - Designed in the demo first, then wired live.
- **The replay fix:** the meter takes only the summary of the chat in view, and repaints once the replay has caught up.
- **Docs:** `local-api.md`, `session-protocol.md` (if the model fact is documented there), the CHANGELOG.

**Not in scope:**
- Compaction, and "Compact now" (the continuity work).
- What a full window costs per turn for a hosted model (#534's hosted keys come first).
- Switching between configured models from the chip, past opening the existing setting (#534).

## Wire shape (approved by the maintainer, 2026-11-27)

```text
ActivityEvent RunSummary (broker -> app), UI schema v16:
  + model: string | null           the model's name, as the broker's settings say
  + chat_id: string | null         the chat the run answered, for a chat run
  + context_parts: {               the largest call's prompt, split by bytes
      system: u64, tools: u64, memory: u64, conversation: u64
    } | null                       (tokens; their sum is peak_input_tokens)
```

The session protocol is unchanged: the model fact reaches the agent through the settings the broker already gives it.

## Acceptance criteria

- [x] A run's summary carries the broker's split of its largest prompt, with system, tools, memory and conversation summing to the model's reported total. It's counted from the forwarded request, whatever the cell says. ([count](../evidence/wo056-count.md): `the_total_is_split_by_size_and_summed_exactly`, `the_largest_prompt_is_split_from_the_request_forwarded`, the seam `an_answered_model_call_is_counted`)
- [x] A request with no memory block counts no memory. One with tools counts them under Tools, not Conversation. ([count](../evidence/wo056-count.md): `each_part_of_a_request_is_measured_where_it_is`; live, [trial](../evidence/wo056-trial.md): Memory 0 before anything was kept)
- [x] The composer shows a ring and the model's name. The exact numbers are on hover, on focus and in the ring's accessible name. The chip opens the model setting. ([panel](../evidence/wo056-panel.md): `test_29b`; live in the [trial](../evidence/wo056-trial.md))
- [x] The ring opens a panel with the bar, the exact total, and the four categories marked "about" and Free. It says how they're counted. ([panel](../evidence/wo056-panel.md); live in the [trial](../evidence/wo056-trial.md))
- [x] Asked which model it is, the agent names the model from the broker's settings, says whether it's local or hosted at a named host, and answers as Hatchling. ([model fact](../evidence/wo056-model-fact.md); live, [trial](../evidence/wo056-trial.md): "I'm Hatchling, made by Ocean Bennett, running right now on gemma4:12b." The local or hosted half is proven by `the_model_note_names_the_model_and_where_its_words_go`; a hosted model wasn't tried live, since the side install has no key.)
- [x] After a broker restart, the meter shows the chat in view and doesn't flicker through older runs. ([panel](../evidence/wo056-panel.md): per-chat context, and `a burst of run summaries, as a replay brings, repaints once when it settles`. Shown by test; a live restart wasn't staged, since restarting the side broker re-keys its app.)
- [x] Each change has its evidence record under `docs/evidence/` (below).

## Gates

All must pass before the WO closes:

- [x] `bash scripts/check.sh` green on the pinned toolchain **and** 1.85.0: CI's gate on #759, #760 and #763. Locally, each slice ran the workspace's tests and clippy on both toolchains.
- [x] Real-cell tests ran rather than skipped (`HATCHLING_REQUIRE_CELL=1`), on the WSL2 sandbox.
- [x] **Mutation, kill floor 1.0** on each slice's change: slice 1, 10 mutants (8 caught, 2 unviable); slice 2, 26 (24, 2); slice 3, TypeScript, hand-broken 7 ways and at 100% live coverage. None missed.
- [x] `cd frontend && npm ci && npm run check`: green on slices 2 and 3.
- [x] Wire types regenerated and committed (slice 2, and again after CI's drift check caught a reworded description).
- [x] Docs that describe the changed behavior updated in the same change: the activity contract, `arch/protocol.md`, `arch/distribution.md`, `STYLE_GUIDE.md` and the CHANGELOG.

## Evidence

- Slice 1, the model fact: [wo056-model-fact.md](../evidence/wo056-model-fact.md).
- Slice 2, the broker's count: [wo056-count.md](../evidence/wo056-count.md).
- Slice 3, the ring, the chip and the panel: [wo056-panel.md](../evidence/wo056-panel.md).
- The real-model trial: [wo056-trial.md](../evidence/wo056-trial.md).

## Notes

### Slices

1. **The model fact.** Each run's instructions name the model, and whether it's local or hosted, from the broker's settings.
2. **The broker's count.** The split by part, `model`, `chat_id` and `context_parts` on the run summary, UI schema v16.
3. **The app.** The ring, the chip and the panel, demo first, then live, with the replay fix.
