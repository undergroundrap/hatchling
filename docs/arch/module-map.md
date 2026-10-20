# Module Map

Hatchling is a Rust workspace. This is what each crate does and how they fit together. If you're looking for where something lives, start here.

Every crate has a row below: `hatchling-dev audit` fails the gate if one doesn't, or if a row names a crate that no longer exists.

## How they connect

```text
                      hatchling-runtime   the `hatchling` command: one binary, both sides
                     /                 \
   ┌─ trusted side ──────────────┐   ┌─ cell side ─────────────────┐
   │ hatchling-broker            │   │ hatchling-agent             │
   │   keys, network, schedules, │   │   runs inside a cell; talks │
   │   the local API             │   │   to the broker only        │
   │ vault  egress  fido2        │   │ loop  model  research       │
   │ isolation  scheduler  skill │   │ prompt                      │
   │ space  goals  feed artifact │   │                             │
   └─────────────────────────────┘   └─────────────────────────────┘
          channel   memory   core   sys    shared by both: wire types, records, syscalls
```

- **The broker is the trusted side.** It holds the vault, owns the network (the egress proxy), builds cells (isolation), and runs the schedules and the local API. It never executes untrusted code.
- **The agent is the cell side.** It runs inside a cell and reaches the outside only through its one peer-authorized session with the broker.
- **The cell side never links the trusted side.** `hatchling-agent`'s dependencies, followed all the way down, never reach `hatchling-broker`, `hatchling-vault`, `hatchling-egress`, `hatchling-isolation`, `hatchling-fido2` or `hatchling-runtime`. The code that runs in a cell is built without the code that holds your keys or the network, and `hatchling-dev audit` fails the gate if that ever changes.
- **One binary, both sides.** `hatchling-runtime` installs one `hatchling` command that is both the broker and the agent: point `HATCHLING_AGENT_BIN` at it, and the broker runs it inside each cell as the agent. That's the pattern Chrome and OpenSSH use for their sandboxed processes. What keeps the agent away from your keys is the cell (no keys or vault files, no network, a seccomp allowlist, no capabilities), not which code is in the program file, so the broker's code in a cell's program has nothing to work with. The crate split above is about keeping the code honest: the agent's code is written and reviewed without the trusted side. (Building from source also gives a `hatchling-agent` binary, the agent alone; it works as `HATCHLING_AGENT_BIN` too.)
- **What's published:** every crate here is on crates.io at the workspace version, except the three marked *not published*.

## The front door

| Crate | Role |
| --- | --- |
| `hatchling-runtime` | The `hatchling` command: `hatchling broker` and `hatchling agent` run the two sides' command lines, and it also answers the broker's spawner helpers and starts as the agent when a cell runs it as `/agent`. Install it with `cargo install hatchling-runtime`. |

## Core runtime

| Crate | Role |
| --- | --- |
| `hatchling-broker` | Central coordinator. Spawns cells, routes messages, enforces grants, serves the local API, and holds the owner's approvals (`approvals`, WO-027). Never executes untrusted code. |
| `hatchling-agent` | The binary that runs inside a cell. It reaches the broker over its one peer-authorized session; all its logic is library code, tested without spawning it. |
| `hatchling-core` | The shared foundation: identity newtypes, protocol framing and message types, grants, kernel peer credentials, and the canonical form and digest an approval binds (`action`). The vocabulary everything else speaks. |
| `hatchling-loop` | The agent's tool-calling loop. The model proposes; the host validates and bounds. LLM-agnostic. |

## Isolation and security

| Crate | Role |
| --- | --- |
| `hatchling-isolation` | Cell lifecycle and the `IsolationBackend` trait. Platform-neutral API; Linux backend implements namespaces + seccomp. |
| `hatchling-sys` | Low-level syscall wrappers (seccomp, namespaces, mounts, process and fd handling). The only crate allowed `unsafe`. |
| `hatchling-vault` | Secret storage. Credentials never leave this crate's boundary except via the egress proxy. |
| `hatchling-egress` | The egress proxy: the only component that touches the network for a cell. Allowlist with pinned DNS, surrogate tokens, credential substitution. A web page read by origin (`web`), resolved and checked per fetch, as text. |
| `hatchling-adversarial` | *Not published.* Attack simulations used in testing. Proves the isolation holds. |

## Capabilities

| Crate | Role |
| --- | --- |
| `hatchling-skill` | Skill loading, verification, and execution in cells. |
| `hatchling-scheduler` | Scheduled tasks: create, list, cancel, fire on time. |
| `hatchling-memory` | Broker-owned durable memory (user and project), paged so results stay bounded. |
| `hatchling-space` | Spaces: named, persistent scopes with members, and their sync, devices and media. |
| `hatchling-feed` | The feed: a scheduled digest of the user's own records (goals, tracking, tagged memory). |
| `hatchling-goals` | Broker-owned goals and an append-only tracking log, with the read-only query the feed uses. |
| `hatchling-prompt` | The authored prompt pack: the system prompt, built from named sections. |
| `hatchling-model` | The model provider: an OpenAI-compatible chat-completions client, reached through the egress proxy. |
| `hatchling-artifact` | The artifact pipeline: where untrusted build output becomes user-facing content, with an inspector at the boundary. |
| `hatchling-fido2` | The FIDO2 transport (CTAP2 over USB HID), one of the vault's unlock factors. |
| `hatchling-channel` | Messaging channels: how the user reaches the agent. The broker owns the endpoints. |
| `hatchling-host` | The frontend host: serves the built UI on `127.0.0.1` and carries its WebSocket to the broker's UI socket, frame for frame. A transport only; the broker checks every connection's UI token. |
| `hatchling-builders` | *Not published.* The document and page builders, each a signed skill. |
| `hatchling-research` | Deep research runs: plan, gather, synthesize, and deliver a cited report as an artifact. |

## Developer tooling (never shipped)

| Crate | Role |
| --- | --- |
| `hatchling-dev` | *Not published.* **Dev-only.** Test harness, mutation-testing support, conformance checks, delegation verification. Never linked into the runtime. |

## Dependency rules

- `hatchling-core` depends only on `hatchling-sys`. It's the foundation.
- The broker depends on almost everything but never on `hatchling-dev`.
- `hatchling-agent` never depends on the trusted side, directly or through another crate. A test enforces this.
- `hatchling-dev` is never a dependency of any shipped crate. A test enforces this.
- New cross-crate dependencies need justification in the PR. A new crate needs a row here; see [CONTRIBUTING](../../CONTRIBUTING.md#adding-a-crate) for when code earns its own crate.

## Where to start reading

1. `hatchling-runtime` — the front door. See how the two sides are started.
2. `hatchling-core` — the types. Understand the vocabulary first.
3. `hatchling-broker` — the coordinator. Understand the flow.
4. `hatchling-isolation` — the security model. Understand the boundaries.
5. Then follow your interest.
