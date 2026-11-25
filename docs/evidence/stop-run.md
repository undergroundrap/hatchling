# Evidence: Stop (WO-051 slice 4)

What was added, and how it was tested, for WO-051's Stop: the owner's action, the broker's end of the run's tree, and the app's button.

| | |
|---|---|
| Base | `main` at `b63d43f0` (#654, slice 3b) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0; the app's checks on Windows, Node 22 |

## What it is

- **The owner's action.** The local API's `stop_run` on the `local` chat (version 11) is the owner's alone: a caller holding less than every grant the owner holds is refused `not_granted`. It's answered `run_stopped`, or `nothing_to_stop` when the run had ended. Nothing in a cell reaches the local API, so a run can't stop itself or forge a Stop.
- **The broker's end of the tree.** A chat run's hit is kept while it runs, by its cell's token, and given up as its run is dropped. The chat's runs in flight are found by its hub ([`follow_ups::stop`]). Stop ends each one's tree as a hit does (`HeldHit::stop`): every cell held is killed there and then, and one held after dies as it's held. A Stop isn't a hit: `seen()` stays empty, so what only a secret's hit brings (a schedule's fire held for review, the notice naming where the secret was going) doesn't follow a Stop.
- **Nothing after is delivered.** A stopped run's session refuses its next frame unread and closes, as after a hit. Its end is taken as `RunEnd::OwnerStopped`, whatever it wrote. The run fails with `Ending::OwnerStopped`, and the chat says "You stopped it." in Hatchling's words. The follow-ups it asked for are held, as for a hit's.
- **Only the chat's runs.** A run with nobody there (a schedule's, a hook's, a follow-up's) isn't registered, so it isn't stopped from the chat. A subagent goes with its parent's tree.
- **The app.** The demo's Stop button now shows in the live app while the agent works. It sends `turn.interrupt`, already in the app's protocol, which the live adapter carries as `stop_run`. There's no Resume live: a stopped run is over. `frontend/src/protocol` and `PROTOCOL-LOCK.json` are unchanged; the generated wire types are regenerated from the schema.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | FMT |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | CLIPPY |
| the whole workspace's tests | TESTS |
| `hatchling-dev audit`, `protocol-schema` | AUDIT |
| the app: typecheck, lint, design, actions, protocol, seam, format | all pass |
| the app's unit tests | 620 passed, 0 failed; coverage 88.65% lines, 95.34% branches, 88.37% functions (floors 78, 88, 81) |
| the live adapter's coverage gate | 264 passed; 100% lines, branches and functions |
| the operator's coverage gate | 16 passed; 100% |

New tests:
- a Stop kills each cell its tree holds and one held after, is seen across the tree, and is no hit;
- a chat run its owner stops is killed at once, ends stopped by them and is told "You stopped it."; once dropped it can't be stopped;
- a run that exited after its owner's Stop is stopped, its output dropped;
- a chat's runs in flight are its own, by its hub;
- only the owner stops the chat's run, in the local chat, and another chat's runs on;
- the owner's Stop refuses a session's next op unread;
- `stop_run`, `run_stopped` and `nothing_to_stop` in their wire forms, and `stop_run` in the API's record;
- the live adapter sends `stop_run` on the local chat, with each outcome in plain words;
- the chat offers Stop while a run works, live too, and Resume only in the demo;
- with real cells, `the_owners_stop_ends_a_chat_run_in_its_cell_at_once`: a hanging chat run is stopped, ends "You stopped it." with `OwnerStopped`, and leaves no run directory; a hanging run with nobody there isn't stopped.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The session reads a stopped run's frames | HB1 |
| A Stop that isn't recorded | HB2 |
| A chat run never registered as stoppable | HB3 |
| Every run registered, background ones too | HB4 |
| Another chat's runs stopped too | HB5 |
| Anyone may stop it | HB6 |
| A stopped run's end not taken as stopped | HB7 |

**Mutation**, on the lines changed against `c2cadd8c` (slice 3b's head, the tree #654 merged):

MUTATION

## Said plainly

- **Stop kills; it doesn't ask.** The run gets no last turn to say where it got to. That's the design: Stop is for a run going wrong, and the limits (time, tokens, room) are the ones that ask it to wrap up.
- **Activity keeps what it did.** A run's trail is summarized in Activity as its session ends, however it ends, a Stop included. Only what it would have said after is lost.
- **A run already ending may still answer.** A Stop that lands after the run's end was taken finds nothing in flight, `nothing_to_stop`, and its answer was delivered.
- **The live button has no browser test.** The app's browser suites run the demo, whose Stop they click. The live choice of button is tested as a function (`turnAction`), and the live adapter's sending of it in its own tests.
