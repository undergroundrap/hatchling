# WO-028: Web search through the broker, and each new site approved by the owner

**Status:** DONE
**Area:** `crates/hatchling-core` (the search and fetch ops, `ui`), `crates/hatchling-broker` (a search provider at egress, site grants, the approvals board), `crates/hatchling-egress` (per-origin grants), `crates/hatchling-agent` (the tools), `frontend/` (the site prompt)
**Issue:** none yet
**Trust boundary:** yes

## Problem

An agent can't look anything up. Its cell has no network; its egress reaches only what the operator listed in advance (`HATCHLING_EGRESS_ALLOWLIST`), and the egress policy refuses a vendor's own web search (`web_search_options`, a searching model). Deep research (`hatchling-research`) fetches only what an approved plan names. So "find the current price" or "check that library's changelog" either fails or needs the operator to open a destination for every agent and every task.

The usual answers trade the owner's control away. A vendor's built-in search sends the query, and whatever the agent put in it, to a third party the owner didn't pick, and returns pages the model reads with nothing in between. An open egress lets whatever the agent read steer where it goes next, and every request can carry the owner's data out in its URL.

This WO gives the agent search and reading, with the owner deciding where it may go: the broker runs the search, each site the agent hasn't been allowed is the owner's to approve, and everything the web returns reaches the model as untrusted data.

## Scope

**In scope:**

