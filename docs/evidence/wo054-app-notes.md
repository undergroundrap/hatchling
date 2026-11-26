# WO-054 slice 2b: the app says whether Hatchling is kept running, and when it wasn't

Part of [#718](https://github.com/undergroundrap/hatchling/issues/718), [WO-054](../workorders/workorder-054-background-runs.md). The maintainer approved the protocol change on 2026-11-26.

## What it is

- **Two optional fields in `SetupState`**, the setup checklist's data. Both are left off the wire when unknown, so an older app or broker reads the same as before. As with #423's `process_limits_only`, the protocol version and `PROTOCOL-LOCK.json` are unchanged: the seam check passes, and the schema, `generated.ts` and `wire.ts` are regenerated.
  - `keep_running`: what Windows last said (slice 2a's marker).
  - `stopped`: the latest stretch Hatchling wasn't running, as the background clock recorded it (slice 1's gaps), in Unix seconds.
- **The broker fills them in** on `read_setup`: `Running::here` reads the marker and the clock's sealed state (`background_ops::last_stop`). A state that can't be read is logged and said as none.
- **The checklist's Keep Hatchling running step**, on a Windows install that has said:
  - done when it's on: scheduled and background work wakes on time;
  - skipped when it's off, since that's the owner's choice in Setup: they're told scheduled and background work runs only while Hatchling is open, and how to turn it on. It doesn't keep the checklist open.
- **The stop note** on the setup page, for a week after a stop ended: from when to when, as the browser says the moments, so anything scheduled for then didn't run on time.

**What isn't here:**
- **When each loop last ran.** No loop is registered yet, so that waits for the first (the clock already keeps it).
- **A switch in the app.** The broker can't make the Windows shortcut, so the step says how in Setup.

## Tests

- `hatchling-core`: `keep_running_and_a_stop_are_on_the_wire_only_when_known` pins both fields on the wire, absent and present.
- `hatchling-broker`:
  - `keeping_hatchling_running_is_said_to_the_app`: `setup_answer` carries both.
  - `whats_known_of_keeping_hatchling_running_is_the_clocks`: `Running::here` reads the clock's store.
  - `the_state_is_kept_and_a_restart_shows_the_gap`: the last stop is the newest of two.
- The frontend's `live-setup.test.mjs`:
  - the broker's word in the app's;
  - the step on Windows (no step when not said or off Windows; done; skipped with the way to turn it on; not nagging);
  - the stop note for a week to the second, and not after.

## The gate

On the sandbox, at the tree under test:

- `cargo fmt --check`, and clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- The whole workspace's tests: 4572 passed, 0 failed. `hatchling-dev audit`: all checks passed.
- `hatchling-dev protocol-schema --write`, then `npm run protocol:generate`: `schema.json`, `generated.ts` and `wire.ts` together.
- Rust hand-breaks, each caught:
  - the broker not telling the app what Windows said;
  - the oldest stop instead of the newest;
  - an unknown stop put on the wire as `null`.
- Rust mutation on the changed lines of `api_ops.rs` and `background_ops.rs`: 7 mutants, 3 caught, 4 unviable.

On Windows, in `frontend/`:

- `npm run check`, with bash as npm's script shell so the coverage gates' globs expand: typecheck, lint, design, actions, protocol and seam checks, and format all clean.
  - Unit tests: 622 of 622, with coverage over the floors.
  - Live coverage: 266 of 266, at 100%.
  - Operator coverage: 16 of 16, at 100%.
- Frontend hand-breaks, each caught by `live-setup.test.mjs` with the build clean:
  - off said as done;
  - a stop said for two weeks;
  - the step shown off Windows;
  - the broker's word dropped.
