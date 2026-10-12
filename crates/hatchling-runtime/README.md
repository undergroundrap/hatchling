<p align="center">
  <img src="https://raw.githubusercontent.com/undergroundrap/hatchling/main/assets/hatchling-logo.png" alt="Hatchling: a chick hatching from an egg" width="160">
</p>

<h1 align="center">Hatchling</h1>

<p align="center"><strong>Personal AI agents you actually own.</strong></p>

<p align="center">
  <a href="https://github.com/undergroundrap/hatchling">GitHub</a> ·
  <a href="https://github.com/undergroundrap/hatchling#quickstart">Quickstart</a> ·
  <a href="https://github.com/undergroundrap/hatchling/blob/main/docs/arch/trust-boundary.md">Threat model</a> ·
  <a href="https://oceanbennett.com/hatchling/">Website</a>
</p>

An always-on agent that lives on your machine, thinks with the model you choose, works while you're away, and is built so it **cannot** see your keys, read your files, or reach the internet unless you granted exactly that.

This crate installs the `hatchling` command, the whole runtime in one binary.

```bash
cargo install hatchling-runtime
hatchling --help
```

## Why it's different

Most agent frameworks start with a powerful agent and add guardrails around it. Hatchling starts with the boundary, and gives the agent only what you grant.

- **Every agent runs in a cell:** its own Linux user, mount, PID, network and IPC namespaces, a seccomp-BPF syscall allowlist, and every capability dropped. If the kernel can't build a cell, Hatchling refuses to start rather than run without one.
- **Your keys never enter the cell.** They live in a vault sealed at rest, unlocked by key file, authenticator codes or a FIDO2 security key. When the agent calls an API, the egress proxy swaps a surrogate for the real key at the boundary.
- **No network by default.** A cell reaches the outside only through the egress proxy, per grant, against an exact-match allowlist.
- **Your model:** any OpenAI-compatible endpoint (Ollama, llama.cpp, LM Studio, vLLM, or a hosted API). Claude Code and Codex CLI run inside cells too.

## Running it

`cargo install hatchling-runtime` installs two programs. `hatchling broker` runs the broker, which holds the vault, the network and the schedules. `hatchling-cell` is the program the broker runs inside each cell: the agent alone, carrying none of the broker's code (a test checks the built binary). Point `HATCHLING_AGENT_BIN` at it.

Linux 5.11+ with unprivileged user namespaces, or WSL2. The broker runs as root so each cell gets a uid of its own. The [quickstart](https://github.com/undergroundrap/hatchling#quickstart) takes you from here to an agent answering from inside a cell, with a stand-in model, so no API key is needed.

Hatchling is pre-1.0. Its own review of the trust boundary found gaps, and they're published, each with a work order: [known limitations](https://github.com/undergroundrap/hatchling/blob/main/docs/arch/trust-boundary.md#known-limitations).

## The other crates

The other `hatchling-*` crates are parts of Hatchling, published so this one can be installed. They carry no stability promise on their own. How they fit together, and why the code that runs in a cell never links the code that holds your keys: the [module map](https://github.com/undergroundrap/hatchling/blob/main/docs/arch/module-map.md).

## License

AGPL-3.0-or-later, with a [commercial license](https://oceanbennett.com/hatchling/#licensing) for organizations that need other terms.
