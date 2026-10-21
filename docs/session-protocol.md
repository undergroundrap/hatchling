# Hatchling session protocol

**Status:** v13 (current). This doc is the human-readable reference
for the broker session wire protocol. The Rust types live in
`crates/hatchling-core/src/messages.rs` (op families). The
authoritative version history is the doc comment on
`PROTOCOL_VERSION` in `crates/hatchling-core/src/framing.rs` —
if this doc and that comment disagree, the comment wins.

**Versioning:** the protocol is versioned by op-family additions.
Versions must match exactly: a peer speaking any other version is
a protocol violation, not a negotiation. Note: v12 made
`ScheduleSpec`'s `tz` required, so a bump can tighten a
previously-loose field. Peers reject unknown variants loudly —
no silent misinterpretation.

## Transport

Sessions run over the broker's framed transport (`MAX_FRAME_BYTES`
= 64 KiB per frame, length-prefixed). Every request is a
`SessionRequest`, every response a `SessionResponse`. All types are
serde-serializable; the wire format is JSON.

Every message fits one frame. An egress body that wouldn't (a long
model conversation, or a long answer) moves in parts instead: see
v13.

## Op families

| Family | Request | Response | Since |
|---|---|---|---|
| Vault | `VaultOp` | `VaultResult` | v2 |
| Egress | `EgressOp` | `EgressResult` | v3 |
| Channel | `ChannelOp` | `ChannelResult` | v4 |
| Memory | `MemoryOp` | `MemoryResult` | v1 |
| Skill | `SkillOp` | `SkillResult` | v5 |
| Goals | `GoalsOp` | `GoalsResult` | v6 |
| Query | `QueryOp` | `QueryResult` | v6 |
| Schedule | `ScheduleOp` | `ScheduleResult` | v7 |
| Hook | `HookOp` | `HookResult` | v8 |
| Artifact | `ArtifactOp` | `ArtifactResult` | v9 |
| Space | `SpaceOp` | `SpaceResult` | v10 |
| Subagent | `SubagentOp` | `SubagentResult` | v10 |
| Web | `WebOp` | `WebResult` | v14 |
| Browser | `BrowserOp` | `BrowserResult` | v15 |

## v9 — Artifact ops

The artifact family covers builds, delivery, and the idea lifecycle.

**`ArtifactOp` variants:** `Build`, `Deliver`, `Describe`,
`CaptureIdea`, `ListIdeas`, `NoteIdea`, `ProposeActivation`,
`Activate`, `RefineIdea`, `BuildIdea`.

- `build_kind: String` — names the builder kind (e.g.
  `"document"`, `"page"`). Carried on `Build`, `BuildIdea`, and
  `ProposeActivation`.

**`ArtifactResult` variants:** `Built`, `BuildFailed`,
`Delivered`, `Refused`, `Described`, `UnknownBuild`,
`IdeaSaved`, `Ideas`, `ActivationProposed`, `IdeaRefused`,
`Denied`, `ArtifactError`.

**Typed failures** (not strings):

- **`BuildFailure`** (7): `BadRequest`, `UnknownBuilder`,
  `CapabilityDenied`, `BuilderFailed`, `BadOutput`, `Full`,
  `UnknownKind`.
- **`DeliveryRefusal`** (7): `UnknownBuild`, `Incomplete`,
  `HashMismatch`, `Blocked`, `GateError`, `BadTarget`,
  `DeliveryFailed`.

Every failure is typed so callers can match on it. A build that
fails for a reason not in `BuildFailure` is a protocol gap, not a
string to parse.

## v10 — Space and subagent ops

**Space** covers the shared-state primitive: key-value
(`Put`/`Get`/`Remove`), task lists (`AddTask`/`RunTask`), sync
(`SyncPull`/`SyncPush`/`Conflicts`), media (`PutMedia`/
`GetMedia`), and pairing (`Pair`, `Create`, `Join`).

**Subagent** covers bounded child-agent delegation:

**`SubagentOp`:** `Spawn { task, budget, capabilities }`,
`Wait { child }`, `Kill { child, reason }`.

**`SubagentResult`:** `Spawned { child, ... }`,
`Report { ... }` (the typed result envelope),
`Refused` (typed refusal — e.g. capability widening, depth bound),
`SubagentError`.

Rules enforced at the protocol layer:

- Capabilities are a subset of the parent's grants; widening is
  refused before a cell exists.
- Budget is allocated as spend (no refunds); the broker keeps a
  monotonic ledger (max 64 steps/session, 8 children, 32
  steps/child).
- Depth is structural (max 1 — a child cannot spawn).
- A child's death/timeout surfaces as a typed result, never kills
  the parent.

Full subagent lifecycle semantics are in
`docs/activity-monitor-event-contract.md`.

## v11 — Redaction results

