# The local API

How to drive the broker's local API from your own code, without reading the Rust source.

## The contract is the schema

**[`frontend/protocol/schema.json`](../frontend/protocol/schema.json)** is the API's JSON Schema (draft 2020-12): every request, response, action, event and schedule type, with their fields and documentation.

- It's **generated, never hand-edited**: `cargo run -p hatchling-dev -- protocol-schema --write` writes it from the Rust types.
- `hatchling-dev`'s tests fail when the checked-in file differs from what the types generate, so it can't drift.
- The frontend's TypeScript decoders are generated from it in turn: `cd frontend && npm ci && npm run protocol:generate`.

The examples below are checked against the same types by `crates/hatchling-core/tests/local_api_doc.rs`: every row in the two tables must decode to the type it names and re-encode to exactly the JSON shown.

## Turning the API on

The broker serves the API only when `HATCHLING_API_SOCKET` is set. For example:

```bash
export HATCHLING_API_SOCKET=$HOME/.hatchling/api.sock
export HATCHLING_API_GRANTS=channel-send:local
```

- **An absolute path outside the broker socket's directory**, which cells mount. The broker creates the socket mode `0600`.
- **It needs `HATCHLING_SCHEDULER_DIR`:** every API call is recorded in the activity log.
- **Grant only what your client needs.** Each label must be a grant the broker's owner holds, and a schedule can carry only grants its creator holds. Unset means none: the caller can follow the activity and do only what needs no grant.
- **Unix socket only.** A network listener is refused in this build: it has no way yet to identify a network caller.
- **Not with cells that run as you.** The API refuses to start when `HATCHLING_AGENT_UID` is the broker's own uid, since a cell could then drive it. An unprivileged broker can give cells only its own uid, so serving cells *and* the API takes a root broker (see the README's quickstart).
- Restart the broker after changing either variable.

The README's quickstart sets these, along with the broker's own required variables. Two clients in the standard library show the protocol: [`docs/examples/hello.py`](examples/hello.py) (handshake, create, list, cancel) and [`docs/examples/first-run.py`](examples/first-run.py) (send the agent a task on the local chat, receive its answer as a `chat_message` event).

## Connecting

- **The socket** is a Unix socket at `HATCHLING_API_SOCKET`. Only its owner (by kernel peer credentials) is admitted.
- **The caller's grants** are `HATCHLING_API_GRANTS`, as grant labels (below). Each must be one the broker's owner holds; unset means none.
- **Framing:** every frame is a 4-byte big-endian length, then that many bytes of UTF-8 JSON, at most 64 KiB.
- **Handshake:** send `hello` first. The broker answers `welcome` with its versions, or `incompatible` with a sentence saying which side to update.
- **After the handshake:**
  - `subscribe` starts the event stream (once per connection);
  - each `action` is answered by an `action_result` with the same `request_id`.

## The UI socket

`HATCHLING_UI_SOCKET` turns on a second socket beside the API's, for `hatchling-host`, which carries a browser's channel to it. It speaks exactly the same protocol, with one more rule: the `hello` must carry the UI token as its `token`.

