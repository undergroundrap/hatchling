# Evidence: a reminder for a time that has passed is not put to the owner

What was tested, how, and what came back, for the fix that refuses a
one-time schedule whose time has gone before the owner sees it, lets the
agent name the day in words, and says why an approved schedule wasn't
made. Every number here is read from a file in the results bundle, which
is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `b833ddd8` (`fix(approvals): close a request that stopped waiting, and say so (#419)`) |
| Tested tree | `75db89aec8e4d4f6be30f64dcd74830bea069e8e` |
| Date | 2026-11-12 (UTC) |
| Machines | Broker: x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2). App: Windows 11, Node 22.17.0 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0, TypeScript 5.8.3 |
| Results bundle | `hatchling-schedule-time-results.tar.gz`, 1060976 bytes, sha256 `dc0e516ace163b29bc830a2df6dafe01d2142498b3d57993c6f75a0693d738c6` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

The agent is not told the date. Asked for a reminder "tomorrow at 9", it
had to write a full date, and the tool's own example showed one
(`2026-11-04 08:00`), so a small model counted from the example and
proposed a date already gone. The broker put that to the owner as an
ordinary request. Approving it then failed, because a one-time schedule
must be in the future, and the reply said only "the broker refused it.
Its log says why". The log held nothing about it.

A first-run test on a small model reported exactly this: a reminder for
"tomorrow" proposed for a week earlier, shown without a flag, and refused
on approval with a pointer to a log.

## What changed

- `schedule_propose` takes `today HH:MM` and `tomorrow HH:MM` for a
  one-time schedule, read by the broker on the owner's calendar. The
  tool's description and example no longer show a date.
- A one-time schedule whose time has passed is refused before it reaches
  the owner. The refusal goes back to the agent with the time it is now
  on the owner's clock, so its next try can be right.
- A one-time request lapses at its own time, not a day later: it can't be
  approved after the moment it was for.
- An approved schedule that isn't made says why, in words.

## Full gate

