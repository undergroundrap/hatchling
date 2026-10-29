# WO-031: Cell-bound credentials skip the broker

**Status:** IN-PROGRESS
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
- **Slice 2's design, after mapping the fill (2026-10-29).**
  - **Where a sign-in goes today:**
    - the owner keeps it from the app, which sends it to the broker;
    - the broker seals it in its own store (`Store::CREDENTIALS`), with a key it holds;
    - on the agent's `browser_sign_in`, the broker reads it, sends each field to the browser cell in a `BrowserCommand::Fill`, and keeps the values (`BrowserSession::filled`) so it can scrub them from every snapshot and answer before the agent sees them.

    Moving the store alone wouldn't take the broker out of the path: the scrub needs the values too.
  - **The threat it stops, and the one it doesn't.**
    - **Stopped:** a broker bug that discloses its memory or its logs reveals no sign-in. The broker would hold no store key, no fill, and no scrub list.
    - **Narrowed, not stopped:** a fully compromised broker. It runs the browser's network, and the operator's certificate authorities reach the browser through it. So it could open a browser on a site it serves, and ask for a fill there. Every release is then the keeper's decision, bound to a browser cell and its signed-in site, and recorded. The page's origin is still checked in the cell before anything is typed.
  - **The design:**
    1. **The store is the keeper's.** `Store::CREDENTIALS` leaves the broker's stores, and its key stays with the keeper. The app's keep, list and delete go through the broker to the keeper. A kept sign-in passes through the broker once, at the owner's own keep, in wiped buffers. The alternative, sealing it in the app to a keeper key, would take that key from the broker, which could swap it, so it isn't worth its complexity.
    2. **A browser cell gets a vault channel,** as agent cells do since slice 1. The spawner names it with the cell, and the profile and site it was opened for.
    3. **A fill is the keeper's.** The broker asks the keeper to fill a sign-in (by id) into a browser cell, with the fields' elements. The keeper checks that the sign-in belongs to the profile and site the cell's channel was named with, records the release, and sends the fill on the cell's channel. It never answers the broker with the values.
    4. **The controller types it, and scrubs it.** In the cell, the controller checks the page's top frame is on the sign-in's origin, as `Fill` does now. It types the values, keeps them for the session, and scrubs them from every snapshot and answer before anything leaves the cell. `BrowserSession::filled` goes from the broker.
  - **Parts:**
    - **2a:** the keeper holds the store and answers the app's requests;
      - *2a landed:* the sign-ins are the keeper's store, beside the vault: the broker is handed no key to them, and its user and its Landlock rules lose their directory. The broker checks a new sign-in is for one of its profile's sites, then asks the keeper to keep it; it asks for the list and each deletion too. Until 2b, a fill still asks the keeper for one sign-in's secrets, which the keeper records in the vault's access log first (`browser-fill`), and sends as raw frames, never as JSON.
    - **2b:** the scrub moves into the controller, and the broker's copy goes. It comes before the fill moves: a broker that scrubbed what was filled would still need the values.
      - *2b landed:* the controller keeps what each fill types (a sign-in, a one-time code) until the browser closes, and hides it from every snapshot after (`PageSnapshot::hiding`, in `hatchling-core`). The broker keeps none of it; it still hides a signed-in session's cookie values, which it reads to keep the session. Until 2c, the broker still has a fill's values while it sends them.
    - **2c:** browser cells' vault channels, and the fill through them, in two parts:
      - **2c-i:** a browser started for a signed-in profile gets a vault channel, which the spawner hands the keeper named with the profile; the keeper fills that profile's sign-ins on it, recorded before each is sent, and tells the broker the fill's nonce alone.
      - **2c-ii:** the controller types a fill from its vault channel when the broker names its nonce, on the keeper's origin; the broker fills through the keeper, and its read of a sign-in's secrets goes.
      - *2c-i landed:* the profile is named at the browser's start (`CellConfig::sign_ins_for`), only through a spawner that hands vault channels to the keeper. The keeper holds a browser's channel until the cell ends it, refuses a second for the same cell, and drops one a fill can't be written to. A fill (`Request::Fill`) names the sign-in, the cell and the origin the broker saw: the keeper refuses one of another profile's sign-in, for another site, or into a browser it holds no channel of, and records it as denied (`fill`, `browser-fill`). A browser whose profile the owner kept while it ran (WO-030's *Keep me signed in*) has no channel, and gets no fill until it's started again. Nothing asks for a fill until 2c-ii.
- **Order.** `vault:fetch` and `vault:store` first: the smaller change, and it sets the cell-to-keeper channel the sign-in fill reuses.
- **Slice 1's design, after mapping the cell's vault path (2026-10-28).**
  - **What the map turned up:**
    - The keeper answered every run's vault op under the operator's grants, not the run's: fixed first, in #229.
    - The broker can name any cell and uid in a `Request::Cell`, so a compromised broker can ask for any granted secret, as any cell. This slice ends that: once every cell has its own channel, the keeper refuses `Request::Cell` on the broker's.
    - Only agent cells send vault ops: the broker's own agent cell, and the cells of scheduled runs, subagents and chats. Skill cells have no connection, and the broker is the client of a browser cell's.
    - The spawner has no channel to the keeper. It is multi-threaded, so it can't hand a descriptor to a helper's exec without racing its other spawns. The helper's stdin, stdout and stderr are taken.
  - **The grants.** The spawner can't know a run's grants, because they come from the broker's schedule store. The broker names them in the cell's spec, and the keeper answers under those it also makes. The broker still decides what a run may do; it never sees the bytes.
  - **Parts, each landing and gated on its own:**
    - **1a:** the keeper takes a third pair at launch, for the spawner. On it, the keeper receives each cell's channel, named with the cell, its uid and its grants, and answers vault ops alone on it, as session requests, recorded as that cell's. Until 1b, the broker closes the spawner's end.
    - **1b:** the spawner holds that end. A cell's spec asks for a vault channel. The isolation layer makes the pair inside the spawn, as it makes the broker's connection, and the cell finds its end in an environment variable. The helper hands the keeper's end back to the spawner, which names it and hands it to the keeper.
    - **1c:** the agent sends its vault ops on that channel when it has one. The broker answers a vault op on its own session `Denied`, and the keeper refuses `Request::Cell` on the broker's channel. The adversarial probe and the run fixture's descriptor list change with it.
    - *Evidence (#234):* CI's hosted-model proof, on a root broker dropped to `hatchling-broker`, has a chat run fetch a canary through its vault channel. The model is shown it redacted, and neither the broker's memory nor its log holds a copy.
    - *1a landed (#230), 1b in two halves (#231, #232).* *1c landed:* the agent adopts its vault channel with its connection, holds its peer to the broker's uid, and sends its vault ops there. With a keeper running, the broker denies a vault op on its session, and `Request::Cell` and `Keeper::cell_op` are gone, not refused: the keeper's wire has no way for the broker to name a cell. Neither the adversarial probe nor the run fixture changed: their tests run without a keeper, where the broker answers vault ops as before.
