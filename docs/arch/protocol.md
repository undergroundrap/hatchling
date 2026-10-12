# Protocol

Hatchling's components communicate over a versioned, typed
protocol. This document is the human-readable overview. The
machine-readable contract is `frontend/protocol/schema.json`
(generated — never hand-edit it).

## Protocol versions

There are two versioned protocols. Don't confuse them:

- **Session protocol** (currently **v13**): the broker↔agent
  wire protocol. `PROTOCOL_VERSION` in
  `crates/hatchling-core/src/framing.rs`, which carries the
  authoritative per-version history.
- **Local API** (currently **v4**): the Unix-socket JSON API
  for external clients. `UI_SCHEMA_VERSION` in
  `crates/hatchling-core/src/ui.rs`.

## Transport

- **Local API:** Unix domain socket, JSON framing. The broker
  listens; clients connect. See `docs/local-api.md` for the
  full API contract.
- **Cell communication:** The broker spawns cells and the
  agent connects back to the broker over a Unix socket. Cells
  have no network except through the broker's egress relay.

## Session operations

The core message types for agent sessions:

| Operation | Direction | Purpose |
| --- | --- | --- |
| Session start/end | Agent → Broker | Handshake, then lifecycle |
| Tool call | Agent → Broker | `SessionRequest`: execute a skill or tool |
| Tool result | Broker → Agent | `SessionResponse`: structured result |
| Output | Broker → Client | Redacted, verdict-checked text |
| Grant check | Broker (internal) | Vault authorizes the action |
| Schedule | Client → Broker | Create/list/cancel scheduled tasks |

## Artifact operations

| Operation | Purpose |
| --- | --- |
| `ArtifactOp` | Build or manipulate a file artifact |
| `ArtifactResult` | Result of an artifact operation |
| `BuildFailure` | Structured build error |
| `DeliveryRefusal` | The output gate refused delivery (with reason) |

## Framing and handshake

Every local API session starts with a handshake: the client
identifies itself, the broker verifies the socket peer
credentials, and grants are established. See `docs/local-api.md`
for the exact handshake flow.

## Schema guarantees

- The JSON schema is **generated** from the Rust types via
  schemars. It cannot drift from the code — a test fails the
  build if the checked-in schema is stale.
- Field tags use serde's internally-tagged enum representation.
  Each enum documents which field carries the tag.
- Breaking changes bump the protocol version. The broker
  rejects the peer on any version mismatch — versions must
  match exactly; a wrong version is a protocol violation, not
  a negotiation.