`ChannelResult::Redacted`: the output gate's verdict surfaced as
a typed result. When the gate redacts a channel message, the
result carries the verdict instead of the content.

## v12 — Schedule list

`ScheduleOp::List { after }` / `ScheduleResult::Listed`: the
read-only schedule listing. `ScheduleSpec.tz` is required in
the type — a schedule without a timezone is refused, not
defaulted.

## v13 — Egress bodies in parts

A message must fit one frame, so an egress body larger than about
48 KiB (once base64-encoded) moved neither way. v13 carries it in parts
of `EGRESS_PART_BYTES` (32 KiB), within the egress proxy's bounds
(1 MiB each way):

- **A request:** `EgressOp::Stage { part_b64 }` adds a part to the
  session's stage; the broker answers `EgressResult::Staged { held }`.
  Then `EgressOp::Egress` names how many bytes the body begins with in
  `staged`, and carries the rest, if any, in `body_b64`. The broker
  refuses the request (`EgressError`) if its stage holds a different
  count, and empties the stage either way.
- **An answer:** when the whole `EgressResult::Ok` wouldn't fit a
  frame, the broker holds the body and answers with the status, the
  headers, an empty `body_b64` and `more`, the bytes it holds.
  `EgressOp::Read` returns `EgressResult::Part { body_b64, more }`, the
  next part and what's left after it, until `more` is 0. A new request
  lets a held answer go.

A body that fits goes whole, as before, with `staged` and `more` at 0.
A session holds one staged request and one answer at most.

## v14 — Web

A page read through the broker, from a site the owner allows (WO-028).

- **`WebOp::Fetch { url, reason }`:** the page's absolute `https` URL, and
  why the agent wants it (at most 500 characters), shown to the owner as
  the agent's words.
- **`WebOp::Search { query }`:** a web search through the provider the
  operator chose (at most 400 characters); the broker holds the key.
- **`WebResult`:**
  - `Page { url, status, text, truncated }`: the page as text, the site's
    words;
  - `Results { results }`: a search's results, at most 10, each a
    `WebHit { title, url, snippet }`, the provider's words;
  - `Redirect { to }`: a redirect to another site, not followed;
  - `Waiting { site, request }`: the owner has been asked and hasn't
    decided; fetch again later;
  - `Denied { site }`, `Expired { site }`: the owner said no, or didn't
    decide in time;
  - `Unasked { reason }`: the site couldn't be put to the owner
    (`ApprovalRefusal`);
  - `Refused { reason }`: the URL, the operator's refusal of the site, or
    the read itself;
  - `Off`: the operator hasn't turned this on (pages, or search).

## v15 — Browser

A real browser the agent drives, in a cell of its own, through the broker
(WO-029). Every session is strict: the browser reaches only the sites the
session holds.

- **`BrowserOp::Open { url, reason }`:** open the page, starting the
  session's browser if there is none. Its site is allowed as a
  `WebOp::Fetch`'s is, and a site allowed for one is allowed for the
  other. A site allowed once is held for that visit: when the browser opens
  another site, it's revoked, and its connections closed.
- **`BrowserOp::Read`:** the page as it is now.
- **`BrowserOp::Act { element, action }`:** act on an element of the last
  snapshot (`PageAction`: click, type, a key, a choice). Text to type that
  holds a secret the broker keeps is refused, and never reaches the page.
- **`BrowserOp::Allow { site, reason }`:** ask the owner to allow a site the
  browser was refused, one of the last page's `refused`, which the page
  needs. It's put to the owner as a page's site is, and a site allowed once
  is held for the visit.
- **`BrowserOp::Scroll { down }`**, **`BrowserOp::Close`**: scroll, or close
  the browser and end its cell.
- **`BrowserResult`:**
  - `Page { snapshot, refused }`: the page's text and the elements it can
    be acted on through (`PageSnapshot`), the page's words; and the sites
    the session doesn't hold that the browser was refused;
  - `Waiting`, `Denied`, `Expired`, `Unasked`: as for `WebResult`;
  - `Failed { failure }`: the browser couldn't do it (`BrowserFailure`);
  - `Refused { reason }`: refused before the browser: the URL, the
    operator's refusal of the site, typed text holding a secret, or a
    browser that couldn't start;
  - `Allowed { site }`: the site is allowed; open the page again to load
    what it needed;
  - `Closed`: the browser is closed;
  - `Stopped`: the owner stopped the browser, from the app; it stays
    stopped for the session;
  - `Off`: the operator hasn't set up a browser.

## Device handshake

`DeviceHello` / `DeviceWelcome` / `DeviceRefusal` — the pairing
handshake for new devices. Typed refusal, not a string.

## For the frontend

The frontend's generated types must cover every variant listed
here; the drift test fails if the schema and these types
diverge. When a new op family lands, this doc is updated in the
same commit.
