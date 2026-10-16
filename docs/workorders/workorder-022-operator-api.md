# WO-022: An operator API, and the command center's Machine view

**Status:** QUEUED
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

- [ ] The operator socket answers the queries above; the UI and API sockets refuse every operator query, and the operator socket refuses every user action; tested.
- [ ] A hello on the operator socket without its token, or with the UI's, is refused before anything is served; tested.
- [ ] No operator answer contains a planted chat line, task, secret, surrogate or body, across a session that used each; tested.
- [ ] The Machine view shows the running cells, sessions and recent egress decisions from a real broker, and updates when a cell starts and ends; tested in a browser.
- [ ] Running the operator API makes no outbound network connection; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

**Why a socket, not a role flag.** A role checked in code is one bug away from a user reaching operator functions. A separate socket, with its own token, means a user's session has no path to them at all, and the code that answers operators never runs for a user.

**Why metadata only.** Operators need to know the machine is healthy and what it let out, not what people said to their agents. Keeping content out also limits what a stolen operator token exposes.