Run as an unprivileged user with cgroup delegation,
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, once per
toolchain from a clean build:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked --no-fail-fast
cargo run -q -p hatchling-dev --locked -- seed-check
cargo run -q -p hatchling-dev --locked -- protocol-schema
```

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3674 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3674 | 0 | 0 |

`gate.log` sha256
`2d23fed8ec74ca2dff2aaa6e0a8e5fa98cb64eddd0bb23ab06fc6081f3dade53`.

The broker's library tests were then run 15 times over on 1.98.1, every
test in parallel. 14 runs passed all 1328 tests. **Run 6 failed one
test**, `room_disk::tests::a_room_disk_is_made_only_where_there_is_none`,
with "Text file busy" starting the stand-in program that test writes for
itself (`stress-failed-6.log`). That test and the code it covers are not
touched by this change, and none of this change's tests start a program.
The failure is a race in the tests: a program file written by one test
can still be held open by a process another test forked at that moment.
It is to be fixed in a change of its own. `stress.log` sha256
`c721c8002e7a20030368d8dd3f169c3d94c6a5a53cc0dee5bc50b206d65c4190`.

## The app

The change reaches the app only through two doc comments in the
generated protocol types. Run on the tested tree, from the `frontend`
directory, with the test commands given to node directly from a POSIX
shell so their quoted coverage patterns are read as written.

| Check | Result |
|---|---|
| `tsc --noEmit` | exit 0 |
| `lint`, `design:check`, `actions:check`, `protocol:check`, `seam:check`, `format:check` | each exit 0 |
| Unit tests, with the 78 / 88 / 81 coverage floor | 490 passed, 0 failed |
| Live adapter tests, with the 100% line, branch and function floor | 176 passed, 0 failed; 100 / 100 / 100 |
| Operator adapter tests, with the 100% floor | 16 passed, 0 failed; 100 / 100 / 100 |

`app/unit.log` sha256
`c496e6dd97ae353f47d42c091cdf0dd371fe51fddca51b282094360cef381d4a`. The
app's logs in the bundle have the machine's own paths replaced by
`<repo>`. The app's browser tests (`npm run test:e2e`) were not run: they
are left to CI.

## Mutation

Every mutant on the lines changed against the base, on 1.98.1 with debug
info off, one run per crate:

```text
cargo mutants --in-place --in-diff <the crate's diff> --timeout 60 -- --lib
```

| Crate | Mutants | Caught | Missed | Timed out | Unviable | `outcomes.json` sha256 |
|---|---|---|---|---|---|---|
| `hatchling-scheduler` | 3 | 3 | 0 | 0 | 0 | `b746c14a9dff299bb183ae409ef0fcf34cf1a3fe767a3735a55dd60d82133e01` |
| `hatchling-agent` | 3 | 3 | 0 | 0 | 0 | `517bd3987260037954b6357741be3ccc0e5eeb00fac122b5f4a99857e306852b` |
| `hatchling-broker` | 20 | 17 | 0 | 0 | 3 | `3528dd6bb89011658bf27bb7b098aa880cf57929742fdc65498ecab7a3c8ecc9` |

`hatchling-core`'s part of the change is two doc comments. No mutant was
excluded, so there is no equivalence proof to give.

Unviable:

- `crates/hatchling-broker/src/approval_board.rs:416:9: replace Board::propose_schedule -> Result<Requested, Refusal> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/schedule_proposals.rs:39:5: replace propose -> ScheduleResult with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/schedule_proposals.rs:59:5: replace propose_at -> ScheduleResult with Default::default()`:
  unviable, build failure.

## Defences broken by hand

Each defence was removed by hand, one at a time, and the tests that guard
it were run and their failures read. The unbroken tests passed before and
after.

| Defence removed | Test that failed | How it failed |
|---|---|---|
| A time that has passed is refused before the owner sees it | `a_time_that_has_passed_is_never_put_to_the_owner` | "it was not refused": the past date was proposed |
| A day named in words is read | `a_day_named_in_words_is_read_on_the_owners_clock`, `a_time_that_has_passed_is_never_put_to_the_owner` | `tomorrow 09:00` left as written |
| Tomorrow is the day after today | the same two | `2026-11-11 09:00` where the 12th was due; refused as already passed |
| The day is the owner's, not UTC's | `a_day_named_in_words_is_read_on_the_owners_clock` | `2026-11-11` where the owner's clock, in Tokyo, said the 12th |
| The refusal says what time it is now | `a_time_that_has_passed_is_never_put_to_the_owner`, `a_time_that_cannot_run_says_which_way` | the reason came without the time |
| A time the clocks skip is said to be that | `a_time_that_cannot_run_says_which_way` | "that timing can't be read" |
| A schedule that wasn't made says why | `a_schedule_that_was_not_created_says_why` | "the broker refused it." |
| A one-time request lapses at its time | `a_time_that_has_passed_is_never_put_to_the_owner` | it waited until a day on, past its time |
| A far-off one-time request still waits no more than a day | `a_one_time_schedule_waits_only_until_its_time` | a wait of 248400 seconds |
| A day on is a calendar day, not 24 hours | `a_date_some_days_on_is_a_calendar_day_on_the_zones_clock` | still the 1st, a day on from the 1st, on the 25-hour day the clocks go back |
| The tool shows the agent no date to count from | `schedule_propose_offers_day_words_and_no_date_to_count_from` | the example date was found |

## What this change does not do

- **The agent is still not told the date.** It can now name today and
  tomorrow without one, and a wrong date comes back with the time it is.
  Putting the date and time in what the agent is told is a change of its
  own.
- **Other days in words.** "Next Monday" or "in three days" are not read:
  the agent writes the date, and a past one is refused with the time now.
- **Repeating schedules.** A daily or weekly time can't have passed;
  nothing changes for them.
- **The same proposal, asked again.** A proposal identical to one already
  approved is still put to the owner again.
