# hatchling-runtime

The `hatchling` command: personal AI agents you actually own.

Hatchling is an always-on agent that lives on your machine, thinks with the model you choose, and works while you're away. Everything the model drives runs in a kernel-isolated cell (namespaces, a seccomp allowlist, no capabilities, no network), and your keys stay in a vault outside it: the egress proxy swaps a surrogate for the real key at the boundary.

```bash
cargo install hatchling-runtime
hatchling --help
```

`hatchling broker` runs the broker and `hatchling agent` runs the agent. Set `HATCHLING_AGENT_BIN` to the `hatchling` binary's path, and the broker runs the agent from the same binary inside each cell.

Linux 5.11+ with unprivileged user namespaces, or WSL2. Configuration, the quickstart and the threat model are in the repository: <https://github.com/undergroundrap/hatchling>.

The other `hatchling-*` crates are parts of Hatchling, published so this one can be installed. They carry no stability promise on their own.

Licensed AGPL-3.0-or-later, with a commercial license for organizations that need other terms: <https://oceanbennett.com/hatchling/#licensing>.