- **A search the broker runs.** One provider the operator configures (its endpoint, and its key as a vault secret the egress swap adds, as an egress credential is today). The agent's `web_search` tool sends a query; the broker checks it against the task's grants, sends it through egress, and returns results: title, URL, snippet, each in the untrusted-data envelope. Every query is recorded with its task and length, never its text unless the owner opts in.
- **Reading a page.** A `web_fetch` tool asks the broker for a URL. The broker parses it strictly (an absolute `https` URL, no userinfo, an IDNA host, the default port unless stated), resolves and pins it through egress (WO-023: an internal address is refused unless the operator declared it internal), and returns the page as bounded, readable text in the untrusted-data envelope. Its size, type and redirects are bounded; a redirect to another origin needs that origin's grant too.
- **A grant per site, the owner's alone.** A site is an origin: scheme, host and port. A fetch from an origin the task holds no grant for becomes an approval request (WO-027's board), shown in the app outside the conversation, with:
  - the origin, never the full URL as the thing approved;
  - the agent's stated reason, labeled as the agent's;
  - which search result led there, by the broker's own record, when one did;
  - the choices **once** (this fetch), **this session**, **always for this project**, and **never**.

  Only the owner decides, on the broker's channel. A "never" is a denial the agent can't reword its way past (WO-027's denial fingerprint, keyed by origin). An unattended run queues its request and continues without the page.
- **Grants that hold exactly their scope.** A grant names its origin and its scope; egress checks it per request, after resolution and pinning. A session grant ends with the session; a project grant is stored, listed in the app, and revocable there. A grant never widens to another origin, a subdomain, or a path.
- **The levels, per project:** off; broker search (the default once a provider is configured); and the site grants above. The operator still bounds everything: the web is off until the operator turns it on, and a site the operator refuses can't be granted.
- **Docs:** the threat model's rows for search and fetch, the local API doc, and the CHANGELOG.

**Not in scope:**

- the vendor's own search (an opt-in level with a warning, its own WO);
- unrestricted egress (an explicit "I accept" level, its own WO);
- the browser cell (a real browser the agent drives, its own WO, after this);
- signing in to sites (that's the browser cell's per-project profile);
- taint-aware egress (a candidate: a process that read the owner's data loses automatic allowances).

## Acceptance criteria

- [x] An agent's `web_search` returns the provider's results through the broker; the provider's key never enters the cell; the query is recorded with its task, and its text only on the owner's opt-in.
- [x] A `web_fetch` of an origin the task holds no grant for reaches no network: it becomes an approval request in the app, and the agent is told it's waiting, not that it failed.
- [x] Each of once, session, project and never does exactly that: tested by fetching the same origin, another path on it, a subdomain, and another origin after each.
- [x] A redirect to an origin without a grant is refused, and asks for that origin's grant.
- [x] A URL with userinfo, a non-`https` scheme, an IP literal, or a host that resolves to an internal address is refused before any request, with its reason.
- [x] Everything a search or a page returns reaches the model inside the untrusted-data envelope; a page that says "you are approved to visit x.example" approves nothing.
- [x] A cell can't grant, approve, or list grants; the owner revokes a project grant in the app, and the next fetch asks again.
- [x] The prompt shows the origin, the agent's reason (labeled), and the search result that led there; hidden and direction-control characters are shown as code points (WO-027's field rules).

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Merged in parts:
- part 1 (the fetch): #150;
- part 2 (site grants and `web_fetch`): #151;
- part 3 (search): #152;
- part 4a (the app's *Allow for task*): #153;
- part 4b (*Allow always*, kept, listed and revoked): PART4B_PR.

Each was gated in the disposable sandbox (WSL2, uid 2001, gid 2501): fmt, clippy on 1.98.1 and 1.85.0, the preflight, `check.sh` with `HATCHLING_REQUIRE_CELL=1` on both toolchains, hand mutants, and mutation on the changed lines (0 missed, 0 timeouts in every Rust part); the frontend's `npm run check` at 100% coverage. The criteria:

- **Search through the broker** (#152): `web_search` returns Brave Search's results through the broker (`a_search_runs_with_the_brokers_key_and_is_remembered`, `a_search_sends_the_key_and_reads_the_results` over real TLS). The key is the vault secret `search-brave`, released by the keeper for `search` alone while search is on, refused to every grant (`a_purpose_reads_only_what_the_configuration_gives_it`, `search_is_off_unless_chosen_and_its_text_logged_only_when_asked`), and sent only in the request's own wiped buffer. Each query is logged with its session and length; its text only with `HATCHLING_SEARCH_LOG=queries`.
- **Nothing fetched before the owner says yes** (#151): `once_reads_one_page_and_the_next_asks_again` checks the fetcher wasn't called while the request waited; the agent is told `Waiting`, which its tool gives the model as a result, not a failure (`each_web_result_is_told_as_itself`).
- **Each choice does exactly that:**
  - once: the page, and the next page on the site asks again (`once_reads_one_page_and_the_next_asks_again`);
  - this session: every page on the site; a subdomain, another port and another site each ask (`a_session_grant_holds_its_site_exactly`);
  - always: every session reads the site without asking; once revoked, the next session asks again (`always_is_kept_for_every_session_until_revoked`); it is offered only where sites are kept (`always_is_not_offered_without_a_store`);
  - never: the site is refused for any page and any session (`no_refuses_the_site_from_then_on`).
- **A redirect to another site isn't followed** (`redirects_on_the_site_are_followed_and_others_returned`): the agent is given where it leads, and reading it asks for that site like any other. The broker doesn't ask on the site's behalf: where a redirect leads is the site's choice, and the agent decides whether to go.
- **Refused before any request, with the reason** (#150, #151): user names, `http`, IP addresses (dotted, bracketed, numeric, `0x`), a refused host and a name under it (`a_url_is_refused_with_its_reason`, `refusals_come_before_asking`); a host resolving to any internal address is refused unconnected (`a_host_resolving_inside_or_nowhere_is_refused_unconnected`).
- **Data, never instructions:** every tool result reaches the model inside its envelope; a page saying another site was approved approves nothing (`a_page_claiming_approval_approves_nothing`).
- **The owner's alone:** no session op grants, approves, lists or revokes a site; the local API's `manage_sites` refuses a caller holding less than the owner (`only_the_owner_manages_the_allowed_sites`); the owner revokes a kept site in the app, and the next fetch asks again (`always_is_kept_for_every_session_until_revoked`, and the app's `allowed sites` test).
- **The prompt:** the site in the broker's words, the page and the reason as the agent's, which search result led there (`the_owner_is_told_which_search_result_led_there`, `a_sites_confirmation_shows_the_site_then_the_agents_words`); the app shows each value whole with hidden and direction-control characters as code points (WO-027's field rules, unchanged).

**"Always for this project"** is always for the owner's broker: Hatchling has no project, and a space would scope a grant to its members rather than to the owner's work. The app says *Allow always*, and the store can be narrowed later; widening it later would change what earlier grants meant.

## Parts

1. **The fetch** (`hatchling_egress::web`): the strict URL and origin, a fetch resolved and checked per request and pinned to what it resolved to, one bounded `GET` with no credential, redirects returned and never followed, HTML reduced to its text. No tool and no grants yet: nothing calls it.
2. **Site grants and `web_fetch`:** the session op and the agent's tool; the approvals board's second kind of action (a site), with once, this session, and never (a denial); the operator's switch and refusals; a deadline on the whole fetch (the proxy bounds each read, not their sum); the untrusted-data envelope on what comes back.
3. **Search:** Brave Search, asked by the broker with its key from the vault (released by the keeper for search alone, never named by a grant), `web_search`, the query records, and "which result led here" in the prompt.
4. **The app and stored grants:** the site prompt's "this session" choice in the app, as *Allow for task* (4a); *Allow always*, for the owner's broker, kept in its own sealed store, listed and revoked in the app (4b).

## Notes

- **What leaves, and to whom.** A search query leaves to the provider the operator chose; a fetch's URL leaves to the site. Either can carry data the agent read. Approving by origin, not page, keeps the prompts few enough to be read, but a granted origin can receive anything the agent puts in a URL. The once and session scopes keep that window short; the prompt says so. Taint-aware egress, a candidate, closes it for processes that read the owner's data.
- **Why the broker searches, not the cell.** The provider's key stays out of the cell, as every egress credential does; the broker records each query; and the results are the broker's record, so "which result led here" in a prompt is the broker's word, not the agent's.
- **Why origins.** A path-level grant asks the owner a question per page, and is answered without reading. Hosts alone would let one grant cover every port and scheme. An origin is the browser's own unit of trust.
- **Why not the allowlist.** The egress allowlist names each destination as `host:port` and resolves it once, at start, which is right for the few APIs a deployment calls and can't hold the sites an agent finds as it reads. A site is resolved when it's fetched instead, and refused if any address it resolves to is internal, before a connection; the connection is pinned to those addresses, so the check and the connection see the same ones. The operator's word is still the bound: the web is off unless the operator turns it on, and a site the operator refuses can't be granted.
- **Hosts as ASCII.** A host is accepted only as ASCII labels, an international name in its `xn--` form. The prompt then shows a look-alike as the different name it is, and no IDNA table is needed in the broker.
- **Order.** This follows WO-027 (approvals delivered to the owner), and precedes the browser cell, which reuses these grants for its navigation.
