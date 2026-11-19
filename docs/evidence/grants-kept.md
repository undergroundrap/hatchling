# Evidence: standing site grants kept as keys and scopes, kept when chosen (WO-036 slice 1, #542)

What was tested, how, and what came back, for WO-036's first slice: the owner's
standing grants kept as a key and a scope, the owner's *Allow always* kept at
their decision, and a revoke reaching the session that chose it.

| | |
|---|---|
| Base commit | `92ee32ee` (`fix(broker): a file's note to the model outlives a run that failed (#541)`) |
| Tested tree | `b2951dae` |
| Date | 2026-11-19 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The threat

A standing yes must not become more than the owner gave, outlast their will, or
be kept other than as they chose. The design's review (private) found two ways
the sites the owner always allows already fell short:

- **#542: an always kept only when the session came back.** *Allow always* was
  written by the session that asked, when it claimed the approval, not by the
  owner's decision. If that session never read the site again, the owner's
  choice was lost, and Settings didn't show it. A failed write was only logged.
- **A revoke the chooser didn't feel.** The session that chose always also held
  the site for itself, so revoking it left that session reading it until it ended.

And what a grant must hold to be extended (a day scope, more kinds):

- **A key no one else can spell differently.** A target is a site's origin in its
  one canonical form (`WebUrl`'s): no other spelling, path or scheme makes a key,
  and a store holding one is refused.
- **A day that can't be stretched.** A clock set back must never extend a grant,
  across a restart included.
- **A check that writes nothing.** Only the owner's operations change the store;
  what the agent's reads cause is never a write.

## What changed

- **`site_grants.rs`** keeps grants as a kind and a target (`web.visit` and an
  origin) with a scope:
  - **day:** 24 hours by the broker's clock;
  - **always.**

  They are kept in one sealed entry of the sites store, with the latest time the
  owner's operations have seen.
  - **The clock:** a clock that says earlier than that time, or than the latest
    this broker saw for the store, makes every day grant cover nothing until it
    catches up.
  - **Tidying:** ended day grants are dropped by the owner's next operation,
    never by a check.
  - **Sites kept before:** the sites always allowed move in as always grants,
    made when they were allowed. That happens under the store's lock before
    anything else it does, and the old list is removed once they are written.
- **`approval_board::decide_as_owner`** is the owner's decision, used by the API
  and the tests alike. It decides on the board, then keeps a site's always as a
  grant.
- **The session that claims an always** reads the page the owner was shown. The
  grant covers the rest, so a revoke reaches it. Only *Allow for task* holds the
  site for the session.
- The day scope can't be chosen in the app yet: that is slice 2 (local UI API v9).
- `docs/local-api.md`, the work order (in progress, its scope as agreed on
  2026-11-19), the queue and the changelog say so.

## Results

| Check | Result |
|---|---|
| `site_grants`, `web_ops`, `browser_ops`, `approval_board`, `api_ops` tests | 165 passed on 1.98.1 and on 1.85.0 |
| Hand-breaks | 15 broken, 15 caught (below) |
| Targeted mutation (`--in-diff`) | 88 mutants: 78 caught, 0 missed, 0 timeouts, 10 unviable (each a `Default` the type has none of) |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | 0 on 1.85.0 and on 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,226 passed, 0 failed, 0 ignored, on each toolchain |
| `seed-check`, `protocol-schema`, `audit` | 0 on each toolchain; "all checks passed" |

### Hand-breaks

| Break | Caught by |
|---|---|
| A key made from any spelling of a site | `a_key_is_a_site_in_its_one_form`, `a_site_is_allowed_listed_and_revoked`, `the_stores_parts` |
| A day grant with no end | `a_day_grant_covers_for_a_day`, `checking_writes_nothing_and_the_owner_tidies` |
| A day grant covering before it was made | `a_day_grant_covers_for_a_day` |
| A clock set back stretching a day grant | `a_clock_set_back_stretches_no_day_grant` |
| The store's latest time never kept | that, and `checking_writes_nothing_and_the_owner_tidies` |
| A check that writes | `checking_writes_nothing_and_the_owner_tidies` |
| A day grant replacing an always grant | `granted_again_a_day_starts_again_and_always_stays` |
| One past the cap kept | `the_grants_are_bounded_and_none_in_force_dropped` |
| Tidying dropping always grants | `the_grants_are_bounded_and_none_in_force_dropped` |
| A store with a target no site is, read as sound | `the_stores_parts` |
| The old list left after moving in | `the_sites_allowed_before_move_in_once` |
| A site moved in twice | `the_sites_allowed_before_move_in_once` |
| The owner's decision keeping nothing (#542) | `an_always_is_kept_when_the_owner_chooses_it`, `always_is_kept_for_every_session_until_revoked` |
| Less than always kept as always | `the_owners_always_is_kept_when_decided`, `an_always_is_kept_when_the_owner_chooses_it` |
| An always claim holding the whole site past a revoke | `always_is_kept_for_every_session_until_revoked` |

**Found by these runs:**
- **Two breaks survived the first run, both test gaps.**
  - A day grant covering before it was made was masked by the clock check. A store
    written with its latest time behind a grant now reaches the guard.
  - An always claim holding the site was reached only after a revoke, when the
    session comes back for the very page it was shown. The test now does that.
- **The first mutation run missed four mutants:**
  - the day's length (`24*60*60`), which the tests named only by its constant: now
    pinned to 86,400;
  - a count read only by a log line: removed;
  - a revoke of a grant whose day had ended, said found when another grant was in
    force: now tested.

## Residual, for the next slices

- **A session you allowed a site for its task** still keeps it past a revoke, as
  the browser's own once-a-visit sites and its proxy do. Slice 1b makes a revoke
  reach every one of them, with a record of what each grant covered.
- **A restore** brings back grants revoked since that backup.
- **A clock that lags after a host's sleep** (WSL) can still extend a day grant by
  the lag.
