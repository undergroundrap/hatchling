# Evidence: a Room that fails tells the agent nothing of the host (#570)

What was tested, how, and what came back, for
[#570](https://github.com/undergroundrap/hatchling/issues/570), found by
the v0.9.0 release security checklist (item 3: what crosses the wire
stays generic).

| | |
|---|---|
| Base | `main` at `40a004cf` |
| Tested tree | `a4d70a93` |
| Date | 2026-11-22 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no code.

## The problem

When the Room couldn't start, its supervisor couldn't be read, or the
spawner couldn't be reached, the agent's cell was told why in the
error's own words: a host directory the trust check refused, a mapper
device, a cgroup error, `(os error 13)`, or the spawner client's
`cannot be reached: Broken pipe (os error 32)`. That text reached the
agent's results (`Failed`, `NoPage`, `NoSearch`, `NoChange`,
`NotHandedOver`), and a hosted model with them. The errors come from
dozens of `to_string()` calls on the launch path, so fixing each would
leave the next one to be found.

## What changed

Every answer of the real Room reaches the agent through one function,
`room_session::for_the_agent`, applied where the spawner's client is the
agent's Room (`impl Rooms for Spawner`, which the hand-over and folder
code use too):

- **The Room's own sentences pass as they are:** the session's fixed
  sentences, why a job didn't start, and every way the supervisor's
  refusals are said, whatever numbers they count (a file's last line,
  the places an edit's text is at). Only the numbers may differ.
- **Anything else is one fixed sentence:** *the computer couldn't do
  that: Hatchling's log says why*. The detail goes to the broker's log,
  escaped.
- **Around a reason, the Room's words stay:** after why a session ended
  (*the file may or may not have been changed*), and before why the last
  session is still there.

A request is checked before it's sent to the Room, so a check's refusal
never comes back through it. The one path that didn't check first, a
file handed over, now does, as a folder already did, so a path the Room
would refuse is still said in the check's own words.

## Results

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.85.0 and 1.98.1 | clean |
| `hatchling-broker`'s tests, lib and integration, with the workspace built, 1.98.1 | 2,080 passed, 0 failed |
| The broker's `room_session::`, `room_ops::`, `spawner::`, `hand_over::`, `serve::` and `room_job::` lib tests, 1.85.0 and 1.98.1, at `b2dc4bb7` | 249 passed on each |
| Mutation of the lines changed (`--in-diff`, the broker's lib tests) | 36 mutants: 30 caught, 6 unviable, 0 missed, 0 timeouts |

The new tests:

- `a_failure_in_the_rooms_words_reaches_the_agent_as_it_is`: each kind
  of the Room's sentence, with other numbers than the ones the list was
  made with, and around a reason.
- `a_failure_without_the_rooms_words_is_said_plainly_and_logged`: a path
  and an OS error, a mapper's, a status, a sentence of the Room's with
  more after it (and after a number in it), and empty, each said as the
  fixed sentence; logged, and a newline in it forges no line.
- `a_room_that_cannot_start_tells_the_agent_nothing_of_the_host`: the
  real spawner's serving loop with a session that can't start, its
  error a path with an OS error, asked through the real client for each
  of run, page, search, change, take and list; then a spawner that can't
  be reached at all, for all nine asks. Every answer is the fixed
  sentence.
- `a_path_no_file_can_be_at_is_refused_before_the_room_is_asked`.

### Hand-breaks

Each was applied, the Room, spawner, hand-over and session tests run,
then undone, at tree `b2dc4bb7`: the tested tree but for the two
sentences that catch the mutants below. 19 broken, 19 caught:

| Break | Caught by |
|---|---|
| Each of the nine asks of the real Room unfiltered (9 breaks) | `a_room_that_cannot_start_tells_the_agent_nothing_of_the_host` |
| Every reason passed | `a_failure_without_the_rooms_words_is_said_plainly_and_logged` |
| No reason passed | `a_failure_in_the_rooms_words_reaches_the_agent_as_it_is` |
| The words after why a session ended dropped | `a_failure_without_the_rooms_words_is_said_plainly_and_logged` |
| The words before why the last session is still there dropped | the same |
| The detail logged unescaped (`%`) | the same |
| The detail not logged | the same |
| A list of numbers not taken as one | `a_failure_in_the_rooms_words_reaches_the_agent_as_it_is` |
| An edit's refusals left out of the words | the same |
| A file's last line left out of the words | the same |
| A file handed over not checked before the Room is asked | `a_path_no_file_can_be_at_is_refused_before_the_room_is_asked` |

### The mutants the first run missed

The first mutation run missed 2 of 36: `shape`'s helper that steps past
a number, replaced with `""` or `"xyzzy"`. Either way, every sentence was
cut at its first number, so one of the Room's sentences with anything
after a number in it would have passed: a path after *the file's last
line is line 7*, say. The tests then had no such sentence; now they
have two, and the second run caught both.

## Residual

- A reason the Room really gives that isn't in the list reaches the agent
  as the fixed sentence: less said, nothing leaked. A new sentence of the
  Room's is added to the list with it.
- The owner's Activity line for a file that wasn't handed over says what
  the agent was told, not the detail; the detail is in the broker's log.
