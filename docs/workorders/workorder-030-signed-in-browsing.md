# WO-030: Signed-in browsing: sessions sealed per site, sign-in filled by the broker, the agent never in the secret path

**Status:** DONE (2026-10-30)
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

- [x] A credential stored for origin A is never filled on origin B, along every navigation path the tests can build: a redirect, a subframe, a look-alike host, a page that changes its URL with script, a controller that reports A while the page is B.
- [x] After a full sign-in flow, the agent's transcript, its snapshots, the action log and the broker's log contain no credential, cookie value or code: checked by scanning for each.
- [x] A cookie for a domain outside the profile's sites, planted in the jar by a page, isn't restored next session; a service worker a page registers isn't there next session.
- [x] A signed-in cell can't reach an origin outside its profile without the owner: tested with a page's script, a redirect and a subresource.
- [x] Two profiles never share a cell; a profile's credentials and cookies are only ever sent to its own cell.
- [x] Killing a cell mid-session leaves no readable profile anywhere: the cell's storage is gone, and the jar sealed is the last one read.
- [x] Signing out ends the site's session at the next launch.
- [x] A one-time code typed in the app is filled once, into the page that asked, and not kept.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates). The browser tests run on the pinned Chromium, in the sandbox and in CI, as WO-029's do.

## Evidence

Built in #187, #188, #194, #196, #198, #199 and #200 (the broker and the browser), and #195, #235 and #236 (the app). WO-031 then moved the fill out of the broker: the keeper sends a sign-in to the profile's browser on a channel of its own (#238 to #243).

An audit at close mapped each criterion to the tests behind it, and read the code beside them:
- It found a real bug, fixed in #246: signing a profile out didn't hold while a browser for it ran.
- It found criteria proven only in part; #247 adds those tests.

Each change was gated in the sandbox on real cells, with a delegated cgroup and the pinned Chromium:
- fmt and clippy on 1.98.1 and 1.85.0;
- preflight;
- the tests with `REQUIRE_CELL=1` and `REQUIRE_BROWSER=1`;
- a hand mutant for each claim, each read for failing at its intended check;
- then CI.

"Fake" below means the broker's tests with a scripted browser (`crates/hatchling-broker/src/browser_ops.rs`). "Real" means real Chromium in a real cell.

