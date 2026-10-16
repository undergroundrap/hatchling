# WO-007: Bind agent sessions to the cells the broker spawned

**Status:** CLOSED
**Area:** `crates/hatchling-broker` (serve, cell, run launcher), `crates/hatchling-isolation`, `crates/hatchling-agent`
**Issue:** [#10](https://github.com/undergroundrap/hatchling/issues/10)
**Trust boundary:** yes

## Problem

The broker admits an agent session by its kernel uid alone, and the `cell_id` in the hello is self-asserted. Any host process with the agent's uid can open a full agent session, and a session can log itself under another cell's id. The handshake also has no read timeout, so a connection that never says hello holds a connection slot.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** tying each session to the cell the broker spawned (through an inherited pre-connected socket, or a per-spawn secret over an inherited descriptor); using the broker-known cell id in audit records; a handshake read timeout; refusing `nobody` (65534) as the agent uid at startup, unless explicitly overridden.

**Not in scope:** network transports for agents.

## Acceptance criteria

- [x] A process with the agent's uid that wasn't spawned as a cell is refused before any request is served; tested.
- [x] Audit records carry the broker-known cell id; tested.
- [x] A connection that sends nothing is closed after the timeout and frees its slot; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Hardened WSL2 Ubuntu, Linux 6.6.87.2, `HATCHLING_REQUIRE_CELL=1`. Three pull requests, each gated on its own; CI's README quickstart and hosted-model jobs ran the real broker and agent cell end to end, as root, after each.

**How a session is tied to its cell.** Through an inherited pre-connected socket, the first of the two ways the scope allowed. The spawn makes a socket pair and the cell's program holds one end, kept across its exec at the descriptor `HATCHLING_CONNECTION_FD` names; the broker serves the session on the other end, with no peer gate, since the kernel would only report the broker, which made both ends. Nothing can read the connection out of `/proc` the way it could read a secret in the environment or a file. A cell the broker forks itself (its agent cell) gets the pair from the spawn; a run's or a subagent's cell, which a helper process spawns, has its helper send the broker its end over a channel given as the helper's stdin. In cell mode the broker binds no agent socket, and runs bind none at all.

- **Part 1 ([#71](https://github.com/undergroundrap/hatchling/pull/71)):** the hello is read with a 10-second timeout, cleared once it's in; `nobody` (65534) is refused as `HATCHLING_AGENT_UID` unless `HATCHLING_AGENT_ALLOW_NOBODY=true`.
  - `check.sh` green on Rust 1.98.1 and 1.85.0, 2,152 tests each.
  - Mutation on the three changed files: 228 mutants, 187 caught, 41 unviable, 0 missed, 0 timeouts.
  - Hand mutants failed their tests: skipping the nobody refusal, and reading the hello without a timeout.
- **Part 2 ([#72](https://github.com/undergroundrap/hatchling/pull/72)):** the broker's agent cell gets its own connection; the hello must name the cell id the broker gave it.
  - `check.sh` green on both toolchains, 2,163 tests each.
  - Mutation on the changed lines of nine files: 60 mutants, 46 caught, 14 unviable, 0 missed, 0 timeouts.
  - Hand mutants failed their tests: the program's end closed at exec, the spec's own variable kept, another cell's hello welcomed, the adopted connection left inheritable, and a connected cell given the socket.
- **Part 3 ([#73](https://github.com/undergroundrap/hatchling/pull/73)):** run and subagent cells get theirs through the helper; no run socket is bound.
  - `check.sh` green on both toolchains, 2,167 tests each.
  - Mutation on the changed lines: 11 mutants, 7 caught, 4 unviable, 0 missed, 0 timeouts, after two tests were tightened (a helper that sends late; a hand-over that sends nothing).
  - Hand mutants failed their tests: stdin left as the channel to the broker, the helper skipping the hand-over, and the helper keeping a copy.
- **As root**, with a delegated test cgroup, after parts 2 and 3: 48 of 50 test binaries pass, the connected-cell and scheduled-run suites among them. The two that fail, `mount_escape_all_writes_denied` and the `subagents` suite, fail on `main` too and belong to [WO-015](workorder-015-root-cell-tests-in-ci.md).

**The acceptance criteria, as tested:**

- **Refused before any request:** a cell's agent handshakes over its own connection with no socket bound anywhere (`a_connected_cells_agent_handshakes_over_its_own_connection`, the real agent in a real cell); a run's directory holds no socket, and a run can reach neither another run's socket path nor `/sock`. There is nothing for another process with the uid to connect to.
- **The broker-known cell id:** a session on a cell's connection welcomes the hello naming that cell's id and refuses any other with a reason (`a_cells_own_connection_serves_that_cell_alone`, `the_handshake_welcomes_only_the_cell_it_expects`, and a run's own id in `a_run_session_is_served_on_the_connection_its_helper_sends`), so the id the session runs and is recorded under is the one the broker gave.
- **A silent connection:** `a_silent_peer_times_out_before_its_hello` ends the hello read with an I/O error once the peer has been silent past the timeout; the session then returns, and its slot is dropped as the connection's thread ends (`connection_cap`'s tests show a freed slot admitting the next connection).

**Left over, out of this scope:** cells aren't told apart when it comes to schedules: a cell can list and cancel any schedule an agent session made, though never the owner's. It's a candidate in the [queue](README.md#candidates).
