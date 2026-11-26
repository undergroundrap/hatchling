# Evidence: the rest of the prompt's gaps, and its overlapping sections merged (#711, gaps 4, 5 and 8 to 12)

| | |
|---|---|
| Base | `main` at `f8aa99a7` (#710), with #712 under it |
| Date | 2026-11-26 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Issue | [#711](https://github.com/undergroundrap/hatchling/issues/711), the second of its two pull requests; gap 6 waits for WO-052 |

## What it does

Prompt text only: no code, protocol or UI change.

| Gap | Where | The line |
|---|---|---|
| 4: "done" can mean "started" | `honesty` (every kind) | Work is done when a result says it finished: started, queued or scheduled isn't done, so say which it is. |
| 9: no rule on correcting itself | `honesty` | When you find you were wrong, say so, put it right, and revisit what you built on it. |
| 12: time estimates without a source | `honesty` | Give a time estimate only when something reports one; otherwise say what's left. |
| 10: resourcefulness inside its tools | `tools` (every kind with tools) | Before saying the tools can't do it, try the other ways they give you and say what you tried. A refusal or a grant's limit is where it stops. |
| 11: no check before an outward call | `sensitive-actions` | Check that the call serves the task and sends the owner's data only where the task meant it to go. The broker refuses what wasn't granted; this check is the agent's as well. |
| 5: short replies | `voice` (Chat) | A short reply isn't less work: it's the work, said briefly. |
| 5: formatting | new section `formatting` (domain `chat`, Chat and Worker) | What the app's chat bubble renders, and when to use each form. It names what it doesn't render: a heading shows as plain text, and lists don't nest. |
| 8: schedules from chat | new section `scheduling` (domain `product:scheduler`, Chat) | The task stands on its own: what to do, what it may touch, when it should stop. A run that reaches its stop says so, so the owner can end the schedule. Choose the lightest timing, and a yes to once isn't a yes to every day. |

**Fitted to Hatchling.** These lines are new authoring from the issue's gaps; where a mechanism differs, they say what Hatchling does:
- **The formatting guide was checked against the renderer.** It says what the app's chat bubble does (`frontend/src/components/markdown.ts`, called with headings off).
- **The scheduling section is written for `schedule_propose`.** The chat agent can't schedule anything itself, only ask: the owner sees what's proposed and decides. A run can't end its own schedule, so it says so in its result. The results come back to the chat.

**The overlapping sections merged.**
- **Dates:** `timezones` is folded into `temporal`, the only kinds it was in. `accuracy` keeps its own date line, because the research kinds have no `temporal`.
- **Memory:** `memory-domain` is gone. Its "check it's still current" was already in `memory-use`, and its provenance line moved to `memory-discipline` as "note where each fact came from". `memory-discipline` moves from the `session` domain to `product:memory`, so the memory rules are the memory domain's, and every domain still holds a section. The ask to keep durable facts was said in both `memory-use` and `memory-discipline`; it's now said once.

The pack keeps 40 sections. Each kind's prompt, in bytes:

| Kind | Before | After |
|---|---|---|
| Chat | 14650 | 15700 |
| Worker | 14663 | 15158 |
| Research plan | 4034 | 4307 |
| Research gather | 6821 | 7256 |
| Research synthesize | 3581 | 3854 |
| Builder | 9048 | 9679 |

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4545 passed, 0 failed (tree `59712a8f`) |
| `hatchling-dev audit` | all checks passed |

The pack's pinned tests hold every section's text and domain, every kind's list and every assembled prompt word for word. They were updated with the change.

**Hand-breaks**, each caught by the pinned tests and the tree restored:
- Chat loses `scheduling`;
- Worker loses `formatting`;
- one word of `honesty` changed in the pack but not in its pin.

**Mutation** on `hatchling-prompt`'s changed lines since `main`: 2 caught, 0 missed, 0 timeouts. The pack's text is data; its two mutants are in `now_note`, from #712.
