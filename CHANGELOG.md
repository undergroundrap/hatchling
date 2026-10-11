# Changelog

Every user-visible change to Hatchling, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). Before 1.0, a minor version (0.x.0) may change the wire protocol or configuration; the entry says so when it does.

Each pull request adds its line under **Unreleased**. When a version is cut, that section becomes the version's entry and its release notes. What's planned for the next versions is on the [milestones](https://github.com/undergroundrap/hatchling/milestones), in the order set in [Order of work](docs/workorders/README.md#order-of-work).

## [Unreleased]

### Added

- This changelog, and a milestone per planned version: v0.2.0 (what a hostile cell can do), v0.3.0 (what a local attacker can do), v0.4.0 (what a security review asks for next).

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

[Unreleased]: https://github.com/undergroundrap/hatchling/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/undergroundrap/hatchling/releases/tag/v0.1.0
