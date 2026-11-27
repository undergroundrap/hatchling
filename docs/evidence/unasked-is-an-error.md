# A proposal that wasn't asked is an error to the agent (#751)

Closes [#751](https://github.com/undergroundrap/hatchling/issues/751). Found during WO-048's real-model trial.

## The fault

When the broker doesn't put a proposal to the owner, it answers `MemoryResult::Unasked` or `ScheduleResult::Unasked` with a reason. That happens when:
- the proposal is unfit;
- it's already kept;
- a request already waits for that task.

`memory_propose` and `schedule_propose` (`crates/hatchling-agent/src/tools.rs`) turned that answer into an ordinary result, `{"asked": false, "reason": ...}`.

**In the trial:**
- qwen3.5:9b read that result as success four times, and told the owner "I've asked to remember…, it'll be stored once you approve it" with nothing waiting.
- Gemma 4 12B read the same results correctly.
- One model also put two things in one proposal. The description didn't say one thing per call, and a combined memory can't be corrected or forgotten in part.

## The fix

- **An unasked proposal is a tool error** (`ToolOutput::Err`), from one function, `not_asked`:
  > Not asked: <reason>. Nothing waits for the owner's approval and nothing is kept (or scheduled), so don't tell them it is: tell them it wasn't asked, and why.
- **`memory_propose`'s description** says "One thing per call: for two things, call it twice." It now ends "Tell them you asked only when it says it was asked."

The broker, the protocol and the approval rules are unchanged.

## Tests

- `tools::tests::memory_propose_asks_the_owner_and_says_what_became_of_it`: the unasked reply is exactly the error above. The description holds the one-per-call sentence and the new ending.
- `tools::tests::schedule_propose_asks_the_owner_and_says_what_became_of_it`: the same, for a schedule.
- `tests/memory_recall.rs`, `a_contradicted_memory_is_corrected_by_the_id_it_is_recalled_with`: the scripted run's refused correction reaches the model as the error, with the broker's reason.

**Hand-breaks,** each caught:
- memory's unasked reply back to an ordinary result: the memory test fails;
- schedule's unasked reply back to an ordinary result: the schedule test fails.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:
- all of `hatchling-agent`'s tests (lib, 213, and every integration binary): passed, none failed.
- clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- **Mutation** on the change (`--in-diff`): 6 mutants, 4 caught, 2 unviable, none missed.