- **A credential for origin A is never filled on origin B.**
  - **A controller that reports A while the page is B:** `sign_in_filled` and `kept_sign_in_filled` (`crates/hatchling-browser/tests/real_cell.rs`, real). The controller checks the top frame's own `securityOrigin`, whatever it was told, and a fill for another origin or another port is refused (`crates/hatchling-browser/src/page.rs`).
  - **A redirect, a script that moves the page, a look-alike host, and a form framed from another site:** `kept_sign_in_follows_the_page` (real, #247). The script moves the page to `hatchling.test.other.test`. Each fill is refused `OtherOrigin`, and the form is checked untouched. A hand mutant comparing origins by prefix fails it with "filled on a look-alike host".
  - **Another profile's sign-in, or another site's:** refused by the keeper (`a_sign_in_is_filled_on_its_profiles_browser_channel_alone`, `crates/hatchling-broker/src/keeper.rs`) and by the broker (`a_sign_in_is_refused_off_its_origin_outside_a_profile_and_unless_its_named`, Fake).
- **After a full sign-in flow, no credential, cookie value or code is anywhere the agent or a log can see.**
  - **Fake:** `after_a_whole_signed_in_session_neither_the_agent_nor_the_log_holds_a_secret`. Through a sign-in, a code, a page that sets and shows a cookie, and clicks, it scans for the username, the password, the restored cookie, the page-set cookie and the code in three places:
    - every result the agent is given;
    - the broker's captured log;
    - the action log the owner's app is sent (#247).

    A hand mutant that tells the owner of an action before its result is hidden fails it with "the app was sent a secret".
  - **CI, real processes** (`scripts/browser_end_to_end.py`): the agent signs in with a canary sign-in the owner kept, and the site answers with a fresh session cookie, which its page shows (#247). The run checks:
    - the broker's memory holds neither half of the sign-in, after it was kept or after the fill;
    - its log and the model's requests hold none of the canaries;
    - the model was shown the cookie as `session=[hidden]`;
    - the app shows none of it.
- **A foreign cookie isn't restored; a service worker doesn't survive.**
  - **Foreign cookies:** a cookie is kept only for the profile's own domains, when the jar is kept and again when it's read back:
    - `a_jar_keeps_only_its_profiles_cookies_across_entries_and_replaces_the_last`, `a_planted_cookie_in_the_store_is_never_read_back` and `only_a_profiles_own_cookies_are_kept` (`browser_profiles.rs`);
    - `a_site_the_owner_signed_in_to_opens_in_its_profile_with_its_session` (Fake), where a page's cookie for another site isn't kept.

    A real page can't set a cookie for another domain. One can arrive only from another site's response, and that site loads only once the owner allows it, so these tests plant it in the jar directly.
  - **Service workers:** `a_kept_session_is_its_cookies_alone_and_a_service_worker_dies_with_its_browser` (`crates/hatchling-broker/tests/browser_cell.rs`, real). A page registers a service worker. The next browser is given the jar, and has the session and no worker.
- **A signed-in cell reaches nothing outside its profile without the owner.**
  - **The proxy:** `a_page_in_a_real_browser_reaches_only_the_sites_its_session_holds` (real). These are refused, with no name looked up:
    - a page's script reaching an image, a script and a `fetch` on another site, an internal address and a loopback port;
    - a WebRTC peer, which gets no UDP candidate;
    - a redirect to a site only it names (#247).
  - **A profile's cell is the same cell and proxy, holding the sites `open` grants it:** the profile's own, and nothing held for a visit before (`a_site_the_owner_signed_in_to_opens_in_its_profile_with_its_session`, Fake).
  - **The owner:** another site is put to the owner first, and the signed-in browser runs on meanwhile (`leaving_a_profile_keeps_its_session_once_the_next_site_is_allowed`, Fake).
- **Two profiles never share a cell; a profile's secrets go only to its own.**
  - **Switching:** `a_browser_moving_to_another_profile_is_another_browser_with_that_profiles_session_alone` (Fake, #247) checks four things:
    - the first browser ends with its session kept into its own profile;
    - a second starts for the other;
    - each is given its own profile's cookies alone;
    - the second holds none of the first's sites.

    Three hand mutants fail it: a switch that keeps nothing, one that carries the sites over, and one that counts only leaving a profile for none.
  - **Sign-ins:** the keeper fills a sign-in only on the channel of a browser started for its profile (`keeper.rs`, and `the_keeper_holds_the_sign_ins_and_hands_the_broker_no_key_to_them`, `crates/hatchling-broker/tests/keeper.rs`, a real keeper process).
- **Killing a cell mid-session leaves no readable profile.**
  - **Real:** `a_browser_cell_killed_mid_session_leaves_no_profile_on_the_host` (#247). A browser signs in, with a cookie and a service worker. Then every process in its group is killed at once through `cgroup.kill`. The checks:
    - no file of a profile (`Cookies`, `Preferences`, `Local State`, `Service Worker`, or the worker's script) is on the host while it runs, or after;
    - the cell answers nothing more;
    - its group is gone.

    The cell's only writable place is a tmpfs of its own (`a_browser_cell_sees_its_browser_fonts_and_scratch_and_nothing_else`, `crates/hatchling-broker/src/cell.rs`). A hand mutant that binds it from a host directory fails the test, finding Chromium's `Service Worker` directory on the host.
  - **The jar:** the jar sealed is the last one read (`a_session_killed_mid_page_has_its_last_read_jar_sealed`, Fake).
- **Signing out ends the site's session at the next launch.** `a_profile_signed_out_while_its_browser_runs_stays_signed_out_and_its_cookies_hidden` (Fake, #246) signs the profile out through the owner's op while its browser runs. It checks:
  - nothing is kept again, after an op or on close;
  - a cookie set after the sign-out is hidden from the agent;
  - the next browser restores nothing, and keeps a new sign-in.

  The owner's hand-back keeps nothing after a sign-out (`only_the_owner_keeps_a_sign_in_and_only_while_they_drive`, `api_ops.rs`). Five hand mutants each fail these tests.
- **A one-time code is filled once, into the page that asked, and not kept.**
  - `a_code_is_asked_of_the_owner_typed_once_given_and_never_shown` (Fake): the code is typed into the field asked for, not one named later, through the same exact-origin fill as a sign-in. A second give is refused, and nothing of it is left to hide.
  - `a_code_declined_or_expired_is_never_typed_and_one_off_https_or_unwatched_is_refused` (Fake).
  - The board's tests in `browser_board.rs`.
  - `sign_in_filled` (real): the controller types a code only into its field, on its origin.
  - The app keeps it in no state (`frontend/tests/unit/live-surfaces.test.mjs`).

**Docs:**
- the threat model: a signed-in site's row in `docs/arch/trust-boundary.md`;
- the configuration: `HATCHLING_BROWSER_PROFILES_DIR` and `HATCHLING_BROWSER_CREDENTIALS_DIR` in `docs/storage.md`;
- the local API: "Signed-in sites" in `docs/local-api.md`, its examples checked by `local_api_doc.rs`;
- the agent's ops: `docs/session-protocol.md` v16;
- the CHANGELOG.

**Left for later:** passkeys (out of scope above), and a code typed into a real page through the board end to end. That one is shown in parts: the board with a scripted browser, the controller with real Chromium.

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
