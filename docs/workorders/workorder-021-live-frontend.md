# WO-021: The UI drives a real broker

**Status:** QUEUED
**Area:** a new `crates/hatchling-host` (the frontend host the `arch` check already reserves), `crates/hatchling-broker` (`api`, `api_auth`, `api_ops`, `api_stream`), `crates/hatchling-core` (`ui`), `frontend/src/main.ts`, `frontend/src/adapters/live/`
**Issue:** [#77](https://github.com/undergroundrap/hatchling/issues/77)
**Trust boundary:** yes

## Problem

The frontend is a demonstration: it runs on scripted fixtures, and nothing a person does in it reaches an agent. The pieces for a live one exist but aren't joined. The live adapter (`frontend/src/adapters/live/`) speaks the broker's local API, but it's tested only against a fake transport. The broker serves that API on a Unix socket, which a browser can't open. There is no frontend host to carry the browser's channel to it. `frontend/src/main.ts` always picks the scripted adapter. And the UI's main chat is `main` where the broker's is `local`, so every message the UI sent would be refused. Someone running Hatchling today can talk to their agent only through the example scripts, and can't see what it's doing.

A browser channel is also a new way in. Any web page the person has open can reach a loopback port, and so can any other local user. Whatever carries the UI must let neither drive the broker.

## Scope

**In scope:**

- **The UI socket.** The broker serves a second local API socket for the UI (`HATCHLING_UI_SOCKET`), with the same frames, actions and events as `HATCHLING_API_SOCKET` and the same peer check. Every connection on it must also present the UI token in its hello: a random value the broker mints at each start and writes to a file only its owner can read. A hello without it, or with another, is refused before anything is served. This is the token provider the API's `Provider` boundary was left open for. `HATCHLING_API_SOCKET` and its clients are unchanged.
- **`hatchling-host`.** A program that serves the built frontend and one WebSocket endpoint on `127.0.0.1`, and passes frames between that endpoint and the UI socket unchanged, translating only the framing. It reads the token file to print, and optionally open, the URL to visit, with the token in the URL's fragment, which the browser never sends to a server. It holds no authorization logic: the `arch` check's rules for it stand, and widen only by the WebSocket library it needs, added with a dependency review.
- **The browser side.** A `UiTransport` over WebSocket in `src/adapters/live/`. The composition-root change in `src/main.ts` the frontend README anticipates: the live adapter when the page was opened from the host with a token, the scripted one otherwise, so the standalone preview still works. The token goes into the hello the live client already sends.
- **Chat.** The UI's main chat maps to the broker's `local` channel, in the live adapter's mapping. Sending a message and receiving the agent's replies work end to end.
- **The agent's status.** The broker emits `agent_status` events (idle, working, and what it's working on, from what the session already reports), so the UI's status line shows the real agent.
- **Honest gaps.** Every action the broker doesn't serve yet is shown as not available in the live UI, never as a silent failure or a fake success.

**Not in scope:** approvals delivered to the user (the next WO, which needs this one); artifacts, attachments, the browser view and its controls; any address but loopback; identities for more than the one owner; the operator API and the command center ([WO-022](workorder-022-operator-api.md)). No operator function is added to the UI socket.

## Acceptance criteria

- [ ] With the README quickstart's broker and `hatchling-host`, a person opens the printed URL, sends a message in the main chat, and sees the agent's answer there; CI runs this end to end in a browser, against the stand-in model, as it runs the quickstart.
- [ ] A hello on the UI socket without the token, or with a wrong one, is refused before any request is served; so is a WebSocket connection made through the host by a page that doesn't have the token; tested.
- [ ] The token file is the broker owner's alone (mode 0600), a new token is minted at each start, and the token never appears in a log or a URL a server receives; tested.
- [ ] `hatchling-host` passes the `arch` check with no authorization code, and its new dependency has a dependency-review entry.
- [ ] The UI's status line follows the agent: working while it runs a task, idle after; tested against the broker's events.
- [ ] Opened without the host, the UI still runs the scripted adapter; the standalone preview is unchanged.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

**Why the broker checks the token, not the host.** The host is held thin by design: the broker makes every authorization decision, and the `arch` check rejects authorization code in the host. A token checked by the broker keeps it that way. The host forwards a hello it can't validate, and the broker refuses it. A malicious page can reach the loopback port, but it can't read the URL fragment, so it has no token to send.

**Why a separate UI socket.** Requiring a token on `HATCHLING_API_SOCKET` would break the scripts that use it today. A second socket lets each be held to its own rule, and gives WO-022's operator socket a pattern to follow.

**Why the agent's status is here.** It's what lets a person see Hatchling work, which is the point of this WO, and the approvals WO builds on the same event stream.