- **The token** is minted at each broker start and kept in `<socket>.token` (the socket's path with `.token` added), mode `0600`, readable by the broker's user alone. A new start replaces it.
- **A hello without it, or with another,** is closed without a word, as a non-owner's connection is. The comparison takes the same time however much of a guess was right.
- **Why:** a browser's channel reaches a loopback port, and any web page the person has open can reach one too. A page can't read the token file, so it has no token to send. The host only forwards frames; the broker decides.
- **Rules it shares with the API socket:** an absolute path outside the broker socket's directory, the activity log, and the owner's peer credentials. It needs `HATCHLING_API_SOCKET` set too, and a different path.

## The operator socket

`HATCHLING_OPERATOR_SOCKET` turns on a socket for the people who run the machine, apart from its users ([WO-022](workorders/workorder-022-operator-api.md)). An operator can see the machine's state there, and can't act as its users; a user can't reach it.

- **Its own view.** `hatchling-host`, given `HATCHLING_OPERATOR_SOCKET`, serves the Machine view and carries its channel here: see the README's "Using the app".
- **Its own frames.** It speaks `OperatorRequest` and `OperatorResponse` (in the schema): a `hello`, then `query` frames, each answered by one `answer` with the same `request_id`. The API and UI sockets can't decode a query, and the operator socket can't decode a user's action or `subscribe`: either ends the connection.
- **Its own token,** minted at each start into `<socket>.token`, mode `0600`, as the UI's is. A hello without it, or with another (the UI's included), is closed without a word.
- **Its rules:** an absolute path outside the broker socket's directory, different from the API and UI sockets; the owner's peer credentials; refused when cells run as the broker's own uid. It needs no other socket and no activity log.
- **Metadata only.** `broker` answers with the broker's version, when it started and how long it has run. `cells` lists the cells running, oldest first: each one's id, its kind (`agent`, `run` for a scheduled task's, a hook's or a chat message's, `subagent`, `skill`, `harness` or `browser`), when it started and the limits it's held to; a cell is listed from its start until the broker reaps it. `sessions` lists the API and UI connections open, oldest first: each one's id (the one the activity log records its calls under), its socket (`api` or `ui`) and when it was admitted; a connection is listed from its welcome until it closes, and one refused is never listed. `egress` returns the broker's most recent egress decisions (up to 200), oldest first: when, which cell, the destination it named (cut to 255 characters: the cell chose it) and the verdict, `allowed`, `no_grant`, `not_allowed`, `bad_surrogate`, `policy` or `failed`; never a path, a header, a body or an answer. No answer ever holds what a user said or stored, or what a cell was asked.

```json
{"type": "query", "request_id": "q1", "query": {"query": "broker"}}
{"type": "answer", "request_id": "q1", "answer": {"answer": "broker", "version": "0.4.0", "started_at": "2026-10-16T09:00:00Z", "uptime_secs": 61}}
```

## Chat

`send_message` on the `local` chat queues the text on the broker's local channel, and the API's stream shows it as a `chat_message` from the user. The broker answers it with a **chat run**: the agent, in a fresh cell, given the message as its task, exactly as a scheduled run is given its task. The answer comes back as a `chat_message` from the agent.

- **Its grants are the API caller's** (`HATCHLING_API_GRANTS`): only the owner can queue a message, so a chat run can do what the owner's API client could schedule. Without `channel-send:local` among them there is nowhere to answer, and the broker runs no chat.
- **It needs runs:** the scheduler (`HATCHLING_SCHEDULER_DIR` and its model settings), the agent binary, and the chat ingress (`HATCHLING_CHANNEL_INGRESS`), whose queue the messages wait in. Messages the ingress's own client sends are answered the same way.
- **One at a time,** oldest first: the next message waits for the answer to the last.
- **The stream follows it:** an `agent_status` of `working` once a message is queued, with the message's first line as the detail, and `idle` when the answer arrives.
- **A run that fails** answers with a fixed sentence; why it failed is in the broker's log.
- **A chat run sees the conversation so far** (WO-026): its message, and the last exchanges before it, up to 8 and 24 KiB, newest kept. An exchange is kept only when its answer reached you as written: a failed run's sentence, or an answer the output gate blocked or replaced, is never given back. The broker keeps the conversation in its memory, so a broker restart starts a new one, and a run can't add to it.

## Approvals

When the agent proposes an action that needs your yes (building an idea into an artifact, or reading a site it hasn't been allowed), the broker freezes it and asks you on the stream, never in the chat (WO-027, WO-028).

- **The stream's `confirmation`** carries several parts:
  - the request's `confirmation_id` and `revision`;
  - its `digest`: SHA-256 over the action's canonical form, which binds every field of what would run;
  - a `summary` in the broker's words;
  - the action's `fields`, in review order. Each field has a `label`, a `value` (whole, never cut short) and a `source`: `broker` for what the broker resolved, `agent` for the agent's own words;
  - `session`: whether it can be approved for the rest of the agent's session as well as once.
- **`decide`** names the `confirmation_id`, the `revision` and the `digest` you were shown, with `approve`, `approve_session` (only where `session` is true), `approve_always` (only where `always` is true), or `deny`. A decision the request doesn't offer is rejected as `not_offered`.
  - Only a caller holding every grant the owner holds can decide; anyone else gets `refused`/`not_granted`.
  - A decision on a request that changed, lapsed (15 minutes), or was already decided is rejected with its reason.
  - A `confirmation_decided` event follows on the stream either way.
- **On approve, the broker runs the action it recorded, once,** on its own worker. A session can't run it, and nothing a session sends afterwards is what runs. The outcome comes back on the local chat, as the broker's.
- **A site** (WO-028): what's approved is the site, `https://host[:port]`; the page the agent asked for and its reason are shown as the agent's words. `approve` lets the session read that page once, and the next page on the site asks again; `approve_session` (the app's *Allow for task*) lets it read any page on the site until the agent's session ends, which in a chat is that answer; `approve_always` (offered when the broker keeps sites, `HATCHLING_WEB_GRANTS_DIR`) keeps the site, sealed, and every session reads it without asking until you revoke it; `deny` refuses the site from then on, for every session, however the agent asks. The session that asked reads the page itself: nothing runs on the worker. When one of the session's searches gave the page, the card says which result it was (`Found by`, the broker's) and shows the search (`The search`, the agent's words).
- **The sites you always allow** are yours alone to see and change: `manage_sites` with `list` (each site, and when you allowed it) or `revoke` (the next page on it asks again; a session that holds the site for itself keeps it until it ends). A caller holding less than every grant you hold is refused (`not_granted`), and no session op reaches them. `off` means the broker keeps no sites.
- **Asking is bounded:**
  - at most 5 requests wait, one per agent session;
  - at most 20 are made in an hour;
  - a denied action is never asked again, however it's reworded;
  - after a denial, new requests wait 10 minutes.

  The agent is told why it couldn't ask (`approval-refused`).
- **Requests live in the broker's memory:** a restart withdraws them, and you're asked again.

## How enums appear in JSON

Every enum is *internally tagged*: one field names the variant, beside the variant's own fields. It is never a wrapper object such as `{"Cron": {...}}`.

| Rust type | Tag field |
| --- | --- |
| `UiRequest`, `UiResponse` | `type` |
| `UiAction` | `action` |
| `UiActionResult`, `ScheduleResult` | `result` |
| `ScheduleOp` | `op` |
| `ScheduleSpec` | `every` |
| `ScheduleGrant` | `grant` |

Variant names are `snake_case` for the `Ui*` types, and kebab-case for the schedule types (`channel-send`, `create-feed`, `missing-timezone`).

<!-- serde-tags:begin -->
| Rust | JSON |
| --- | --- |
| `UiRequest::Hello` | `{"type":"hello","schema_version":4,"client":"my-script","token":null}` |
| `UiRequest::Subscribe` | `{"type":"subscribe","after_seq":null}` |
| `UiRequest::Action` | `{"type":"action","request_id":"r1","action":{"action":"manage_schedule","op":{"op":"list"}}}` |
| `UiAction::ManageSchedule` | `{"action":"manage_schedule","op":{"op":"cancel","id":"sch-1"}}` |
| `UiAction::ManageSites` | `{"action":"manage_sites","op":{"op":"revoke","site":"https://a.example"}}` |
| `ScheduleOp::Create` | `{"op":"create","when":{"every":"cron","expr":"0 9 * * 1-5","tz":"Europe/Berlin"},"task":"Summarize my open goals.","grants":[{"grant":"goals","action":"query-goals"},{"grant":"channel-send","channel":"local"}],"channel":"local"}` |
| `ScheduleOp::List` | `{"op":"list","after":"sch-1"}` |
| `ScheduleOp::Cancel` | `{"op":"cancel","id":"sch-1"}` |
| `ScheduleSpec::Cron` | `{"every":"cron","expr":"0 9 * * 1-5","tz":"Europe/Berlin"}` |
| `ScheduleSpec::Once` | `{"every":"once","at":"2026-12-01T09:00","tz":"America/New_York"}` |
| `ScheduleGrant::ChannelSend` | `{"grant":"channel-send","channel":"local"}` |
| `ScheduleResult::Created` | `{"result":"created","id":"sch-1"}` |
| `ScheduleResult::Listed` | `{"result":"listed","schedules":[{"id":"sch-1","kind":"task","when":{"every":"cron","expr":"0 9 * * 1-5","tz":"Europe/Berlin"},"channel":"local","grants":[{"grant":"channel-send","channel":"local"}],"next_due":1791622800}],"next":"sch-1"}` |
| `ScheduleResult::Refused` | `{"result":"refused","reason":"missing-timezone"}` |
| `UiActionResult::Schedule` | `{"result":"schedule","outcome":{"result":"cancelled"}}` |
| `UiActionResult::Refused` | `{"result":"refused","reason":"not_granted"}` |
<!-- serde-tags:end -->

## Grant labels and JSON grants

The API's caller grants (`HATCHLING_API_GRANTS`) are written as labels: `kind:field` or `kind:field:field`, comma-separated. A schedule's `grants` are the same grants as JSON objects. Each label maps to exactly one grant:

<!-- grant-labels:begin -->
| Label | JSON grant |
| --- | --- |
| `vault:fetch:app-token` | `{"grant":"vault","action":"fetch","name":"app-token"}` |
| `egress:api.example.com:443` | `{"grant":"egress","destination":"api.example.com:443"}` |
| `memory:user:recall` | `{"grant":"memory","scope":"user","action":"recall"}` |
| `goals:query-goals` | `{"grant":"goals","action":"query-goals"}` |
| `channel-send:local` | `{"grant":"channel-send","channel":"local"}` |
<!-- grant-labels:end -->

## Schedules

- **Creating one:**
  - **Grants:** the caller must hold every grant the schedule names (a feed's are derived from its spec).
  - **Delivery:** the delivery channel must be among them, as `channel-send`.
  - **`tz` is required:** an IANA name such as `Europe/Berlin`. There is no server-local default. A create without one, or with `null` or a blank name, is answered `{"result":"refused","reason":"missing-timezone"}`.
- **Listing:** `{"op":"list"}` returns the schedules the caller holds *every* grant of, a page at a time, in id order.
  - Each shows its `id`, `kind` (`task` or `feed`), `when`, `channel`, `grants` and `next_due` (Unix seconds); never its task text.
  - A page that isn't the last carries `next`. Send it back as `after` for the next page.
  - Nothing says how many other schedules exist.
  - A caller with no grants at all is refused `not_granted`. With the scheduler off, the answer is `denied`.
- **Cancelling** needs the owner's whole grant set, since a cancel can undo any schedule.
- **A fire of unknown outcome** (WO-025): a run a broker had claimed or started when it stopped is never run again on its own. The next broker holds it, and `list` shows it on its schedule as `held` (`{"due":…,"started":true}`); the schedule's later fires are held until the owner acts. `{"op":"resolve","id":"sch-1"}` marks it resolved, after looking at what it did, and the schedule resumes; `{"op":"retry","id":"sch-1"}` runs it again now, as a new attempt under the grants the creator holds then.
- **Parked results:** a result that couldn't be delivered is kept for the owner. `{"op":"list-parked"}` lists them, a page at a time, oldest first, each with its `id`; `{"op":"dismiss","id":"parked-0"}` removes one. At 64 kept, a result isn't parked: it stays with its schedule, whose next fire is refused until it is delivered, or a dismissal makes room to park it.
- `resolve`, `retry`, `list-parked` and `dismiss` are the owner's alone: they need the owner's whole grant set here, and a cell's session is refused them (`denied`).

Every refusal is typed: the reason names what was wrong with the request, and never anything the caller couldn't already see.
