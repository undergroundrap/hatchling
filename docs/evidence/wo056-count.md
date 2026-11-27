# WO-056 slice 2: the broker counts what fills the window

Part of [#535](https://github.com/undergroundrap/hatchling/issues/535), [WO-056](../workorders/workorder-056-context-panel-and-model-in-use.md). Builds on slice 1 ([wo056-model-fact.md](wo056-model-fact.md)).

## What changed

**`crates/hatchling-broker/src/prompt_parts.rs`, new:**
- `measure(body)`: the bytes of each part of a chat completion request, as the broker forwards it:
  - **system:** the system or developer messages;
  - **tools:** the `tools` definitions;
  - **memory:** the user message whose content begins with recall's `[RECALLED-MEMORY]` label;
  - **conversation:** everything else, including a tool's result that copies the label.

  A body that isn't a chat request (not JSON, or no `messages` list) is `None`.
- `split(total, bytes)`: the server's `prompt_tokens`, split in proportion. It's by largest remainder (the earlier part on a tie), in `u128`, so the parts sum to `total` exactly and nothing overflows. A request with no bytes is all conversation.

**The trail** (`run_trail.rs`):
- On each counted call whose prompt is at least the largest yet, it keeps the split of the request it forwarded. A smaller call later leaves it.
- Its summary carries `context_parts`, `model` (the name from the broker's settings, which the trail already had for asking the server its window), and `chat_id` (`Trail::in_chat`).

**`serve.rs`:** the session's trail is told its chat by `chat_of`, the one function that also says which chat a memory proposal came from, so they can't disagree.

**The wire** (`hatchling-core`), as the maintainer approved on 2026-11-27:
- `ContextParts { system, tools, memory, conversation }`;
- `SubagentEvent::RunSummary` gains `model`, `chat_id` and `context_parts`, each left out when unknown;
- UI schema v16.

**Docs:**
- `activity-monitor-event-contract.md`: the three fields, and how the split is made;
- `arch/protocol.md` and `arch/distribution.md`: v16;
- the CHANGELOG.
- The app's generated types are regenerated. The app uses the fields in slice 3.

## Tests

- `prompt_parts::tests`:
  - `each_part_of_a_request_is_measured_where_it_is`: system, tools, the memory block, and the rest, with a tool result that copies the label counted as conversation, a developer message, and a request with neither tools nor memory.
  - `what_isnt_a_chat_request_isnt_measured`.
  - `the_total_is_split_by_size_and_summed_exactly`: exact shares; a three-way tie going to the earlier parts; the largest remainder taking the rounding; zero; no bytes; and `u64::MAX` everywhere without overflow.
- `run_trail::tests::the_largest_prompt_is_split_from_the_request_forwarded`:
  - the parts sum to the server's 1000, and system > memory > conversation;
  - the model and the chat are named;
  - a smaller call later leaves the split, and a larger one replaces it;
  - an unreadable body leaves none.
- `serve::tests::an_answered_model_call_is_counted`, the seam: an egress op's body flows through `handle_egress_op` into each trail. A request with no messages counts all 11 tokens as conversation.
- `hatchling-core`: `a_run_summarys_model_chat_and_context_parts_have_their_wire_form` covers the three fields on the wire, left out when unknown, and a summary from before v16 reading as none. The version pin is 16.

## Hand-breaks

Each one fails at least one of the tests above:

1. memory isn't known by its label;
2. a tool's result can pass for memory;
3. tools aren't counted;
4. the rounding is dropped;
5. the smallest remainder takes the rounding;
6. a smaller call replaces the largest's split;
7. the summary drops the split;
8. the summary names no chat.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:
- **The whole workspace's tests:** 4702 passed, 0 failed.
- **clippy** with `-D warnings`, the whole workspace, on 1.98.1 and 1.85.0: clean.
  - The first run stopped at a doc paragraph in `hatchling-core`. Past it, three more were in the broker:
    - two doc paragraphs;
    - `chat_of` placed between `serve_session`'s `#[allow]` and the function itself, which moved that allow off it;
    - the seam test, two code lines over its limit, now marked `#[expect(clippy::too_many_lines)]` with its reason.
  - After those: broker lib 1955 passed, core 260.
- `hatchling-dev audit`: all checks passed. `protocol-schema --write`: all checks passed, with the schema and the app's generated types committed.
- **Hand-breaks:** all eight caught.
- **Mutation** on the change (`--in-diff` against slice 1, `hatchling-broker` and `hatchling-core`): 26 mutants, 24 caught, 2 unviable, **none missed**.
- **The app:** `npm run check`, green with the new types.
  - Unit tests: 633 passed.
  - Live-adapter tests: 277 passed, at 100% coverage.
  - Operator tests: 16 passed.
