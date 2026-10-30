# WO-031: Cell-bound credentials skip the broker

**Status:** DONE. Every acceptance criterion is met (see Evidence). A cell's `vault:store` is shown with real processes rather than end to end, because no shipped tool stores a secret yet (the owner's call, 2026-10-29).
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

- [x] With a root broker, a cell's `vault:fetch` and `vault:store` of a canary secret, and the fill of a canary sign-in, succeed. Afterwards, a scan of the broker's memory finds no copy of either canary, and neither appears in any message the broker sent or received. Tested end to end in CI, as WO-017's hosted-model proof scans for the model credential.
- [x] The keeper refuses a cell's vault op its grants don't allow. One cell's channel can't fetch or store for another cell. Both are tested with real processes.
- [x] The sign-in store's directory and files are readable only by the keeper's user; read as the broker's user, it is refused. The broker opens no sign-in store.
- [x] A sign-in is released only into its profile's browser, for its own origin: a fill for another origin or another profile is refused and recorded as denied. Tested.
- [x] A hand mutant that has the broker relay the bytes again fails a test.
- [x] `docs/arch/trust-boundary.md` and the module map show who holds each cell-bound credential, and who may talk to whom.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Each criterion, in order, and what shows it. Each PR's gate is in its description. The parts: 1a (#230), 1b (#231, #232), 1c (#233), 2a (#238), 2b (#239), 2c-i (#240), 2c-ii (#241); the keeper's grants per run, first (#229).

1. **Met for `vault:fetch` and the fill end to end; `vault:store` with real processes** (the owner's call, 2026-10-29).
   - **`vault:fetch` (#234):** CI's hosted-model proof, on a root broker dropped to `hatchling-broker`, has a chat run fetch a canary through its cell's vault channel. The model is shown it redacted, and neither a scan of the broker's memory (`memory_holds`) nor its log finds a copy.
   - **The fill (#243):** CI's browser proof keeps a canary sign-in, new each run, and the agent signs in to a test site with it. The site receives exactly that sign-in. Then neither the broker's memory, nor its log, nor any request the model was sent holds either half. Its first run found the owner's API frames left unwiped in the broker's memory, which held both halves; the same PR wipes them.
   - **`vault:store`:** no shipped tool sends one. The agent library has `Cell::vault_store`, but nothing the model drives calls it, so no run can store end to end. A store goes on the same vault channel as a fetch, of which the broker holds no end. The broker denies one sent to it (`a_vault_op_sent_to_the_broker_is_denied_when_a_keeper_runs`), and the real keeper stores on a cell's channel (`a_cells_vault_channel_is_answered_under_its_own_grants_alone`). The end-to-end store proof comes with the first tool that stores.
2. **Met, with real processes (#244).**
   - `a_cells_vault_channel_is_answered_under_its_own_grants_alone` starts the keeper's binary and hands it two cells' channels, as the spawner does. The cell with grants stores and fetches. The cell without is refused the store, and both fetches, before and after the other's store. The vault log records each op as its own cell's.
   - A hand mutant that answers a channel under the operator's grants, not the cell's, fails it ("another cell's store").
   - The broker can't name a cell to the keeper at all: `Request::Cell` is gone (#233).
3. **Met (#238, #244).**
   - The sign-in store is handed to `hatchling-keeper` at start (2a).
   - CI's browser proof, as root, checks after the fill that:
     - the store's directory admits no group or other;
     - every file in it is the keeper's;
     - listing or reading it as `hatchling-broker` is refused;
     - the running broker has no descriptor open under it.
4. **Met (#240, #241).**
   - `a_sign_in_is_filled_on_its_profiles_browser_channel_alone` refuses another profile's sign-in, a fill for another site, and a fill into a browser the keeper holds no channel of. The vault log records each as denied, and a fill that can't be recorded sends nothing.
   - `a_sign_in_is_refused_off_its_origin_outside_a_profile_and_unless_its_named` covers the broker's side. The controller types a fill only on the origin the keeper sent.
5. **Met.**
   - **The fill (#241):** a hand mutant that has the broker send the browser a sign-in's secrets again fails `a_sign_in_is_filled_in_its_profiles_browser_on_its_origin_and_never_shown`.
   - **Vault ops (#233):**
     - an agent that sends its vault ops to the broker fails `a_cells_vault_ops_go_to_its_vault_channel`;
     - a broker that answers one itself fails `a_vault_op_sent_to_the_broker_is_denied_when_a_keeper_runs`.
   - **End to end:** #243's run before the wipe is the browser proof failing on a real copy in the broker.
6. **Met.**
   - `docs/arch/trust-boundary.md`'s vault row (#233, #241) and the module map show that the keeper holds a cell's secrets and the sign-ins.
   - They show that each cell's vault channel goes from the spawner to the keeper, never through the broker.
   - They show that the broker names a fill to the browser by its nonce alone.

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
      - *2c-ii landed:* the broker's fill is `BrowserCommand::SignIn { nonce, username, password }`, after the keeper's fill (`Keeper::fill`, through `browser_ops::Fills`): it never has the secrets, and a broker without a keeper fills none. The controller reads the fills on its vault channel (`HATCHLING_VAULT_FD`), reading past at most 8 the broker never named, and types the one named into the fields named, on the origin the keeper sent; what it types is hidden as 2b's fills are. The keeper's wire has no read of a sign-in's secrets any more (`SignInRequest::Read` is gone), and the local store's `Credentials::sign_in` refuses when a keeper holds it.
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
