# WO-023: Refuse internal addresses for destinations that aren't declared internal

**Status:** DONE
**Area:** `crates/hatchling-egress` (`allowlist`), `crates/hatchling-broker` (the coding-CLI relay's vendor destinations)
**Issue:** [#81](https://github.com/undergroundrap/hatchling/issues/81)
**Trust boundary:** yes

## Problem

The egress allowlist resolves each destination once, at startup, and the proxy connects only to those pinned addresses, so DNS can't move a destination later (no rebinding). A `local:` entry must resolve to loopback, or the broker refuses to start. But any other entry is accepted whatever it resolves to: a destination that, at startup, resolves to a loopback, private, link-local or cloud-metadata address is pinned there without a word. That happens through DNS poisoning at the moment the broker starts, or a typo or stale record on the operator's side, and it gives a cell a path to a service on the operator's own network.

Verified TLS already blocks most of what that path could reach: every non-local destination is HTTPS, checked against the allowlisted name, so a plain-HTTP metadata endpoint or an internal service without a certificate for that name fails the handshake. It doesn't stop an internal service whose certificate is valid for the name: a TLS-intercepting corporate proxy, or a service the operator's own CA bundle (`HATCHLING_EGRESS_CA_BUNDLE`) covers.

Some deployments reach internal addresses on purpose: split-horizon DNS, or a company's model gateway on its own network. Refusing those outright would break them.

## Scope

**In scope:**

- **Refuse at startup.** When any address a non-local destination resolves to is loopback, private (RFC 1918), link-local, shared (100.64.0.0/10), IPv6 unique-local or link-local, unspecified, multicast, or a known metadata address, the allowlist fails closed, naming the entry and the address. An IPv4 address mapped into IPv6 (`::ffff:10.0.0.1`) is judged as the IPv4 address it is.
- **Declare an internal destination.** An entry written `internal:host:port` may resolve to such addresses: going internal is always written down, as going keyless is for `local-keyless:`. It is otherwise an ordinary credentialed destination, held to verified TLS. Listing a destination both ways fails closed, as for the other qualifiers.
- **Every way out.** The coding-CLI relay's vendor destinations are held to the same check.
- **The docs:** the allowlist's settings, and the trust boundary's egress rows.

**Not in scope:** resolving at request time (it would bring back rebinding); IP literals and ranges in the allowlist, which stay refused; an operator's own firewall.

## Acceptance criteria

- [x] A non-local destination resolving to any address in the refused ranges fails the allowlist at startup, naming the entry and the address; each range is tested, an IPv4-mapped IPv6 address among them.
- [x] The same destination written `internal:` is accepted, pinned and served over verified TLS; tested.
- [x] A destination listed both as `internal:` and as another kind fails closed; tested.
- [x] The coding-CLI relay's vendor destinations are held to the same rule; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

One pull request, [#98](https://github.com/undergroundrap/hatchling/pull/98). Hardened WSL2 Ubuntu, `HATCHLING_REQUIRE_CELL=1`: plain clippy clean on Rust 1.98.1 and 1.85.0; `check.sh` green on both, 2,235 tests each; mutation on the changed lines of the allowlist, the broker's config and the harness: 32 mutants, 27 caught, 5 unviable, 0 missed, 0 timeouts, 0 orphans. Six hand mutants failed their tests: an IPv4-mapped address not judged as IPv4, a public source or an ordinary destination not checked, the metadata address or the shared range left out, and the coding-CLI relay's vendor resolved around the rule.

**How.** `is_internal` judges an address against the ranges in the scope; the allowlist checks every address a destination resolves to at startup, where the addresses are pinned, and fails closed naming the entry, the address and the fix (`write it internal:host:port`). `internal:` is a fourth kind, an ordinary credentialed HTTPS destination that may resolve inside. `Allowlist::parse_with` takes the resolver, so the tests put a name on any address, inside or out, without touching the network. The coding-CLI relay's vendor setting goes through the same parse.

**The acceptance criteria, as tested:**

- **Each refused range, at startup:** `each_internal_range_is_internal_and_the_internet_is_not` covers loopback, the three private ranges, link-local and the metadata address, shared (Alibaba's metadata among it), this network, broadcast, multicast, Azure's wire server, and IPv6 loopback, unspecified, unique-local (AWS's metadata among it), link-local and multicast, with IPv4-mapped forms, beside public neighbours just outside each range. `a_destination_resolving_inside_fails_unless_written_internal` shows an ordinary destination and a public source refused for an internal address, even one among public ones, with the entry, the address and the fix named.
- **`internal:` accepted, pinned and served over verified TLS:** the same test shows it allowed and pinned to exactly its addresses; `an_https_destination_speaks_tls_to_its_own_name` (the relay) serves one over TLS checked against its name, and refuses another name's certificate; and CI's hosted-model proof, whose stand-in vendor `api.example.test` resolves to `127.0.0.1`, now runs written `internal:`.
- **One kind:** `an_internal_destination_is_one_kind`, with `internal:` beside a plain, public or local listing of the same destination.
- **The coding-CLI relay:** `a_vendor_api_is_one_destination_pinned_now` refuses a vendor resolving inside, and serves the same written `internal:` under its bare name.

**What it changes for an operator:** an allowlist entry that resolves to an internal address now stops the broker at start, saying which entry, which address, and what to write if it is meant. Tests and fixtures that used `localhost` as an ordinary destination now write `internal:`.

## Notes

Raised in a review on 2026-10-16. The check sits where the addresses are pinned, at startup, because that is the moment they are decided: after it, the proxy never resolves again.
