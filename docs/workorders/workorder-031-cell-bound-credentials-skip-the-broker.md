# WO-031: Cell-bound credentials skip the broker

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (the keeper, the spawner, `browser_credentials`, `browser_ops`, the vault ops), `crates/hatchling-agent`, `crates/hatchling-browser` if the browser cell's controller changes, `docs/arch/`
**Issue:** [#227](https://github.com/undergroundrap/hatchling/issues/227)
**Trust boundary:** yes

## Problem

WO-017 split the trusted side so the broker holds no plaintext credential on the network side: the vault keeper releases egress credentials, vendor keys and the search key to the egress authority alone. Two kinds of credential are different, because they are meant for a cell, and the cell's one connection is to the broker:

- **A cell's vault secrets.** Under a `vault:fetch:<name>` grant, the keeper releases the secret, and the broker relays it to the agent cell. A `vault:store` sends a secret the other way, through the broker.
- **Browser sign-ins (WO-030).** They're sealed in a store of the broker's own (`HATCHLING_BROWSER_CREDENTIALS_DIR`), and the broker reads each one to fill it into the browser cell.

So a compromised broker can read every sign-in, and every secret a cell fetches or stores while it's compromised. That is exactly what WO-017's first acceptance criterion rules out, and why it was left out of WO-017 (the owner's call, 2026-10-28).

## Scope

**In scope:**
- A way for a credential meant for a cell to go from the keeper to that cell without the broker reading it, and back for a store. The broker still decides and records that an op is allowed; it never sees the bytes.
- The sign-in store moved to the keeper. Its files belong to `hatchling-keeper` in root mode. The keeper releases a sign-in for one fill, into its profile's browser, for its origin, and records it.
- The app's sign-in requests (keep, list, delete) answered through the keeper, still never through a conversation.
- The same split without root, as WO-017's processes have it.
- The trust boundary and the module map.

**Not in scope:**
- channel account credentials. No shipped channel uses one; they move with the first that does (WO-017's note);
- changing what a grant allows, or how the owner approves one;
- the egress side, which WO-017 finished.

## Acceptance criteria

- [ ] With a root broker, a cell's `vault:fetch` and `vault:store` of a canary secret, and the fill of a canary sign-in, succeed. Afterwards, a scan of the broker's memory finds no copy of either canary, and neither appears in any message the broker sent or received. Tested end to end in CI, as WO-017's hosted-model proof scans for the model credential.
- [ ] The keeper refuses a cell's vault op its grants don't allow. One cell's channel can't fetch or store for another cell. Both are tested with real processes.
- [ ] The sign-in store's directory and files are readable only by the keeper's user; read as the broker's user, it is refused. The broker opens no sign-in store.
- [ ] A sign-in is released only into its profile's browser, for its own origin: a fill for another origin or another profile is refused and recorded as denied. Tested.
- [ ] A hand mutant that has the broker relay the bytes again fails a test.
- [ ] `docs/arch/trust-boundary.md` and the module map show who holds each cell-bound credential, and who may talk to whom.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- **Where the bytes go today.** A cell's vault op is a session op on its connection to the broker. The broker asks the keeper on its own channel, and the keeper's `releases_to_cell` checks the configured grants and records the release before it answers. The broker then passes the answer to the cell. A sign-in fill reads the broker's own sealed store, and the broker types the values into the browser cell through its controller.
- **A shape to evaluate first.** The spawner, which already hands the broker each cell's connection, could also make a pair for the cell and the keeper: one end to the cell, the other to the keeper, with the cell's identity and grants. The broker would ask the keeper to release to cell N, and the keeper would answer on cell N's pair.
- **Open questions:**
  - **What the browser cell is shown.** The browser cell runs a real browser on hostile pages, so the origin it reports can't be trusted. The keeper must learn a fill's origin from something the page can't forge.
  - **How the keeper gets a cell's grants.** It could take them from the spawn, or read them from its own configuration as the egress authority does.
- **Order.** `vault:fetch` and `vault:store` first: the smaller change, and it sets the cell-to-keeper channel the sign-in fill reuses.
