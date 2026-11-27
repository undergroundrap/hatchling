# The covered list's own call no longer asks for it again (#747)

Closes [#747](https://github.com/undergroundrap/hatchling/issues/747). Found during WO-048's real-model trial, on a side install of `e29ff192`.

## The fault

A feedback loop between the broker and the app, shipped in 0.9.0 with #547:

1. Every Local API call is kept as an `api_call` activity event under the run id `api` (`ApiLog::record` in `crates/hatchling-broker/src/api_ops.rs`), appended to `<root>.activity` and streamed to the app.
2. The live adapter called `LiveCovered.observe()` for **every** activity event (`frontend/src/adapters/live/adapter.ts`).
3. `observe()` sends `list_covered`. That is an API call, kept as a new activity event, which triggers step 2 again.

`refresh()` allows one request in flight and one queued, so the loop ran at the speed of a round trip.

**Measured with the app open:**
- 481 `list_covered` calls in one minute;
- `schedules.activity` at 1.27 MB after about ten minutes.

That is about 300 MB a day of activity log, plus one journal line per call. Each answer also called `changed()`, so the page redrew several times a second. Browser automation in the trial had clicks fail: twice with "ref is stale (element removed)", and once with "could not be attributed to a frame".

## The fix

`LiveCovered.observe(event)` refreshes only for events whose `run_id` isn't `api`, the broker's `API_RUN_ID`.

- **Covered requests are a run's only:** a standing grant covers a cell's egress or browser request. So nothing the Local API connection does can add one, and skipping its events loses nothing.
- **Runs still refresh the list as they go,** as before.

## Tests

`frontend/tests/unit/live-covered.test.mjs`:
- `the app's own calls to the broker don't ask again, the list's own among them (#747)`. API-connection events (`list_covered`'s own `api_call`, `read_usage`, a `file_in`) send nothing. A run's event sends one request, and the answer's own `api_call` doesn't send another.
- The existing tests pass a run's event to `observe`.

`frontend/tests/unit/live-adapter.test.mjs`, the seam through the real adapter:
- `the list of covered requests isn't asked for again for its own call, kept as activity (#747)`. After the list is answered, the broker's `api_call` event for it doesn't send `list_covered` again. A run's `subagent_progress` does.

**Hand-break:** the filter made always true (`|| true`) fails exactly these two tests, and nothing else.

## The gate

`npm run check` (typecheck, lint, design, actions, protocol, seam, format, tests with coverage, live coverage, operator coverage): green.
- Unit tests: 631 passed, 0 failed.
- Live-adapter tests: 275 passed, 0 failed, at 100% line, branch and function coverage.
- Operator tests: 16 passed.

No Rust changed.
