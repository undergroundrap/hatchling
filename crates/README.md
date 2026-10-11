<p align="center">
  <img src="https://raw.githubusercontent.com/undergroundrap/hatchling/main/assets/hatchling-logo.png" alt="Hatchling: a chick hatching from an egg" width="96">
</p>

# A part of Hatchling

This crate is one part of **[Hatchling](https://github.com/undergroundrap/hatchling)**: personal AI agents you actually own, in kernel-isolated cells that never hold your keys.

You probably want **[hatchling-runtime](https://crates.io/crates/hatchling-runtime)**, which installs the whole runtime as one `hatchling` command:

```bash
cargo install hatchling-runtime
```

This crate is published so that one can be installed. It carries no stability promise on its own: its API changes whenever Hatchling needs it to, and a 0.x minor version may break it. Depend on it directly only if you're prepared to follow Hatchling's releases.

[GitHub](https://github.com/undergroundrap/hatchling) · [Quickstart](https://github.com/undergroundrap/hatchling#quickstart) · [Threat model](https://github.com/undergroundrap/hatchling/blob/main/docs/arch/trust-boundary.md) · [Website](https://oceanbennett.com/hatchling/)

Licensed AGPL-3.0-or-later, with a [commercial license](https://oceanbennett.com/hatchling/#licensing) for organizations that need other terms.
