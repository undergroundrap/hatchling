# Dependency review

Every crate in `[dependencies]`, `[dev-dependencies]`, or
`[build-dependencies]` of any workspace member must have a `## <name>`
section below. The `hatchling-dev` architecture check fails the gate
otherwise. A new dependency is a supply-chain decision: write the
rationale here first, then add it to the manifest.

## rustix

Safe wrappers over Linux syscalls. Used by `hatchling-core` for
`SO_PEERCRED` peer-credential reads without hand-rolled FFI, keeping
the workspace-wide `unsafe_code = "deny"` intact. Features used: `net`,
`process`. Also in dev-dependencies of `hatchling-agent` and
`hatchling-broker` (tests read the real kernel uid; `process` only).
`hatchling-sys` uses `fs`, `mount`, `process` and `thread`, and `net`
for a harness cell's relay: the listener on the cell's own
loopback, and the `SCM_RIGHTS` handoff of its fd to the broker;
`event`, to poll a pidfd so a helper ends with its broker (WO-012); and
`termios`, to hold a terminal's echo off while a security key's PIN
is typed (`EchoOff`); its test opens a pseudo-terminal (`pty`, a
dev-dependency only). `hatchling-vault` uses `process` (the owner
checks' uid) and `fs`, for the keyring's edit lock: a no-follow open and
`flock` (WO-011).

## hatchling-core

Workspace crate, not an external dependency. The shared foundation:
identity newtypes, protocol framing, handshake message types, kernel
peer credentials, `LogLevel`. Extracted in so the agent and the
broker speak one contract instead of duplicating it. No external deps
beyond the reviewed set. `hatchling-model` also uses this crate for the
shared `ModelReasoning` vocabulary, keeping agent, broker and provider
configuration consistent without a broker dependency on the model crate
or a new external dependency (WO-020). It uses `sha2`, already reviewed for
the workspace, for the digest an approval binds (WO-027).

## hatchling-adversarial

Workspace crate, not an external dependency. The adversarial
suite: the in-cell probe (`hatchling-probe` binary) that attacks real
production-spawned cells — socket sweeps, mount-escape attempts, the
seccomp denied-syscall catalog, privesc/unshare probes, peer-gate
bypass attempts, session-uniformity checks — plus the driver
(`hatchling-broker/tests/adversarial.rs`) that spawns cells, runs the
probe, and verifies the boundary held. Used only as a dev-dependency
of `hatchling-broker` tests; never linked into the runtime. No
external deps beyond the reviewed set (`rustix`, `serde_json`,
`hatchling-core`, `hatchling-sys`).

## hatchling-broker

Workspace crate, not an external dependency. The broker daemon:
socket lifecycle, peer authorization, handshake serving. Depends on
`hatchling-core` for the shared contract. No external deps beyond the
reviewed set.

## hatchling-vault

Workspace crate, not an external dependency. The vault backend: the
filesystem store the broker mediates (atomic `0600` files under a
`0700` owner-checked root, `Secret` with zeroization and redacted
`Debug`). Owned by the broker alone — the agent never depends on it;
the agent reaches secrets only through the broker's grant-checked
session. Shares `SecretName` and the size bound from `hatchling-core`
so the name alphabet and wire limits are one definition. No external
deps beyond the reviewed set.

## hatchling-goals

Workspace crate, not an external dependency. The goals and tracking
store: user-explicit goals, an append-only tracking log, and
the read-only feed query interface whose every item carries its
provenance. Owned by the broker alone at runtime; the agent reaches it
only through the broker's grant-checked goals and query sessions, and
depends on it only as a dev-dependency (component tests parse feed
items). Shares the directory discipline of `hatchling-memory` and the
wire types (`GoalsOp`/`QueryOp`) from `hatchling-core`. No external
deps beyond the reviewed set.

## hatchling-memory

Workspace crate, not an external dependency. The memory backend: the
broker-owned durable store for user/project/session memory items
(scoped reads, no bulk export, explicit retention, append-only
context log). Owned by the broker alone — the agent never depends on
it; the agent reaches memory only through the broker's grant-checked
session with peer-asserted principal binding. Shares the wire
protocol types (`MemoryOp`/`MemoryResult`) from `hatchling-core`. No
external deps beyond the reviewed set.

## serde

Serialization framework. Used for the handshake protocol types
(`Serialize`/`Deserialize` on `AgentMessage`/`BrokerMessage`).
`derive` feature for the derive macros.

## serde_json

JSON encoding for the length-prefixed handshake frames (agent), and
for parsing `cargo-mutants`' `outcomes.json` (dev tooling).

## thiserror

Typed-error derives. Every crate's `Error` enum uses it; errors stay
structured instead of stringly-typed.

## toml

TOML parsing for `hatchling-dev`'s manifest and toolchain-pin audits.

## zeroize

Memory hygiene for secret bytes. Used by `hatchling-vault`'s `Secret`
type (manual `Drop` impl, pinned by a dedicated test) and by the
sealed vault's `MasterKey`. `MasterKey` uses the
`Zeroize`/`ZeroizeOnDrop` derives instead of a hand-written `Drop`:
declarative, and there is no `Drop` body for a mutant to delete.
The `derive` feature pulls `zeroize_derive`, a proc-macro from the
same project (MIT/Apache-2.0). Narrow, single-purpose use —
no key derivation, no allocation tricks. `hatchling-model`
uses `Zeroizing<String>` for the secrets its redactor holds so they
can be scrubbed from every model request: the same crate and version
already in the tree, no new features. Since WO-011 the broker's gates
and fetch replies, `hatchling-core`'s secret wire payload
(`SecretB64`) and `hatchling-artifact`'s gate wipe their copies of
held secrets with it too, the same crate and version. `hatchling-core`
turns on its `serde` feature, so an `ApiToken` (the UI's and the
operator's) is a plain string on the wire and a `Zeroizing<String>` in
memory; it adds no crate (`serde` is already a dependency).

## base64

Standard base64 engine. Used by `hatchling-core`'s secret codec:
secrets are arbitrary bytes and the wire protocol is framed JSON, so
the `Store`/`Fetched` payloads carry base64. One engine, pinned in
core, used by both the agent (encode) and the broker (decode). This is
runtime protocol code, not dev tooling.

## tracing

Structured logging. The agent logs lifecycle events (peer authorized,
handshake complete) through it.

## tracing-subscriber

`fmt` subscriber wiring stdout logging for the agent binary, with the
level from `HATCHLING_LOG_LEVEL`.

## hatchling-egress

Workspace crate, not an external dependency. The egress proxy: the
only component that touches the network on the agent's behalf.
Surrogate minting/validation (HMAC-bound to peer uid, destination,
expiry), destination allowlist (deny-by-default, no wildcards, DNS
pinned at startup), and credential swap (real credentials from the
vault, zeroized after use). Owned by the broker alone — the agent
never depends on it; the agent reaches the network only through the
broker's grant-checked session. No external deps beyond the reviewed
set: `serde` and `serde_json` (reviewed above) read model request
bodies for the request policy, with a strict reader that refuses a key
named twice.

## rustls

TLS for `hatchling-egress`'s HTTPS client, replacing minreq.
Pure-Rust TLS 1.2/1.3, used with `default-features = false` and the
`ring` provider only, so one crypto backend is linked and no C toolchain
build (aws-lc) is needed. The proxy drives it directly over a
`TcpStream` it connected to the allowlist's pinned addresses, so DNS is
never consulted per request; the HTTP/1.1 framing and the bounded
response reader are the proxy's own (`local.rs`). The client trusts one
root store: `webpki-roots` plus the operator's PEM bundle
(`HATCHLING_EGRESS_CA_BUNDLE`). It never reads the system CA store, and
`deny.toml` bans the crates that would (and openssl/native-tls). The
broker is the TLS client end to end: nothing is decrypted in the middle.
Its own dependencies (`ring`, `rustls-webpki`, `rustls-pki-types`,
`subtle`, `zeroize`) were already in the tree under minreq.

The minimum is 0.23.46, the [upstream security release](https://github.com/rustls/rustls/releases/tag/v/0.23.46)
fixing RUSTSEC-2026-0346 through RUSTSEC-2026-0349. The lockfile
selects that release; features remain `ring`, `std` and `tls12`, with
defaults disabled. Hatchling uses the built-in session cache, enables
neither ECH nor Brotli certificate compression, and uses ring rather
than aws-lc's hybrid key exchange. These configurations narrow exposure
to the reported paths; upgrading retains all upstream hardening without
adding advisory exceptions or weakening certificate verification.

## webpki-roots

The Mozilla CA root list, compiled in, as `hatchling-egress`'s bundled
trust roots. Data only (CDLA-Permissive-2.0, allowed in
`deny.toml`). Chosen over the system store so the roots a deployment
trusts are the same everywhere and inspectable in the build, with the
operator's own bundle added on top when they need one.

## ring

Already in the tree as rustls's one crypto provider. Also a
direct dev-dependency of `hatchling-egress`: its TLS tests build a fresh
throwaway CA and server certificates in memory on each run (a small DER
builder in the tests, signed with ring's P-256 ECDSA), so no private
key, even a test one, is ever committed, and no certificate-generation
crate (rcgen pulls `time`, whose fixed versions need Rust 1.88) is
needed.

Also a direct dependency of `hatchling-fido2`: the P-256 key
agreement of CTAP2's PIN/UV auth protocols, ES256 verification of an
assertion's signature, and system randomness. One crypto provider in
the tree, not two.

## aes

The AES block cipher (RustCrypto). Used only by `hatchling-fido2`: CTAP2 encrypts the `hmac-secret` salt and output, and the PIN
token, with AES-256-CBC under the key agreement, and the protocol fixes
the mode. Pure Rust, constant-time software AES (or the CPU's AES
instructions when present); MIT/Apache-2.0.

## cbc

The CBC block mode (RustCrypto), over `aes`. Used only by
`hatchling-fido2`, for CTAP2's AES-256-CBC: whole blocks, no
padding, as the protocol defines it. A reviewed mode rather than a
hand-rolled one; MIT/Apache-2.0.

## hmac

HMAC construction (RFC 2104). Used by `hatchling-egress` for
surrogate authentication: surrogates are HMAC-SHA256 over
(peer_uid, destination, expiry) with a per-session key, so the agent
cannot forge or extend them. `hatchling-broker` uses it for the
redaction audit log's tags: HMAC-SHA-256 under a random key sealed
beside the kept texts, so the log alone confirms no guess at what was
redacted (WO-011).

## sha1

SHA-1, used by `hatchling-host` for one thing: the WebSocket opening
handshake's `Sec-WebSocket-Accept` value, which RFC 6455 defines as the
base64 of the SHA-1 of the client's key and a fixed GUID. It proves only
that the server speaks WebSocket; nothing in Hatchling's security rests
on it. The RustCrypto crate, version 0.10, beside `sha2` 0.10, so the two
share one `digest` and no version is duplicated. Chosen over a WebSocket
library, whose own dependencies (`rand`, `getrandom`, `http`) would
duplicate versions the workspace already holds, against `cargo deny`'s
rule.

## sha2

SHA-256 implementation. Used by `hatchling-egress` as the hash
function for the surrogate HMAC, and by `hatchling-broker` for the
audit log's HMAC tags (WO-011).

## base64

Base64 encoding (RFC 4648). Used by `hatchling-egress` for the
surrogate wire format, and by the agent/broker for secret and body
payloads inside framed JSON (already used via `hatchling-core`'s
encode/decode helpers).

## hatchling-loop

Workspace crate, not an external dependency. The agent tool-calling
loop: LLM-agnostic `Model` trait, validated `ToolRegistry`, bounded
`Loop` runner. The model proposes; the host validates arguments
against a hand-rolled minimal JSON Schema validator and enforces the
step budget. v0.1 ships `FakeModel` for tests; real providers are a
later task.

## hatchling-model

Workspace crate, not an external dependency. The model-provider
layer: transport-agnostic OpenAI-compatible chat-completions client
(`ChatCompletionsProvider<T: Transport>`), strict HTTPS endpoint
validation, deterministic synthesized tool-call ids, and fail-closed
response parsing. The crate never opens a socket; the agent
implements `Transport` over the broker-mediated egress path, where
the proxy swaps the surrogate for the real credential.

## hatchling-channel

Workspace crate, not an external dependency. The messaging-channel
abstraction: the `Channel` trait (broker-owned endpoints,
agent access only through the broker session), `ChannelId`/`SenderId`
identity newtypes, `Message` (broker-stamped sender), and the
`LocalChannel` reference implementation (in-memory for the broker and
tests, stdio for agent-side CLI use). The broker depends on it for
the channel registry; the agent depends on it for the session
vocabulary (`ChannelOp`/`ChannelResult` in `hatchling-core`). No
external deps beyond the reviewed set (`serde`, `serde_json`,
`thiserror`, `hatchling-core`).

## hatchling-prompt

Workspace crate, not an external dependency. Hatchling's authored
prompt pack: the system prompt assembled from an explicit ordered
section list (identity, hierarchy, tools, honesty), and the
untrusted-data envelope wrapping every tool result the model sees.
Pure string construction over `serde_json`; no I/O, no network.
`hatchling-loop` uses it for the default system prompt and
`hatchling-model` for the tool-result envelope, so prompt text lives
in exactly one auditable crate.

## hatchling-sys

Workspace crate, not an external dependency. The sole owner of
`unsafe` OS bindings for the cell boundary: user/mount/pid
namespace unsharing, id-map validation/rendering, mount plan
application (`tmpfs` root, bind mounts, `pivot_root`), seccomp-BPF
filter installation, process primitives (`fork`, `exec`, `waitpid`,
`setresuid`/`setresgid` for the cell user, `NoNewPrivs`), and the
spawn handshake pipes. The workspace denies `unsafe_code` everywhere
except this crate. `hatchling-isolation`'s Linux backend depends on it
to build cells; `hatchling-broker` only for its own process
(its uid, socket umasks). No external deps beyond `libc` for the raw
syscall bindings.

## hatchling-fido2

Workspace crate, not an external dependency. The FIDO2 transport: CTAPHID framing over 64-byte HID reports, CTAP2's canonical
CBOR subset (a small codec of its own, strict and bounded, since a
device's reply is untrusted input), the PIN/UV auth protocols 1 and 2,
and `makeCredential` / `getAssertion` with `hmac-secret`, as the
vault's `Authenticator`. Linux reaches keys through `hidraw` with plain
file I/O and sysfs, so the crate has no `unsafe`. `hatchling-broker`
depends on it for the keyring CLI and start-up.

## hatchling-isolation

Workspace crate, not an external dependency. The isolation seam: the platform-neutral `IsolationBackend` trait between the
broker's cell policy and a platform's mechanism, its spec and view
types, the owner-transparency conformance checks every backend must
pass, and the Linux backend (the cell the broker built before, moved
unchanged behind the trait). `hatchling-broker` depends on it for
`spawn_cell` and for helper supervision. It has no `unsafe`: the Linux
backend reaches the kernel only through `hatchling-sys`. Its other
dependency is `thiserror`, and `rustix` is a dev-dependency for test
cleanup only.

## libc

Raw FFI bindings to the C library. Used only by `hatchling-sys`
for syscalls without a safe wrapper (`unshare`, `pivot_root`,
`setresuid`/`setresgid`, `prctl`, `pipe2`, seccomp `prctl`,
`mount`, `umount2`). All uses are confined to `hatchling-sys`,
the workspace's sole `unsafe` crate; no other crate depends on it
directly. Version pinned via Cargo.lock; no build script.

## hatchling-agent

Workspace crate, not an external dependency. The agent side of the
runtime: the `hatchling-agent` binary that runs inside a cell, plus
its library (`Cell` state machine, broker handshake, config parsing,
tool wiring). `hatchling-broker` dev-depends on it so the
cell-boundary integration tests spawn the real agent binary as the
handshake fixture; the path dependency keeps the two in lockstep.
External deps: `thiserror`, `tracing`, `tracing-subscriber`,
`serde_json` (all reviewed above); workspace crates
`hatchling-core` and `hatchling-loop`.

## chacha20poly1305

Authenticated encryption for the sealed vault: secrets at
rest are XChaCha20-Poly1305 ciphertext under a 256-bit master key.
Chosen over `ring`/`openssl` because it is pure Rust (`alloc`-only,
no C compiler, no build script), has no `unsafe` in its public
surface that we touch, and XChaCha20's 192-bit nonce makes random
per-secret nonces safe — no nonce-management state to get wrong.
RustCrypto project, widely audited; MIT/Apache-2.0. Used only by
`hatchling-vault`; the key type and envelope framing are ours. The
`std` feature is enabled (on top of `alloc`) for one reason: without
it `aead::Error` does not implement `std::error::Error`, and the
typed `SealFailed` source needs it. `default-features = false`: the
default `getrandom` feature would pull `rand_core` 0.6 → `getrandom`
0.2 and trip the duplicate-version ban (the tree already carries
`getrandom` 0.4 via `cc`); nonces are filled directly with
`getrandom` 0.4 instead. The crypto itself stays `alloc`-only in
behavior.

## getrandom

OS randomness for master-key generation and per-secret nonces in the
sealed vault: `getrandom::fill` directly, no `rand`/`rand_core`
facade — we need a CSPRNG fill, not distributions or PRNGs.
Version 0.4 was already in the tree (via `cc`'s build graph), so it
adds no duplicate version; `rand_core` 0.6 would have pulled
`getrandom` 0.2 and tripped the duplicate-version ban. `getrandom::Error`
is the `std::error::Error` source of the typed `Random` variant.
`hatchling-egress` uses the same `getrandom::fill` for the proxy's
surrogate HMAC key, with its own `Random` variant; same version, so
still no duplicate.
MIT/Apache-2.0.

## ed25519-dalek

Ed25519 digital signatures (RFC 8032). Used by `hatchling-skill` for the skill trust core: manifests are signed by the skill
author and verified at admission against a 32-byte trust-root public
key. No hand-rolled crypto — this is the standard, well-reviewed
implementation. The `serde` feature is enabled for signature
serialization in tests. MIT/Apache-2.0 (via the RustCrypto project).

## hatchling-skill

Workspace crate, not an external dependency. The skill trust core
(/): signed skill manifests, SHA-256 code pinning at
registration, call-time capability enforcement (`check_skill_call`),
and the `SkillRegistry` with identity-drift detection. The broker
depends on it for skill admission — the cell never self-attests.
External deps are the reviewed `ed25519-dalek` (signatures),
`sha2` (code hashing), and `serde`/`serde_json` (manifests).

## hatchling-scheduler

Workspace crate, not an external dependency. Scheduled work:
five-field cron expressions evaluated in each schedule's explicit IANA
timezone, the stored schedule record with create-time validation
(subset-closed grants, a required timezone), and the dispatcher state
machine (overlap skip, missed fires once and flagged, a fire-time grant
re-check, delivery with one retry, and parking). It holds no store and
no I/O of its own: the broker implements its `Store`, `Launcher`, and
`Delivery` traits, so the cell never reaches any of it. External deps
are the reviewed `jiff` (time and timezones), `serde`, `thiserror`, and
`tracing`.

## hatchling-artifact

Workspace crate, not an external dependency. The artifact pipeline core: pinned build requests (inputs hashed when the broker
receives them), strict parsing of a builder's output (a builder cannot
write provenance or any other broker-owned field), broker-asserted
provenance (the pinned inputs, the builder's signing identity, and the
output hash the broker computed), the share-review gate (held secrets
raw, hex, or base64, plus the operator's review policy, failing
closed), and the one delivery function every target goes through. It
holds no store and no I/O of its own: the broker keeps the builds and
implements the delivery targets. External deps are the reviewed
`sha2` (content hashes), `base64` (encoded secret forms), and
`serde`/`serde_json` (requests, builder output, and records).

## hatchling-space

Workspace crate, not an external dependency. Spaces:
broker-minted space identities, membership checked against the
session's broker-asserted principal (the peer's kernel uid, or a space
task's run bound to its one space), the admission factors (possession,
session liveness, the pinned peer, freshness, and 's byte-exact
name binding), and space tasks, whose grants are validated with the
scheduler's own subset-closure against the space's. It holds no store
and no I/O of its own: the broker keeps the space store and the session
admission table. External deps are the reviewed `serde` (stored
records) and the workspace's `hatchling-core` and `hatchling-scheduler`.
 adds sync, device pairing, and media rules: the reviewed
`ed25519-dalek` (the device endpoint's keys and signatures; the broker
now depends on it directly too, for its own device key), `base64` (the
keys' and media's wire form), `sha2` (device ids from key hashes), and
`serde_json` (sizing paged sync replies by their encoded form).

## hatchling-feed

Workspace crate, not an external dependency. The feed: a feed
spec (the user's own tuning, as rules) with the read grants derived from
it, one per kind of section, and the digest generated from it through a
read-only `Reader`: every entry an observation of one record, quoting
its text, with the record's transitive provenance label from the store
layer. It does no I/O of its own: the broker supplies the reader, over
`hatchling-goals`' read-only query path, and delivers the digest through
the scheduler. External deps are the reviewed `jiff` (UTC times), and the
workspace's `hatchling-core` (the wire spec and the grant vocabulary),
`hatchling-goals` (queries and provenance), and `hatchling-memory`
(tags). The broker depends on it to answer feed runs.

## hatchling-research

Workspace crate, not an external dependency. Deep research runs: a phased mode of the agent loop (plan, gather, synthesize,
report), each phase a bounded run of `hatchling-loop`'s `Loop`. It
holds the run's rules: bounds in sources and time, plan-only gathering
by source id with user-approved amendments, fetch records (URL, time,
content hash), the gathered sources as evidence in `hatchling-prompt`'s
untrusted-data envelope, and the report's citation discipline (claims
bound only to sources the run read, with their depth; uncited claims
flagged; disagreements kept as sides). It does no I/O of its own:
fetching is a `Fetcher` the agent supplies, through the broker's
egress proxy. External deps are the reviewed `serde`/`serde_json` (the
model's plan and draft), `sha2` (content hashes), `jiff` (UTC fetch
times), and `thiserror`, and the workspace's `hatchling-loop` and
`hatchling-prompt`. The agent depends on it to run research; the broker
uses it only in a delivery test.

## jiff

Timestamps, civil (local) date and time, and IANA timezones for the
scheduler: a cron schedule fires at local minutes in its own
zone, which needs a real timezone database. Daylight-saving gaps (a
local time that does not exist) and folds (one that happens twice) are
reported explicitly, so the scheduler can skip a gap and fire a fold
once. Used by `hatchling-scheduler` with `default-features =
false` and exactly `std`, `tz-system`, and `tzdb-zoneinfo`: it reads the
host's `/usr/share/zoneinfo`, with no bundled database, no proc-macros,
and no further dependencies on Linux (its Windows and wasm extras are
target-gated and never built here). Pure Rust, MIT/Unlicense, by the
author of `regex` and `ripgrep`. It is not `unsafe`-free: its formatting
buffer and timezone internals use a small amount of `unsafe`
(`src/fmt/buffer.rs`, `src/fmt/mod.rs`, `src/tz/timezone.rs` in 0.2.24),
and the rest is platform code never built here. The workspace's own
`unsafe_code = "deny"` is per-crate and unaffected. Chosen over
`chrono-tz` (a bundled, build-time-generated database) because the
system database updates with the OS, and over hand-written TZif parsing
because DST edge cases are exactly where hand-rolled code goes wrong.
`hatchling-research` also uses it, with `default-features =
false` and only `std`, to state source fetch times in UTC: no timezone
database at all. `hatchling-feed` uses it the same way, for the
times a digest states.

## syn

Rust source parsing for `hatchling-dev`'s mutation pre-flight gate: it finds each function, the lines it spans, whether it is
test code, and the calls in its body, so the gate can check that every
changed function is called directly by a test and that a test's
`// Mutants:` comment names only what the test calls. Dev tooling
only; nothing in the runtime links it. Already in the tree at this
version as the parser behind the workspace's proc-macro dependencies
(`serde_derive`, `tracing-attributes`), so no new crate is added. Used
with `default-features = false` and exactly `full`, `parsing` and
`visit`: no printing, no proc-macro bridge. MIT/Apache-2.0, by David
Tolnay.

## proc-macro2

The token and span types `syn` parses into. `hatchling-dev` depends on
it directly only to enable `span-locations`, which gives a parsed
span its line numbers outside a proc-macro: the pre-flight gate
reports every finding as `file:line`. Already in the tree as `syn`'s
own dependency; dev tooling only. MIT/Apache-2.0, by David Tolnay and
Alex Crichton.

## schemars

JSON Schema generation for the frontend protocol. The Rust wire
types in `hatchling-core` are the single source of the frontend/backend
contract: with `hatchling-core`'s off-by-default `schema` feature, each one
derives `JsonSchema`, and `hatchling-dev protocol-schema` collects them into
`frontend/protocol/schema.json`, from which the frontend generates its
TypeScript types. Chosen because it reads serde's own attributes (through
`serde_derive_internals`), so `tag`, `rename`, `rename_all` and `flatten`
come out exactly as serde puts them on the wire, which a hand-written schema
would have to re-derive and could get wrong.

Only `hatchling-dev` turns the feature on, so no runtime binary compiles it;
`cargo deny` checks it anyway (`all-features`). Used with
`default-features = false` and `derive` + `std`. Version 1.2, MIT, by
Graham Esau. It brings `schemars_derive` and `serde_derive_internals`
(MIT/Apache-2.0; the latter is serde's own crate), `dyn-clone` and
`ref-cast` / `ref-cast-impl` (MIT/Apache-2.0, by David Tolnay). No `unsafe`
in the derive path; `dyn-clone` and `ref-cast` use small, audited `unsafe`
for their one trick each.

## hkdf

HKDF (RFC 5869) for the vault keyring: a hardware-key tier's KEK is
`HKDF-SHA-256` of the FIDO2 `hmac-secret` output, salted by the vault id and
bound to the credential, and later tiers derive their KEKs the same way.
Chosen over a hand-written extract-and-expand: key derivation in a vault is
not the place for home-made crypto. RustCrypto, version 0.12, MIT/Apache-2.0;
pure Rust, no `unsafe`, no build script. It brings nothing new: it is built on
`hmac` 0.12 and `digest` 0.10, already in the tree. `hatchling-vault` also
takes `hmac` and `sha2` directly now (both already in the tree, through
`hatchling-artifact` and `ed25519-dalek`), for the virtual authenticator's
`HMAC-SHA-256`.

## argon2

Argon2id (RFC 9106) for the vault keyring's TOTP tier: the tier's
KEK is Argon2id of the authenticator seed, as the task pins, with
OWASP's baseline parameters (19 MiB, 2 iterations, 1 lane). The tier is weaker
by construction, since the seed is stored, and the keyring's documentation says
so; Argon2id is the pinned derivation, not a claim that it makes a stored seed
safe. RustCrypto, version 0.5, MIT/Apache-2.0, pure Rust. Used without its
default features (no `rand_core`, no OS RNG through it). It brings `blake2`
0.10 (Argon2's internal hash) and `password-hash` 0.5 (a non-optional
dependency, whose PHC-string API the vault does not use); both RustCrypto,
MIT/Apache-2.0, on `digest` 0.10 and `base64ct`, already in the tree.

## sha1

SHA-1 for RFC 6238 TOTP codes. Authenticator apps compute
HMAC-SHA-1 codes; SHA-256 TOTP is not universally supported, so a vault that
only spoke SHA-256 would lock out common apps. SHA-1's collision weakness does
not apply to HMAC-SHA-1 as a one-time-code PRF, and the vault uses SHA-1
nowhere else. RustCrypto, version 0.10, MIT/Apache-2.0, pure Rust, on `digest`
0.10 and `cpufeatures`, already in the tree.

## subtle

Constant-time comparison for the vault keyring: TOTP codes and backup-code verifiers are checked with `ConstantTimeEq`, so
how long a wrong one takes to reject says nothing about how close it was. Chosen over a
hand-written comparison loop, which the optimizer is free to shortcut.
dalek-cryptography, version 2, BSD-3-Clause, pure Rust, no dependencies of its
own. It brings nothing new: it was already in the tree, through `curve25519-dalek`
(for `ed25519-dalek`) and `digest` 0.10 (for `hmac`);
`hatchling-vault` now takes it directly. `hatchling-broker` takes it too: a harness relay checks each request's surrogate key with
`ConstantTimeEq`.
