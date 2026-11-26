# WO-054 slice 1: the background clock

Part of [#718](https://github.com/undergroundrap/hatchling/issues/718), [WO-054](../workorders/workorder-054-background-runs.md).

## What it is

`hatchling_scheduler::background` is the clock that wakes Hatchling's own background loops, apart from the owner's schedules. It keeps the dispatcher's rules, and adds what background work needs:

- **Claim, then start.** One store write moves the loop's next due time and says it's running, before the run starts. A store that's down starts nothing.
- **Catch up, never pile up.** A loop that came due while Hatchling was down, or the machine slept, runs once when it's next up, flagged as missed past the dispatcher's grace (`MISSED_GRACE_SECS`). Its next due time is reckoned from then.
- **One at a time, chat first.** No loop starts while another runs, or while the broker's chat has a run in flight. A run in flight is told to stop, once, when a chat run starts or the owner pauses. A stopped run runs again as soon as nothing holds it, not a period later.
- **Off as a whole, or loop by loop.** A loop that's off doesn't run, and its cadence moves on, so turning it back on doesn't start it there and then. The pause holds every loop where it is, so each catches up once on resume.
- **Nothing new, no run.** A loop that needs activity, due with none since it last started, is moved on without a run: that's its evidence probe, and it spends nothing.
- **Gaps.** A tick that finds the last one more than `GAP_SECS` (three minute ticks) ago records the gap, the newest `MAX_GAPS` kept. Slice 2's app note reads them.
- **A run an earlier broker left** is noted as interrupted, of unknown outcome, and isn't run again before it's due.

Cadences: every so many seconds, a cron time in the machine's zone (nightly, daily), or once after things have been quiet long enough after activity.

The broker (`background_ops`) keeps the clock's state as one sealed entry in the schedule store, under that store's key. A schedule op can't name it, and it holds no text: only due times, outcomes and gaps. The dispatcher's thread ticks the clock after the owner's schedules each minute. Background work waits while a run is in flight for the broker's own chat (`follow_ups::in_flight_for`, on the chat's hub, so another hub's runs don't count). The owner's switches, `set_on` and `set_loop_on`, are kept in the same entry; slice 2 gives them the app's switch.

**No loop is registered yet.** The first, memory reconciliation, comes with the seat runner. Until then each tick records only the broker's uptime. The pause (#673) isn't built: the clock takes it as an input, and the broker passes "not paused" until it exists.

## Tests

In `hatchling-scheduler` (`background_tests.rs`):

| Test | What it proves |
| --- | --- |
| `a_suspend_shows_up_as_one_catch_up_run` | The design's test: a ten-hour sleep wakes to one run, flagged missed, with the gap recorded, and the next an hour from the wake |
| `a_run_is_missed_past_the_grace_and_not_before` | Missed means more than the grace late |
| `the_claim_is_written_before_the_run_starts` | The launcher sees the claim; a store that's down starts nothing |
| `nothing_starts_while_a_chat_run_is_active` | Chat first |
| `a_chat_run_stops_the_run_in_flight_once_and_it_runs_again_after` | Stopped once, and rerun when the chat is idle |
| `the_pause_holds_every_loop_and_each_catches_up_once` | The pause stops the run in flight and starts nothing; resumed, one run |
| `a_loop_turned_off_moves_on_and_doesnt_start_when_turned_on` | Off, a loop's cadence moves on |
| `background_off_stops_the_run_in_flight_and_starts_nothing` | Off as a whole |
| `one_loop_at_a_time_the_most_overdue_first` | One at a time, the most overdue first |
| `a_loop_with_nothing_new_moves_on_without_a_run` | The evidence probe |
| `a_loop_after_activity_waits_for_quiet_and_runs_once` | Debounced, and once |
| `a_run_an_earlier_broker_left_is_noted_and_not_rerun_early` | Interrupted, not rerun early |
| `a_run_that_cant_start_is_failed_and_unclaimed` | A failed start leaves nothing claimed |
| `a_nightly_loop_is_due_at_its_local_time` | A cron cadence in the machine's zone |
| `only_real_gaps_are_kept_and_only_the_newest` | The gap bound, and the cap |

In `hatchling-broker`: `the_state_is_kept_and_a_restart_shows_the_gap` (sealed, under its key, across brokers), `the_owners_switches_are_kept`, `no_store_no_clock`, `a_run_in_flight_for_the_chat_holds_background_work`, `no_loop_starts_yet`, and `the_brokers_own_says_whether_a_chat_has_a_run_in_flight`.

## The gate

On the sandbox, at the tree under test:

- `cargo fmt --check`, and clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- The whole workspace's tests: 4567 passed, 0 failed. `hatchling-dev audit`: all checks passed.
- **Hand-breaks**, each run against the tests named and each caught:

| Break | Caught by |
| --- | --- |
| A claim moves the due time one second on, so missed periods pile up | `a_suspend_shows_up_as_one_catch_up_run` and four more |
| Chat first dropped for a loop that's due | `nothing_starts_while_a_chat_run_is_active`, `a_chat_run_stops_the_run_in_flight_once_and_it_runs_again_after` |
| The claim doesn't say the loop is running before it starts | `the_claim_is_written_before_the_run_starts`, `a_run_an_earlier_broker_left_is_noted_and_not_rerun_early` |
| The run in flight isn't told to stop | `a_chat_run_stops_the_run_in_flight_once_and_it_runs_again_after` |
| Gaps go unrecorded | `a_suspend_shows_up_as_one_catch_up_run`, `only_real_gaps_are_kept_and_only_the_newest` |
| The broker's chat no longer holds background work | `a_run_in_flight_for_the_chat_holds_background_work` |

- **Mutation on the changed lines:**
  - `hatchling-scheduler`: 36 mutants, 36 caught.
  - `hatchling-broker` (`background_ops`, `follow_ups`): 15 mutants, 12 caught and 3 unviable.

  The first pass missed six, and each was fixed:
  - The gap cap's `>` to `>=`, and its `-` to `/`, were equivalent as written (with 17 gaps, `17 - 16` and `17 / 16` are both 1), so the cap is now a `saturating_sub` with no branch.
  - The clock's `running()` and its `Debug` gained assertions.
  - The broker's empty loop list was equivalent to `vec![]`, so it went: the clock is made with an empty list until there's a loop.
  - The placeholder run's `poll` gained a test.
