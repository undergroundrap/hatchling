# WO-022: An operator API, and the command center's Machine view

**Status:** DONE
**Area:** `crates/hatchling-broker` (a new operator listener beside `api`), `crates/hatchling-core` (`ui`: the operator's types, in the generated schema), `crates/hatchling-host`, `frontend/` (a second app in the same package)
**Issue:** [#78](https://github.com/undergroundrap/hatchling/issues/78)
**Trust boundary:** yes

## Problem

The broker has one local API with one owner. Everything a caller can do, it does as that owner. A company running Hatchling for its people, or a product running it for its users, needs the people who run the machine (operators) to see its state without being able to act as its users, and needs its users unable to reach what operators can. Hiding buttons in one UI doesn't give that: anything that can talk to the API could call the hidden parts. Today an operator also has no view of the machine at all: which cells are running, what they're held to, and what the network allowed or refused, short of reading logs.

## Scope

**In scope:**

- **The operator socket.** A separate listener (`HATCHLING_OPERATOR_SOCKET`), owner-only, with its own token as the UI socket has ([WO-021](workorder-021-live-frontend.md)). It serves operator queries only, and the user and UI sockets serve none of them: the split is the operating system's and the broker's, not the UI's.
- **Read-only operator queries.** The broker's version and uptime; the cells running (id, kind: agent, run, subagent, skill or harness; when it started; the limits it's held to); the open API and UI sessions (when, and which socket, never their content); and recent egress decisions (time, destination, allowed or refused, and why), without request or response bodies.
- **Metadata only.** No operator query returns chat text, task text, memory, artifacts, a secret, a surrogate, or a body. A test scans every operator answer for a set of planted values and finds none.
- **The command center's first screen.** A second app in the frontend package, served by `hatchling-host` at its own address with the operator token, in the same style guide and design system as the UI. Its first screen, the Machine view, shows the queries above and refreshes while open.
- **On the machine.** Nothing the operator API gathers leaves the machine: no network call is made on its behalf.

**Not in scope:** users and roles; policy ceilings (what users may turn on); audit export ([candidate](README.md#candidates)); fleet configuration; any action that changes the machine. Each of those is its own WO, built on this split.

## Acceptance criteria

- [x] The operator socket answers the queries above; the UI and API sockets refuse every operator query, and the operator socket refuses every user action; tested.
- [x] A hello on the operator socket without its token, or with the UI's, is refused before anything is served; tested.
- [x] No operator answer contains a planted chat line, task, secret, surrogate or body, across a session that used each; tested.
- [x] The Machine view shows the running cells, sessions and recent egress decisions from a real broker, and updates when a cell starts and ends; tested in a browser.
- [x] Running the operator API makes no outbound network connection; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Hardened WSL2 Ubuntu, `HATCHLING_REQUIRE_CELL=1`, plain clippy clean on Rust 1.98.1 and 1.85.0 for every Rust part. Seven pull requests, each gated on its own; the broker's parts each passed `check.sh` on both toolchains, mutation on their changed lines with nothing missed, and hand mutants of their key lines.

- **Part 1, the operator socket ([#91](https://github.com/undergroundrap/hatchling/pull/91)):** `HATCHLING_OPERATOR_SOCKET`, its own frames (`OperatorRequest`, `OperatorResponse`) and its own token in `<socket>.token`; the `broker` query. 2,214 tests each; 27 mutants, 19 caught, 8 unviable. Hand mutants failed their tests: the token ignored, any peer admitted, the socket allowed to be the API's.
- **Part 2, the cells running ([#93](https://github.com/undergroundrap/hatchling/pull/93)):** a `Running` guard at each broker-side launch (the agent cell, runs and subagents, skills and harnesses), listed until the cell ends. 2,219 tests each; 33 mutants, 16 caught, 17 unviable. Four hand mutants failed their tests.
- **Part 3, the sessions open ([#94](https://github.com/undergroundrap/hatchling/pull/94)):** an `Open` guard for each admitted API and UI connection, taken before its welcome. 2,223 tests each; 19 mutants, 5 caught, 14 unviable. Three hand mutants failed their tests.
- **Part 4, the egress decisions ([#95](https://github.com/undergroundrap/hatchling/pull/95)):** the last 200, each with a verdict from a closed set, recorded in the broker's egress handler. 2,227 tests each; 15 mutants, 3 caught, 12 unviable, after a bound that a mutant could turn into a hang was made an `if`. Five hand mutants failed their tests.
- **Part 5, what an answer never holds ([#96](https://github.com/undergroundrap/hatchling/pull/96)):** the planted-values and no-connection tests.
- **Part 6, the Machine view ([#97](https://github.com/undergroundrap/hatchling/pull/97)):** `hatchling-host` carries `/operator` to the operator socket and prints the view's address; the view itself, a second app in the frontend package, in the same design system. Its client and formatting at 100% lines, branches and functions; CI opens it in Chromium beside the app.

**The acceptance criteria, as tested:**

- **The split:** the operator socket answers `broker`, `cells`, `sessions` and `egress`; neither the user's nor the operator's side decodes the other's requests (`neither_side_decodes_the_others_requests`), so a user's socket ends a connection that sends a query, and the operator's ends one that sends a user's action, a `subscribe` or a second hello (`a_users_action_or_a_second_hello_ends_the_operators_session`).
- **Its token:** a hello with no token, the UI's, a wrong one or one a character short is closed unanswered, and nothing is answered before a hello (`a_hello_without_the_operator_token_is_closed_unanswered`); a peer that isn't the owner gets no byte read.
- **Nothing planted reaches an answer:** a chat line (as the task of the run that answers it), a secret, a surrogate, and a request's path, header and body, sent through the broker's real egress handler and launcher, appear in no answer while the run is alive, though the session's own cell id does (`nothing_planted_reaches_an_operator_answer`).
- **The Machine view, in a browser:** CI's README quickstart job opens it before the chat and checks, without a reload, a new egress decision appear with the app's connection, and none of the message's text; then sends a message the stand-in answers slowly and checks a run cell appear in the view while it runs and go once it is answered.
- **No connection out:** the operator's code names no socket type and nothing of the egress proxy (`the_operator_code_opens_no_network_connection`); what it shows is gathered in the broker's memory.

**Left over, out of this scope:** the view asks every two seconds; pushing changes instead is a candidate if the machine grows busy. Users and roles, policy ceilings, audit export and fleet configuration remain their own WOs, as the scope said.

## Notes

**Why a socket, not a role flag.** A role checked in code is one bug away from a user reaching operator functions. A separate socket, with its own token, means a user's session has no path to them at all, and the code that answers operators never runs for a user.

**Why metadata only.** Operators need to know the machine is healthy and what it let out, not what people said to their agents. Keeping content out also limits what a stolen operator token exposes.
