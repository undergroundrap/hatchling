# Evidence: a run's end cuts its model call in flight (#657)

What was fixed, and how it was tested and measured: a run stopped by its owner, or out of time, while its model was answering now ends at once, and the model's server is hung up on.

| | |
|---|---|
| Base | `main` at `28827faf` (#655, WO-051 slice 4) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Measured on | a fresh install in a throwaway WSL2 distro (Ubuntu 26.04), Ollama on loopback with `gemma4:12b` on the GPU, release builds |

## What was wrong

A run's session forwards a model call through the egress authority and blocks until the answer comes. Stop killed the run's cells at once, but the run's end joins its session, so it waited for that answer: the model wrote on, and on a hosted model it could be billed. Measured on `main` at `28827faf`, Stop 20 s into a 2,500-word story was answered `run_stopped` at once, and the chat's "You stopped it." came **49.8 s** later. The clock had the same delay: a 10-minute run (the build before WO-051) was told it timed out at **908 s**.

The tests missed it because the real-cell Stop test stops a fixture agent that hangs, not one waiting on a model call.

## What it does now

- **A run's end cuts its session's link.** Each run holds a cut (`hatchling_egress::Cut`) for its session's link to the egress authority. However the run ends (its owner's Stop, a hit, its clock, or the run dropped), its teardown cuts the link before it joins the session. The link's socket is shut down both ways, so a forward blocked on it ends at once. The session is told the request was dropped (`RefusalKind::Ended`), which the broker logs at info as what a Stop is for, not as a fault, and nothing more is sent on that link.
- **The authority hangs up on the model.** While it forwards a request, the authority watches the broker's end of the connection. When the broker lets go, it cuts the request's upstream connection (local or HTTPS), so the server sees its client gone, and the connection to the broker ends quietly. The broker sends nothing while it waits for an answer, so anything it sends then ends the request too.
- **Unchanged:** a request nobody cut is forwarded as before, with the same timeouts. With no egress authority running (the library's in-process proxy, used by some tests), a session's calls aren't cut: the broker the installer runs always has the authority.

## Measured

On the fix build, Stop 20 s into gemma4:12b's 2,500-word story, three times (`stoptime.py`):

| | Stop answered | "You stopped it." after Stop |
|---|---|---|
| before the fix (`28827faf`) | at once | 49.8 s |
| fix, rep 1 | 0.01 s | 0.15 s |
| fix, rep 2 | 0.00 s | 0.20 s |
| fix, rep 3 | 0.00 s | 0.12 s |

Each time, the authority logged "the broker let go of a request in flight: its upstream is cut", and Ollama ended the request (`500`, at the moment of Stop) and cancelled its generation: `srv stop: cancel task`, then `stop processing` with 8,673 to 9,626 tokens of context, 30 to 40 ms later.

The clock uses the same teardown as Stop. Its cut is tested (below), not measured: a chat run's clock is 30 minutes.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4479 passed, 0 failed |
| `hatchling-dev audit` | all checks passed |

New tests:
- a cut shuts the connection it holds, TCP or unix, both ways, and stays cut; a connection held after the cut is cut as it's held, and refused; one no cut came for stays open;
- a local forward cut while the upstream thinks ends at once, and the upstream sees its client gone; one cut before it connects sends nothing;
- an HTTPS forward cut in its handshake ends at once, and the upstream sees its client gone;
- through a real authority, `a_runs_end_cuts_its_request_in_flight_and_the_upstream_is_hung_up_on`: a model that reads the request and never answers; the run's cut, 300 ms in, ends the session's call within 2 s, the upstream is hung up on within 2 s, nothing more is sent on the link, a link held after the cut sends nothing, and the authority serves the next link;
- a request dropped as its run ended is logged at info, with nothing at warn or error;
- a run's end cuts its session's egress: past its time, stopped by its owner, and dropped.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| A run's teardown doesn't cut its session's egress | `a_helper_past_its_time_is_killed`, `dropping_a_helper_signals_stop_and_removes_its_directory`, `a_run_that_exited_after_its_owners_stop_is_stopped_and_its_output_dropped` |
| The authority doesn't cut the upstream when the broker lets go | `a_runs_end_cuts_its_request_in_flight_and_the_upstream_is_hung_up_on` |
| A run's link isn't held for its cut | `a_runs_end_cuts_its_request_in_flight_and_the_upstream_is_hung_up_on` |
| A local forward isn't held for its cut | `a_forward_cut_in_flight_ends_at_once_and_hangs_up_on_the_upstream` |
| An HTTPS forward isn't held for its cut | `an_https_forward_cut_in_flight_ends_at_once` |
| A link sends again after its cut | `a_runs_end_cuts_its_request_in_flight_and_the_upstream_is_hung_up_on` |

**Mutation**, on the lines changed against `28827faf`:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-egress` (library) | 19 | 6 | 0 | 0 | 13 |
| `hatchling-broker` (library) | 33 | 24 | 1 | 0 | 8 |

The broker's one miss, `serve_cell_connection` replaced with `Ok(())`, is on a line this change touched only to pass `None` on. The library's tests don't serve a cell's connection; the integration tests do. Run against `handshake`, `spawner`, `leaving_keeper` and `cell_boundary`, it's caught.

The first run had 4 egress timeouts: with a cut broken, the new tests' reads waited a minute. They now wait 5 to 10 s, so a broken cut fails its test instead of hanging it.

## Said plainly

- **A hosted model may still bill what it made before the cut.** The cut stops the wait and closes the connection; what a provider does with a request whose client has gone is its own. Ollama stops generating, measured above.
- **The cut answer isn't counted.** A request dropped in flight has no answer, so its tokens aren't added to the run's usage. The tokens Ollama spent before the cut aren't known to the broker.
