# WO-030: Signed-in browsing: sessions sealed per site, sign-in filled by the broker, the agent never in the secret path

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (profiles, the fill, the session's sites), `crates/hatchling-vault` (two sealed stores: cookie jars and site credentials), `crates/hatchling-browser` (reading and restoring cookies, the fill), `crates/hatchling-core` (the ops; the UI's surfaces), `crates/hatchling-agent` (the fill tool), `frontend/` (signed-in sites, credentials, codes)
**Issue:** none yet
**Trust boundary:** yes

## Problem

WO-029's browser starts from nothing every time: no cookies, no site storage, nothing kept. That's deliberate, and it leaves a real gap. Someone who asks Hatchling to check an order, pay a bill or answer a message needs a browser that's signed in, and other agents' browsers stay signed in.

Their browsers stay signed in because they hold the state loosely: the profile on disk, the password in the model's context, one browser for every site. Each of those is a way to lose an account. A prompt-injected agent can type a password into the wrong page; a renderer exploit on one site reaches every other site's session; a compromised browser can plant something in the profile that wakes up next time.

The fix isn't to loosen the browser cell. It's what WO-017 did for keys and WO-027 for approvals: the broker holds the state, the owner decides, and the agent is never in the path of a secret.

## Scope

**In scope:**

- **A signed-in profile per site, in a cell of its own.** A profile is one site the owner signs in to, with the sites its sign-in goes through (an identity provider, its scripts). Each runs in its own browser cell; two profiles never share one. Chromium runs without its own sandbox (WO-029), so a cell is the only thing between two sites' sessions: this rebuilds site isolation at the kernel instead of in the browser.
- **A signed-in cell reaches only its profile's sites.** WO-029's strict mode, made mandatory here: the proxy refuses any origin that isn't the profile's. A flow that wanders to another site (a payment processor, a CDN on another domain) asks the owner, as WO-028 asks for a site: allow it once, or add it to the profile. Only an origin added to the profile joins the cell's set.
- **The session is kept as cookies, never as a profile directory.**
  - At the end of a session the broker reads the cell's cookie jar out over the DevTools protocol, as structured data, and seals that (a new vault store with its own key, WO-017's pattern). `localStorage` for the profile's own origins, if a site needs it, the same way.
  - At the start of the next, Chromium starts from an empty profile and the broker restores only the cookies whose domain is one of the profile's sites. A cookie for any other domain in the jar is dropped, and logged.
  - Nothing executable persists: no service worker, no cache, no extension state, no profile file. A compromise lives as long as its session.
  - No op returns a cookie or a stored value; snapshots stay page text and elements.
- **Credentials, filled by the broker.**
  - A site credential is stored for one profile and one exact origin (scheme, host, port), sealed in its own vault store. The owner enters it in the app, never through a conversation, as approvals are decided.
  - The agent has one op: fill the sign-in form on the current page, naming the fields from its snapshot. It learns only that the form was filled, that the profile has no credential for this origin, or that the fill was refused.
  - The broker fills only into the cell of the profile the credential belongs to, only when the controller reports the credential's exact origin as the top frame's, and only into the named fields. The value goes from the vault to the broker to that cell's controller to the page (`Input.insertText`); never to the agent, a snapshot, the action log, or the broker's own log.
  - A password field is masked in every snapshot, whatever it holds.
  - Every fill is in the action log with its origin and the credential's label, never its value.
- **One-time codes.** When a page asks for a code, the owner is asked in the app, types it there, and the broker fills it through the same path. Optionally, a TOTP seed kept in the vault, the code computed by the broker: the owner opts in per site, told that it puts both factors in Hatchling's keeping.
- **The owner manages it.** A list of signed-in profiles with their sites, when each was last used, and their credentials by label; signing out wipes a profile's jar; deleting a credential deletes it. Taking control (WO-029) is the manual way in: the owner signs in themselves, and the jar keeps the session.
- **Docs:** the threat model, the configuration, the local API, the CHANGELOG.

**Not in scope:**

- passkeys and WebAuthn (the next frontier: a key the broker holds, used through the controller's virtual authenticator, is a WO of its own);
- importing passwords from another manager;
- the agent reading back any credential, cookie or stored value, ever;
- a profile shared between owners, or across spaces.

## Acceptance criteria

- [ ] A credential stored for origin A is never filled on origin B, along every navigation path the tests can build: a redirect, a subframe, a look-alike host, a page that changes its URL with script, a controller that reports A while the page is B.
- [ ] After a full sign-in flow, the agent's transcript, its snapshots, the action log and the broker's log contain no credential, cookie value or code: checked by scanning for each.
- [ ] A cookie for a domain outside the profile's sites, planted in the jar by a page, isn't restored next session; a service worker a page registers isn't there next session.
- [ ] A signed-in cell can't reach an origin outside its profile without the owner: tested with a page's script, a redirect and a subresource.
- [ ] Two profiles never share a cell; a profile's credentials and cookies are only ever sent to its own cell.
- [ ] Killing a cell mid-session leaves no readable profile anywhere: the cell's storage is gone, and the jar sealed is the last one read.
- [ ] Signing out ends the site's session at the next launch.
- [ ] A one-time code typed in the app is filled once, into the page that asked, and not kept.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates). The browser tests run on the pinned Chromium, in the sandbox and in CI, as WO-029's do.

## Evidence

Filled in at close.

## Parts

1. **Sealed sessions per site:** profiles, a cell each, strict mode for their sites, the jar read out and sealed at the end, restored filtered at the start, the owner's list and sign-out. With taking control, this alone closes most of the gap.
2. **The broker's fill:** site credentials in the vault, entered in the app; the fill op, bound to the profile's cell and the exact origin; masked fields; the action log.
3. **Codes:** asked of the owner in the app and filled; TOTP seeds, opt-in.

## Notes

- **Name the threat, then the mechanism.**
  - *A prompt-injected agent sending a password to the wrong page:* the agent never has it, and a fill goes only to its own profile's cell, on its own origin.
  - *A look-alike host or a redirect catching a fill:* a look-alike isn't one of the profile's sites, so it can't load in that cell at all. Impossible, not checked.
  - *A renderer exploit on one site reaching another's session:* one profile per cell.
  - *A compromised renderer sending a filled password away:* the signed-in cell reaches only its profile's sites.
  - *A compromise outliving its session* (a service worker, poisoned storage): only cookies persist, filtered to the profile's domains; every session starts from an empty profile.
  - *A stolen disk:* jars and credentials are sealed under their own store keys, wrapped under the master key.
- **What the origin check is worth.** The broker learns the page's origin from the controller, which runs beside page code. So the check keeps an honest browser honest; it's the cell (one profile, strict mode) that holds against a compromised one. A password typed into a page in a browser without its own sandbox is in everything in that cell, which is why the cell holds one profile and reaches nothing else.
- **A second kind of secret.** WO-017's keeper releases a credential only into an outgoing request it has checked. A site credential is different by design: its use is to be typed into its own site, so its one destination is its own profile's cell. The keeper releases it to the broker for that alone, under its own purpose, and no grant names it; it never reaches an agent's cell.
- **The cost.** More cells, and a signed-in flow that wanders to an unlisted site stops and asks. That's the trade for a session that holds an account: the capability is there, caged per site.
- **Order.** After WO-029, whose cell, controller, proxy and owner takeover this builds on. Part 2 needs WO-017's keeper for its release.
