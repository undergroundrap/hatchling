# Evidence: a revoke reaches every running task (WO-036 slice 1b)

What was tested, how, and what came back, for WO-036's slice 1b: the owner's
revoke of a site reaching everything a running task holds it by.

| | |
|---|---|
| Base commit | `276ba7be` (`feat(broker): keep standing site grants as keys and scopes, kept when chosen (#543)`) |
| Tested tree | `4a435da8` |
| Date | 2026-11-19 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The threat

A revoked grant must cover nothing from then on, a running task included
(the work order's fourth criterion). Before this slice, the revoke reached
the store and the next session to look, but a running task could keep using
a site the owner had revoked by any of these:

- **A site allowed for the task.** A session that held a site for its task
  kept it until the task ended.
- **An approval not yet used.** A request the owner approved that no session
  had claimed was claimed later, after the revoke, and read the site.
- **The browser's proxy.** It kept the site, and every tunnel open to it,
  until the browser went to another site.
- **The browser's once-a-visit sites.** A site allowed once for the visit
  wasn't asked about again on the same visit.
- **An already-loaded page.** Reading and acting on the page went on, as no
  op but opening a page checks its site.

And a yes the owner gives after revoking a site must stand: a revoke that
reached it would ask the owner again for what they had just allowed.

## What changed

- **`grant_reach.rs`** keeps a part for each running session, by the store
  of grants it was allowed by. The owner's revoke, after the store has let
  the site go:
  - **withdraws every approval to read the site that no session has
    claimed**, of any task (`Approvals::withdraw_approved`);
  - **takes the site from each browser's proxy at once**, closing its
    tunnels both ways, except a signed-in profile's sites, which the owner
    keeps with the profile;
  - **tells each session**, which lets the site go before it next decides
    anything.
- **Order.** Each revoke is numbered from one counter. A hold records the
  number it saw before what allowed it was read, and a revoke reaches only
  a hold that saw an earlier number. A yes after a revoke stands; a revoke
  racing a hold being made reaches it, at the latest when the op ends.
- **The web session** holds each site allowed for its task with the number
  it saw, and lets go of each revoked one before it checks a site.
- **The browser session** records the number each site a yes gave it saw.
  Before each op, and as each op ends, it lets go of every revoked site: its
  tunnels, its once-a-visit hold, and, if it's the site the browser is on,
  the browser itself, closed, so nothing more is done on a page loaded before
  the revoke. The agent is told why, and opening the site again asks the
  owner.
- **What a session was doing as the owner revoked finishes**: a read in
  flight is read, as a CONNECT proxy can't take back bytes it passed.
- The app's revoke dialog, `docs/local-api.md` and the changelog say so.

## Results

| Check | Result |
|---|---|
| `site_grants`, `web_ops`, `browser_ops`, `grant_reach`, `approvals`, `approval_board`, `api_ops` tests | 191 passed on 1.98.1 |
| Hand-breaks | 17 broken, 17 caught, each by the tests named for it, on its own assertion (below) |
| Targeted mutation (`--in-diff`) | 61 mutants: 52 caught, 0 missed, 0 timeouts, 9 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,235 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |
| `npm run check` (the app: the revoke dialog's words), on Windows | 870 passed, 0 failed, coverage 100% |

### Hand-breaks

| Break | Caught by |
|---|---|
| The owner's revoke reaches the store alone | `a_revoke_reaches_a_site_held_for_the_task`, `a_revoke_reaches_a_running_browser`, `a_revoke_withdraws_approvals_not_yet_used_and_a_yes_after_it_stands`, and two more |
| Approvals not yet used kept past a revoke | `a_revoke_withdraws_approvals_not_yet_used_and_a_yes_after_it_stands`, `always_is_kept_for_every_session_until_revoked` |
| Only approvals still waiting withdrawn | the same two |
| A revoke numbered with the number already seen | `every_session_of_the_store_is_told`, `a_revoke_reaches_only_a_hold_allowed_before_it`, `a_revoke_reaches_only_what_a_yes_before_it_gave`, and three more |
| A hold that saw the revoke reached too | `a_revoke_reaches_only_what_was_allowed_before_it`, `a_revoke_reaches_only_a_hold_allowed_before_it` |
| The proxy waiting for the session's next op | `a_proxy_loses_the_site_at_once_but_not_a_profiles`, `a_revoke_reaches_a_running_browser` |
| A profile's sites taken at the proxy | `a_proxy_loses_the_site_at_once_but_not_a_profiles` |
| Another store's sessions told | `every_session_of_the_store_is_told`, `a_revoke_reaches_a_running_browser` |
| A spelling no site has taken as a revoke | `every_session_of_the_store_is_told` |
| The web session never taking a revoke | `a_revoke_reaches_a_site_held_for_the_task`, `a_revoke_reaches_only_a_hold_allowed_before_it`, `a_revoke_reaches_a_running_browser` |
| The web session letting go of a later yes | `a_revoke_reaches_only_a_hold_allowed_before_it` |
| The browser never taking a revoke | `a_revoke_reaches_a_running_browser`, `a_revoke_while_an_op_runs_reaches_the_browser_as_it_ends`, `a_revoke_reaches_only_what_a_yes_before_it_gave` |
| A browser on the revoked site's page going on | `a_revoke_reaches_a_running_browser`, `a_revoke_while_an_op_runs_reaches_the_browser_as_it_ends` |
| A profile's sites taken by the browser | `a_revoke_reaches_only_what_a_yes_before_it_gave` |
| The browser letting go of a later yes | `a_revoke_reaches_only_what_a_yes_before_it_gave` |
| A revoke made during an op waiting for the next op | `a_revoke_while_an_op_runs_reaches_the_browser_as_it_ends` |
| The browser's proxy never watched | `a_revoke_reaches_a_running_browser` |

**Found by these runs:** the first mutation run missed three mutants, all
test gaps, now closed:
- **The withdraw's filter** (`&&` to `||`) would have withdrawn every
  approved request, and the requests still waiting for this site. A test now
  shows another site's approval and an unanswered request survive a revoke.
- **Opening a page's site as a page of its own** (`&&` to `||` in `open`)
  would have skipped asking the owner. A site allowed once for the visit's
  pages is now shown to be put to the owner when opened.
- **Dropped sessions' parts** (`>` to `>=` in `join`) would have been kept
  for ever. A test now shows only running sessions are kept.

## Residual, for the next slices

- **A signed-in profile's sites** aren't reached by a site's revoke: they are
  kept with the profile, which a session holds for its task (slice 3).
- **A restore** brings back grants revoked since that backup.
- **A clock that lags after a host's sleep** (WSL) can still extend a day
  grant by the lag.
- **A revoke racing a yes** for the same site, given within the same instant,
  can take the browser's tunnels from the later yes. The browser asks for the
  site again, so this fails closed.
