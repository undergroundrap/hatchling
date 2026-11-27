# A run's memory recall is told as a read (#749)

Closes [#749](https://github.com/undergroundrap/hatchling/issues/749). Found during WO-048's real-model trial.

## The fault

WO-048 slice 6 added `MemoryOp::RecallBudgeted`, the recall every run makes at its start, and slice 7 made it the op behind `memory_search` and `memory_read`. `run_trail::tool_of` listed only `Recall`, `RecallByTag` and `ContextRead` as recalls, and sent every other memory op through a `Memory(_)` catch-all to `ToolUse::MemoryWrite`.

So every chat run's "Used:" line, and its evidence, said "Wrote to memory". That cost the trail its point: in the trial, a model said "I've asked to remember…" without calling `memory_propose`. The trail read "Wrote to memory", which looked as if something had been asked or kept.

## The fix

- `RecallBudgeted` is a recall.
- The catch-all is gone: the write ops (`Write`, `Remove`, `ContextAppend`) are named one by one, so a new op is a compile error until it's placed on purpose.

## Tests

`run_trail::tests::each_memory_op_is_told_as_a_read_or_a_write` covers every `MemoryOp` but `Propose`, which `each_request_is_the_tool_it_uses` already covers:
- `RecallBudgeted`, both a run's recall and a search;
- `RecallByTag`;
- `ContextRead`;
- `Write`, `Remove` and `ContextAppend`, as writes.

**Hand-break:** `RecallBudgeted` moved to the write arm fails this test.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:
- `hatchling-broker`'s lib tests: 1935 passed, 0 failed.
- clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- **Mutation** on the change (`--in-diff`): 2 mutants, 1 caught, 1 unviable, none missed.
