# Distribution and stability

What ships, where, and what each part promises.

## How you get it

| Way | What you get | State |
| --- | --- | --- |
| Release binaries ([releases](https://github.com/undergroundrap/hatchling/releases)) | `hatchling-broker`, `hatchling-agent`, `hatchling-host`, `hatchling-browser` and `hatchling` for x86_64 and aarch64 Linux (glibc 2.34+). Each comes with an SBOM and a provenance attestation, and can be rebuilt to the same bytes. | From 0.5.0 ([Verifying a release](../releasing.md#verifying-a-release)) |
| `cargo install --locked hatchling-runtime` | One `hatchling` binary that is both the broker and the agent | From 0.1.1 |
| From source | Every program, and the app | Always ([README](../../README.md#quickstart)) |
| Native packages (.deb, .rpm), a systemd unit, a one-command setup | | Planned |
| macOS, native Windows | | Not yet: WSL2 on Windows |

## Versions

- **One version for the whole workspace.** Every crate shares it and is released together. A release publishes every crate, changed or not.
- **Before 1.0, a breaking change bumps the minor version** (0.5.0 to 0.6.0), and anything else the patch. The `semver checks` job compares each crate's public API with its last release on crates.io, and fails a breaking change that doesn't bump the minor version.
- **Each version's changelog entry** names every breaking change, and the crates it breaks.
- **1.0** comes once the local API and the configuration have held up to outside use.

## What's stable, and what isn't

| Surface | Promise before 1.0 |
| --- | --- |
| **The local API** (`docs/local-api.md`) | Versioned: the UI schema is version 4 (`UI_SCHEMA_VERSION`), and the broker's session protocol version 17 (`PROTOCOL_VERSION`). A breaking change bumps the version, and a client and broker that disagree refuse each other. |
| **Configuration** (the `HATCHLING_*` variables, `--help`) | A breaking change is a minor version, and the changelog says how to move. |
| **The command lines** (`hatchling-broker`, `hatchling-agent`, `hatchling`) | Subcommand names are kept; a rename is a breaking change. |
| **The crates' Rust APIs** | Semver, checked by CI, but no stability beyond it: any 0.x minor may break any crate. They're published so that Hatchling can be installed from crates.io. Depend on one directly only if you'll follow Hatchling's releases. |
| **The skill format** | May change before 1.0, with a minor version. |

Everything not listed is internal, even if it's `pub` in Rust.

## System requirements

- **Linux:** kernel 5.11 or later, with unprivileged user namespaces and seccomp-BPF. Isolation is mandatory: without these, the broker refuses to start. There's no unsandboxed fallback.
- **Windows:** WSL2.
- **macOS:** not yet. The isolation layer is a platform-neutral trait, with no macOS backend.
