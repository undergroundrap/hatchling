# Changelog

Every user-visible change to Hatchling, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). Before 1.0, a minor version (0.x.0) may change the wire protocol or configuration; the entry says so when it does.

Each pull request adds its line under **Unreleased**. When a version is cut, that section becomes the version's entry and its release notes. What's planned for the next versions is on the [milestones](https://github.com/undergroundrap/hatchling/milestones), in the order set in [Order of work](docs/workorders/README.md#order-of-work).

## [Unreleased]

## [0.1.2] - 2026-10-11

Publishing and metadata only. No change to the broker, the agent or the wire protocol.

### Added

- Releases publish to crates.io from CI when a GitHub release is published, through trusted publishing: no crates.io token is stored anywhere. The workflow refuses a release whose tag, workspace version and changelog disagree.
- `cargo semver-checks` runs on every pull request against the crates on crates.io.
- crates.io and docs.rs badges, and `cargo install hatchling-runtime` as an install option in the quickstart.
- CONTRIBUTING says when new code earns its own crate, and what each published crate costs.
- `docs/releasing.md`: how a version is cut and published, the trusted-publishing settings, and a new crate's first publish. `scripts/publish-crates.sh` publishes in dependency order for both CI and the maintainer, skips crates already on crates.io at the version (so a failed run is simply re-run), and in CI skips and names crates new to crates.io.

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
