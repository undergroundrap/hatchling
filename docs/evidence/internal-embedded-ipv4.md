# Evidence: an internal address is refused in every IPv6 form that carries it (#682)

| | |
|---|---|
| Base | `main` at `76ebdf59` (#693) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Found by | an outside model's review of `allowlist.rs`, confirmed in the code |

## What was wrong

`is_internal` refuses internal addresses in three places:
- the allowlist at startup;
- `web_fetch`;
- the browser's proxy.

The last two reach sites whose DNS anyone controls. It judged an IPv4 address mapped into IPv6 (`::ffff:10.0.0.1`) by the IPv4 address it carries, but no other form that carries one.

On an IPv6-only network with DNS64 and a NAT64 gateway, `64:ff9b::a9fe:a9fe` is routed to `169.254.169.254`, the cloud metadata address. It was judged a public IPv6 address. So a site whose name resolved to it, or to `64:ff9b::10.0.0.1`, reached the owner's network through `web_fetch` or the browser. 6to4 (`2002::/16`) carried the same gap.

## What it does now

- **Judged by what they carry:** mapped, well-known NAT64 (`64:ff9b::/96`, its last 32 bits) and 6to4 (`2002::/16`, bits 16 to 47). Refusing NAT64 outright would refuse every site on a DNS64 network, where public names resolve into it.
- **Internal whatever they carry:**
  - local-use NAT64 (`64:ff9b:1::/48`), whose embedding is the network's own;
  - Teredo (`2001::/32`) and IPv4-compatible (`::a.b.c.d`), both deprecated and needed by no destination.
- **More IPv4 ranges internal**, none of them a public site:
  - reserved `240.0.0.0/4` (broadcast included);
  - `192.0.0.0/24`, IETF protocol assignments;
  - `198.18.0.0/15`, benchmarking;
  - the three documentation TEST-NETs.

Two tests used `203.0.113.5`, a documentation address, as a public source's address. They now use a public one they never connect to.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | GATE |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | GATE |
| the whole workspace's tests | GATE |
| `hatchling-dev audit` | GATE |

Extended tests:
- `each_internal_range_is_internal_and_the_internet_is_not`:
  - internal: each new IPv4 range at its edges; NAT64 and 6to4 around internal IPv4 (the metadata address, `10.0.0.1`, loopback, `192.168.1.1`); local-use NAT64 around a public address; Teredo; IPv4-compatible around both;
  - public: the addresses just outside each new range; NAT64 and 6to4 around `8.8.8.8` and `1.1.1.1`; an ordinary `2001:` address.
- `a_host_resolving_inside_or_nowhere_is_refused_unconnected`: a page whose name resolves, by DNS64, to `64:ff9b::a9fe:a9fe` is refused before connecting.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| NAT64 not judged by what it carries | GATE |
| 6to4 not judged by what it carries | GATE |
| Teredo taken as public | GATE |
| Documentation ranges taken as public | GATE |

**Mutation**, on the lines changed against `76ebdf59` (`cargo mutants --in-diff`, `hatchling-egress`):

GATE
