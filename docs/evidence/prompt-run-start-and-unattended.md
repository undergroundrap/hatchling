# Evidence: each run told when it started; runs with nobody there, promises and three safety lines (#711, gaps 1, 2, 3, 7 and 13)

| | |
|---|---|
| Base | `main` at `f8aa99a7` (#710) |
| Date | 2026-11-26 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Issue | [#711](https://github.com/undergroundrap/hatchling/issues/711), the first of its two pull requests |

## What it does

**Gap 1: each run is told when it started.** The prompt said the agent isn't told the date and should ask whenever a task depends on one. So it asked for what the machine already knew. A model also can't reliably work out a weekday from a date, so "next Friday" was a guess even with the date in hand.

- **The broker stamps it.** As a run is launched, `hatchling_scheduler::clock::machine_stamp` renders the time on the owner's machine with the weekday computed: `Thursday 26 November 2026, 06:31 (America/New_York, UTC-05:00)`. When the machine's timezone can't be read, it's said in UTC with `timezone unknown`. Scheduled runs, chat turns and subagents all get it (`ScheduledRun.now`).
- **It reaches the agent as one variable.** The run cell's environment carries it as `HATCHLING_RUN_NOW`. The agent accepts at most 128 bytes with no control characters, and refuses the run's config otherwise. An absent value means nothing is said.
- **It's said after the sections.** The agent adds it to the system prompt, after the pack's sections and the note on what this install allows, as `## When this run started`, through the same label quoting as every other value placed in the prompt, so a stamp can't open a line of its own. The note says it's the owner's machine's clock, and that in UTC or with no timezone the owner may be somewhere else, so the agent asks before relying on a local time.
- **The date sections use it.** `temporal` now says each run is told when it started, and that it's when the run started rather than the moment of the answer. `timezones` says relative dates are read in the timezone the run was told, or the owner's own when they say they're elsewhere, and otherwise it asks.

**Gap 13: dates on pages.** `temporal` adds that a page or a file is as current as its own dates, not today's.

**Gap 2: a run with nobody there.** A new section, `unattended` (domain `product:scheduler`), routed to the Worker kind after `scheduled-work`. When a schedule or a hook started the run, there's nobody to ask. It does the part of the task that's clearly wanted, and leaves undone whatever it isn't sure the owner wants, and anything outward or hard to undo that the task didn't name. It says in its result what it left and why.

**Gap 3: no promise nothing delivers.** `delivery` adds that the agent doesn't say it will let the owner know unless a schedule or a hook will notice; otherwise it offers to set one up.

**Gap 7: three safety lines.** `boundaries` adds three:
- it has no goals of its own beyond the owner's task;
- it judges a request by the whole conversation, not the last message alone;
- it doesn't get around a rule through a subagent, a tool or a file.

## What it doesn't do

- The rest of #711 (gaps 4, 5 and 8 to 12, and merging the overlapping sections) is the second pull request. Gap 6 waits for WO-052, so its text describes the notes as built.
- The stamp is when the run started. A long run isn't told the time again; the prompt says so.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4545 passed, 0 failed (tree `b6a7eb80`, with `main` and Keep going, #710, merged in) |
| `hatchling-dev audit` | all checks passed |

New tests:
- `hatchling-scheduler` `clock`: one instant said in New York, Tokyo, Kiritimati (already the next day there) and UTC, and with a fixed offset that has no name, said to be unknown; this machine's stamp is in its own zone.
- `hatchling-prompt`: the note's wording pinned, and a stamp that can't leave its line; `unattended` pinned with its domain and its place in the Worker kind.
- `hatchling-loop` `runner`: when the run started ends the system prompt, after every section, and with none the prompt is as it was.
- `hatchling-agent` `config`: a run reads its start, and refuses an empty one, one over the bound and one with a control character.
- `hatchling-broker`:
  - `cell`: the run's environment carries the stamp, and nothing when the spec says nothing;
  - `run_launcher`: a launched run's stamp is this machine's clock, read at launch.

**Hand-breaks.** Each break was put into the code by hand and the tests were run against it; every one failed, as it should, and the tree was restored.

| Break | Caught by |
|---|---|
| the launcher stamps nothing | `a_run_cell_is_told_when_it_starts_on_this_machine` |
| the cell never passes the stamp | `a_run_is_told_when_it_started` |
| the agent accepts a control character | `a_run_reads_when_it_started_and_nothing_else` |
| the loop ignores the stamp | `when_the_run_started_ends_the_system_prompt` |
| Worker loses `unattended` | the pinned section lists and assembled prompts |
| the clock renders every zone as UTC | `a_stamp_says_the_weekday_date_time_and_zone_as_computed` |

**Mutation** (`cargo mutants --in-diff`, this change's lines):

| Crate | Result |
|---|---|
| `hatchling-scheduler` | 4 caught |
| `hatchling-prompt` | 2 caught |
| `hatchling-loop` | 1 unviable |
| `hatchling-agent` | 8 caught, 2 unviable |
| `hatchling-broker` | 6 caught, 2 unviable |

No mutant was missed and none timed out. The hand-breaks and the mutation ran again on the merged tree, with the same results.
