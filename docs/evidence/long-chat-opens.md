# Evidence: a long chat opens without freezing the page (#528)

What was tested, how, and what came back, for
[#528](https://github.com/undergroundrap/hatchling/issues/528): opening the
app with a long conversation froze the page for minutes.

| | |
|---|---|
| Base | `main` at `2b831b48` |
| Tested tree | `3299a3a3` |
| Date | 2026-11-21 (UTC) |
| Machine | x86_64, 28 threads, Windows 11; Chromium 156 |
| Node | 22.17.0 |

This record was added after the runs, in its own commit. It changes no code.

## The problem

As the app opens, the broker replays every event of the conversation, one
WebSocket frame at a time. On each one, the controller drew the whole page
again, and it drew it more than once: the adapter told it of the change, and
so did the chat's delivery and the approval review, each listening to the
same stream. Each draw holds every message so far, so it takes longer than
the last, and opening a chat cost the square of its length. On a real
install, a chat of about 90 messages blocked the page for over two minutes.

Building the app's state for each event wasn't the cost: 16 ms for all 135
events of a 45-turn chat, against 2.2 s of drawing.

## What changed

- **What Hatchling sends is drawn at the next frame, once,** however many
  events arrive before it (`FrameDraw`, `frontend/src/app/frame-draw.ts`).
  The controller hears the adapter, the chat's delivery and the approval
  review through it. Every event is still seen by the code that keeps
  track of the stream: only drawing waits.
- **What the owner does is drawn at once,** as before: sending, deciding an
  approval, and anything else the controller draws itself.
- **A command's change is on the page when `run()` returns:** it draws
  what's waiting before its caller goes on.
- **A hidden page draws nothing** (a browser runs no frames for it), and
  it draws when it's shown. Before reading whether the chat was seen, the
  page draws what's waiting, so the read is of the chat as it is.
- **The delivery and the approval review** take a second listener for what
  they hear from the stream. Without one, it's the first, as before.

## How it was measured

A page in the browser runs the built app (`dist/`) over a fake broker:
the real `LiveAdapter` and `App`, and a transport that, on `subscribe`,
replays a chat of N turns. Each turn is three events: an `api_call`
activity, the owner's message and the agent's reply, which is a paragraph,
a three-item list with bold and code, a code block and a closing paragraph.
Each event is delivered in a task of its own, as a socket delivers it:
all at once (one after another, nothing between), or 2 ms apart. The page
records the time from the broker's welcome until the last reply is on the
page, and every long task (over 50 ms) the browser reports. The same page
ran on `main` and on this change, in the same browser.

## Results

**Before**, on `main`:

| Chat | Events | Shown after | Long tasks | Main thread blocked | Longest task |
|---|---|---|---|---|---|
| 45 turns, all at once | 135 | 8.5 s | 82 | 7.0 s | 146 ms |
| 90 turns, all at once | 270 | 30.4 s | 195 | 27.7 s | 363 ms |
| 90 turns, 2 ms apart | 270 | 44.5 s | 228 | 42.2 s | 599 ms |

Twice the chat took 3.6 times as long: the square of its length.

**After:**

| Chat | Events | Shown after | Long tasks | Main thread blocked | Longest task |
|---|---|---|---|---|---|
| 45 turns, all at once | 135 | 36 ms | 0 | 0 | none |
| 90 turns, all at once | 270 | 138 ms | 0 | 0 | none |
| 90 turns, 2 ms apart | 270 | 654 ms | 0 | 0 | none |
| 300 turns, 2 ms apart | 900 | 2.1 s | 9 | 0.9 s | 159 ms |

2 ms apart, the events alone take 540 ms (90 turns) and 1.8 s (300 turns)
to arrive: the chat shows about 110 ms and 300 ms after the last one.

### In the browser, as the owner sees it

On the demo app, in the same browser:

| Check | Result |
|---|---|
| A chat renamed through `run()` | on the page when `run()` returns |
| The same with `run()`'s flush taken out | not on the page when it returns; there a frame later |
| A change from the adapter, then the page shown again | waiting for a frame, then on the page before any frame runs |
| The same with that flush taken out | not on the page until a frame runs |
| The 300-turn chat once shown | every message there, scrolled to the last |

### Tests

| Check | Result |
|---|---|
| New tests | `frame-draw.test.mjs` (4); one each in `delivery.test.mjs` and `approvals.test.mjs` |
| `npm run check` | passed: 608 unit tests |
| Coverage, run by hand from Git Bash | the app's: 608 passed, `frame-draw.js` 100% lines, branches and functions; the live adapter's: 262 passed, 100%; the operator's: 16 passed, 100% |

### Hand-breaks

Each break was built and the unit tests run against it, then undone.

| Break | Caught by |
|---|---|
| Every change asks for a frame of its own | `many changes before a frame are drawn once, at the frame` |
| A frame that ran is still counted as waiting | `a change after the frame waits for the next one` |
| Flushing draws with nothing waiting | `flushing draws what is waiting now, and its frame draws nothing more` |
| Flushing leaves its frame to draw again | the same |
| Cancelling cancels nothing | the same, and `cancelling drops the waiting frame without drawing` |
| The delivery draws at once for what it hears | `what the session hears goes to the stream listener...` |
| The delivery, given no stream listener, tells nobody | the same |
| The approval review draws at once for what it hears | `what the port hears goes to the stream listener...` |
| The approval review, given no stream listener, tells nobody | the same |

11 broken in all: these 9, caught by unit tests, and the flushes in
`run()` and on the page being shown, each caught in the browser (above).

## Residual

- **One draw of a long chat still holds every message.** At 600 messages it
  takes about 160 ms, so while a long chat is streaming, each frame that
  draws costs that much. Drawing only what changed is separate work.
- **The browser suites (`npm run test:e2e`) weren't run here:** Playwright
  isn't installed on this machine. They run in CI's `validate` job.
