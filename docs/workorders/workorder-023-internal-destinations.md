# WO-023: Refuse internal addresses for destinations that aren't declared internal

**Status:** QUEUED
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

- [ ] A non-local destination resolving to any address in the refused ranges fails the allowlist at startup, naming the entry and the address; each range is tested, an IPv4-mapped IPv6 address among them.
- [ ] The same destination written `internal:` is accepted, pinned and served over verified TLS; tested.
- [ ] A destination listed both as `internal:` and as another kind fails closed; tested.
- [ ] The coding-CLI relay's vendor destinations are held to the same rule; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

Raised in a review on 2026-10-16. The check sits where the addresses are pinned, at startup, because that is the moment they are decided: after it, the proxy never resolves again.
