# WO-056 slice 3: the ring, the model chip and the context panel

Part of [#535](https://github.com/undergroundrap/hatchling/issues/535), [WO-056](../workorders/workorder-056-context-panel-and-model-in-use.md). Builds on slices 1 and 2 ([model fact](wo056-model-fact.md), [the broker's count](wo056-count.md)).

## What changed

**The composer** (`features/chat/chat.ts`, `contextBar`):
- The line and bar under the composer become a small **ring**, whose arc is the share of the window used. It turns to `--attention` at 90%.
- Beside it is the **model that answered**, as a chip that opens the model setting.
- The ring is a `details.menu`: the floating layer places its panel, and Escape or Tab closes it back to the ring, as every menu does. Its accessible name and tooltip are the exact numbers.

**The panel** has:
- the headline ("13% used · 8.3K of 64K") and a bar;
- Conversation, Tools, Memory and System, each marked "about";
- Free, which is exact;
- a line on how the numbers are made.

Without a window there's no arc, bar or Free row. Without the broker's split, the panel says what filled it isn't known. The words and numbers come from `features/chat/context.ts`, which has no DOM.

**Each chat its own** (`adapters/live/usage.ts`):
- the context is kept by the chat a run answered (the summary's `chat_id`, slice 2), and the composer shows only the chat in view;
- a summary from before it named its chat is the main chat's.

**The replay fix:** run summaries arriving in a burst, as a replay after a restart brings them, repaint once, `SETTLE_MS` (50 ms) after the burst. Before, the meter repainted for each and flickered through older answers. The fix needs no protocol change.

**The preview** (`adapters/scripted-usage.ts`):
- Every chat shows a sample: `gemma4:12b`, 8,320 of 65,536 tokens, split 3,100 / 2,400 / 720 / 2,100.
- Settings' usage page says the preview counts nothing.

**Docs:** `STYLE_GUIDE.md` gains "The composer's context ring", and the CHANGELOG has its entry.

## Tests

`tests/unit/context.test.mjs`:
- the share (rounded, at most 100, none without a window or with a zero one);
- the exact words, the headline and window sizes;
- the rows (about and exact, Free never below zero, and none without parts or a window);
- the note, both ways;
- the preview's port, and its sample adding up.

`tests/unit/live-usage.test.mjs`:
- `each chat has its own last answer's context, with the model and what filled it (WO-056)`;
- `a burst of run summaries, as a replay brings, repaints once when it settles (WO-056)`;
- `without a timer given, the repaint waits for the burst to settle`;
- the existing context test, now per chat, with an older summary counting as the main chat's.

`tests/unit/live-adapter.test.mjs`: the adapter test's context is the main chat's, with the new fields.

`tests/e2e/test_app.py`, `test_29b_context_ring_names_its_numbers_and_opens_what_filled_it` covers:
- the ring's name, its 13% label, and the chip's text;
- the panel's headline, rows, values and bar;
- no overflow;
- Escape closing it;
- the chip opening Settings.

CI runs it. Its steps were also driven by hand in a browser on the built demo, and each matched. That run found that Escape didn't close the panel, so the ring was given the menus' own keyboard handling, and then it did. It was also checked at phone width (375 px), with no overflow and the panel inside the screen.

## Hand-breaks

Each one fails at least one of the tests above:

1. every chat shows the main chat's context (3 fail);
2. each summary repaints at once (3);
3. the model is dropped (1);
4. the parts are dropped (3);
5. a part is said to be exact (1);
6. Free can be less than nothing (1);
7. the share passes 100 (1).

## The gate

`npm run check` (typecheck, lint, design, actions, protocol, seam, format, tests with coverage, live coverage, operator coverage) is green:
- unit tests: 640 passed, 0 failed;
- live-adapter tests: 280 passed, at 100% line, branch and function coverage;
- operator tests: 16 passed.

No Rust changed.
