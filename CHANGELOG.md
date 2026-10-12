# Changelog

Every user-visible change to Hatchling, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). Before 1.0, a minor version (0.x.0) may change the wire protocol or configuration; the entry says so when it does.

Each pull request adds its line under **Unreleased**. When a version is cut, that section becomes the version's entry and its release notes. What's planned for the next versions is on the [milestones](https://github.com/undergroundrap/hatchling/milestones), in the order set in [Order of work](docs/workorders/README.md#order-of-work).

## [Unreleased]

### Fixed

- The harness relay holds at most 64 MiB of request bodies per run, across all its connections. Before, a cell could open 16 connections, each declaring a 32 MiB body, and the relay set aside each body before reading it and copied it again for the vendor: about 1 GiB of the broker's memory per run. A request past the budget is refused with a 503, the body is sent to the vendor without a second copy, a chunk size large enough to wrap the running total is refused, and a connection's place among the 16 is given back however its thread ends (WO-009).
- The broker resolves the egress allowlist once, and every session shares it. Before, each new agent session parsed the allowlist and looked up every destination's addresses again, so a destination whose DNS changed while the broker ran could be reached at addresses the broker had never checked. Now the first resolution is pinned for the broker's life; one that fails pins nothing, and the next session tries again (WO-009).
- A credential an upstream echoes back (an error quoting the request, a debugging endpoint) no longer reaches the cell. The egress proxy and the harness relay overwrite the real key with `*` wherever it appears in a response's headers or body, keeping its length; the relay does it as the answer streams, through chunked framing, so a key split across reads or chunks is still found. Responses are requested uncompressed, the cell's own `Accept-Encoding` and `TE` are dropped, and a compressed answer is refused, since compression would hide an echo (WO-009).
- Egress destinations written as numbers the system resolver reads as IPv4 addresses (`2130706433`, `0x7f000001`, `0177.0.0.1`, `127.1`), or with an all-digit last label, are refused like IP literals: they named no host the allowlist could vouch for ([WO-009](docs/workorders/workorder-009-egress-correctness.md)).
- Two different egress destinations that would share a vault secret name (`api.example.com:443` and `api-example.com:443`) are refused at startup: one would have been sent the other's credential (WO-009).
- The startup check that keeps vault grants off egress credentials now maps `public:`, `local:` and `local-keyless:` allowlist entries without their prefix, as the credential lookup does; before, a vault grant could name the credential of a prefixed entry that wasn't also an egress grant.

### Changed

- **Breaking, `hatchling-broker`:** `EgressConfig::allowlist_raw: String` is now `EgressConfig::allowlist: EgressAllowlist`, which holds the operator's text and its one shared resolution: build it with `EgressAllowlist::new(text)` (or `default()` for none), and read the text with `raw()`. The workspace version is 0.2.0. `hatchling-egress` adds `Proxy::shared`, a proxy over an allowlist already resolved, and `Proxy::allowlist`.
- The AGPL license gains three additional terms under its section 7, in `NOTICE.md`: a work built on Hatchling keeps the credit "Built on Hatchling by Ocean Bennett", a modified version is marked as modified and doesn't present itself as Hatchling, and the name and logo aren't licensed. `TRADEMARKS.md` explains, in plain words, what forks can and can't call themselves. Every crate's docs and crates.io page point to the terms. The license tag stays `AGPL-3.0-or-later`.

## [0.1.2] - 2026-10-11

Publishing, documentation and metadata. No change to the broker's or the agent's behavior, or to the wire protocol.

### Added

- The module map shows how the crates connect: `hatchling-runtime` at the front, the trusted side (broker, vault, egress, isolation) and the cell side (agent, loop, model), and which crates are published. Every crate's crates.io page links to it.
- `hatchling-dev audit` fails the gate if a crate has no row in the module map, if the map names a crate that doesn't exist, or if `hatchling-agent` depends on the trusted side, directly or through another crate.
- Releases publish to crates.io from CI when a GitHub release is published, through trusted publishing: no crates.io token is stored anywhere. The workflow refuses a release whose tag, workspace version and changelog disagree.
- `cargo semver-checks` runs on every pull request against the crates on crates.io.
- crates.io and docs.rs badges, and `cargo install hatchling-runtime` as an install option in the quickstart.
- CONTRIBUTING says when new code earns its own crate, and what each published crate costs.
- `docs/releasing.md`: how a version is cut and published, the trusted-publishing settings, and a new crate's first publish. `scripts/publish-crates.sh` publishes in dependency order for both CI and the maintainer, skips crates already on crates.io at the version (so a failed run is simply re-run), and in CI skips and names crates new to crates.io.
- `scripts/setup-trusted-publishing.sh` makes every published crate trust the release workflow through crates.io's API, with a token scoped to `trusted-publishing` only, read without echoing; it skips crates already set up.
- crates.io pages: `hatchling-runtime` gets a README written for crates.io (the logo, why Hatchling is different, install, the security model in brief, links), and every other crate's page links to it and to the repository.

### Changed

- Keywords and categories are per crate, each describing what that crate does: `hatchling-runtime` carries the headline terms, and every other crate its own (model providers, tool-calling, cron, signed skills, seccomp, FIDO2 and so on), so each reaches a different search and leads back to Hatchling.

## [0.1.1] - 2026-10-11

The first release on crates.io. No change to the broker, the agent or the wire protocol.

### Added

- `hatchling-runtime`, the crate that installs one `hatchling` command: `hatchling broker` and `hatchling agent` run the broker and the agent with their existing command lines. Started as a cell's `/agent`, it is the agent, so `HATCHLING_AGENT_BIN` can point at it too ([WO-016](docs/workorders/workorder-016-publish-to-crates-io.md)).
- The shipped crates are published to crates.io; development and test crates are not.
- CI packages and verifies every published crate on each pull request, and runs the real-cell suites for the broker's spawner helpers through `hatchling` as well as `hatchling-broker`.
- This changelog, and a milestone per planned version: v0.2.0 (what a hostile cell can do), v0.3.0 (what a local attacker can do), v0.4.0 (what a security review asks for next).

### Changed

- Every crate shares the workspace version, and depends on the others through `[workspace.dependencies]`, so a release bumps one line.
- `cargo deny` exempts our own published crates from the license allowlist by name; a third-party AGPL dependency still fails it.

## [0.1.0] - 2026-10-11

The first public release. [Release notes](https://github.com/undergroundrap/hatchling/releases/tag/v0.1.0).

### Added

- Cells: user, mount, PID, network and IPC namespaces, a seccomp-BPF allowlist, and every capability dropped, around every agent. Hatchling refuses to start if the kernel can't build one.
- A vault sealed at rest (XChaCha20-Poly1305), unlocked by key file, authenticator codes or a FIDO2 security key, with backup codes.
- An egress proxy: no network by default, an exact-match allowlist per grant, TLS for remote hosts, and a surrogate swapped for the real key at the boundary.
- Any OpenAI-compatible model; Claude Code and Codex CLI, pinned and verified, inside cells.
- Scheduling, memory, goals and a digest feed; signed skills; artifacts inspected at the boundary; shared spaces.
- A local Unix-socket JSON API with a generated JSON Schema.
- A TypeScript frontend, running on scripted data.
- Dual licensing (AGPL-3.0-or-later and a commercial license) and a contributor license agreement.

### Known limitations

Published in [the trust boundary](docs/arch/trust-boundary.md#known-limitations), each with a work order and an issue.

[Unreleased]: https://github.com/undergroundrap/hatchling/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/undergroundrap/hatchling/releases/tag/v0.1.2
[0.1.1]: https://github.com/undergroundrap/hatchling/releases/tag/v0.1.1
[0.1.0]: https://github.com/undergroundrap/hatchling/releases/tag/v0.1.0
