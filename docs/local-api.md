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

The README's quickstart sets these, along with the broker's own required variables. Two clients in the standard library show the protocol: [`docs/examples/hello.py`](examples/hello.py) (handshake, create, list, cancel) and [`docs/examples/first-run.py`](examples/first-run.py) (schedule a task, receive the agent's answer as a `chat_message` event).

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

Every refusal is typed: the reason names what was wrong with the request, and never anything the caller couldn't already see.
