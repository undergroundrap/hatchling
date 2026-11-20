# Evidence: a day grant's end reaches every running task (WO-036 slice 2a)

What was tested, how, and what came back, for WO-036's slice 2a: a 24-hour
grant stopping, at its end, everything a running task holds by it.

| | |
|---|---|
| Base commit | `528cea48` (`feat(broker): a revoke reaches every running task (#544)`) |
| Tested tree | `03ebcc61` |
| Date | 2026-11-19 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The threat

A 24-hour grant must stop covering anything 24 hours after it was made, by
the broker's clock (the work order's fifth criterion), a running task
included. The design's second review (private) found that it didn't for the
agent's browser:

- **The proxy kept the site** a day grant gave it, and every tunnel open to
  it, until the browser went to another site.
- **The same visit was never asked again:** a page of the site the browser
  was on skipped the check, so a visit opened just before the end could go on
  as long as the session did.

A page read checks the store each time, so it already stopped at the end. No
card offers the day scope yet: that is slice 2b, and this slice comes first so
a day is never offered before it ends everywhere.

## What changed

- **`site_grants`** says how long the grant covering a site covers:
  `Cover::Always`, or `Cover::Until` its day's end.
- **`web_ops::allow`** says what allowed a site (`Yes`): the revoke number it
  saw and, for a day grant, its end by two clocks (`End`), whichever comes
  first. The monotonic clock can't be set back, so a clock set back never
  stretches it. The broker's own clock goes on while the host sleeps, which
  the monotonic one doesn't count.
- **The proxy's sites** (`Sites::grant_until`) hold a day grant's site to its
  end. From then the proxy refuses it, and registers no tunnel to it. As the
  end comes, a timer revokes it and closes its tunnels both ways. The timer
  looks at the broker's clock at least once a minute, so a host that slept is
  seen within a minute of waking. A site
  granted again without an end has none, and a timer whose end was replaced
  does nothing. The same grant read again keeps its end and its one timer.
- **The browser session** keeps each day-held site's end. Before and after
  every op it lets go of each one that has ended. A browser on its page is
  closed, and the agent is told the grant ended; opening the site again asks
  the owner.

## Results

| Check | Result |
|---|---|
| `site_grants`, `web_ops`, `browser_ops`, `browser_proxy`, `grant_reach`, `approvals`, `approval_board`, `api_ops` tests | 213 passed on 1.98.1 and on 1.85.0 |
| Hand-breaks | 17 broken, 17 caught, each by the tests named for it |
| Targeted mutation (`--in-diff`) | 59 mutants: 49 caught, 0 missed, 0 timeouts, 10 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,240 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |

### Hand-breaks

| Break | Caught by |
|---|---|
| A day grant said to cover always | `a_grant_says_how_long_it_covers`, `a_day_grants_end_is_said_by_the_monotonic_clock`, `a_day_grants_site_is_held_to_its_end` |
| The web session never saying a day grant's end | `a_day_grants_end_is_said_by_the_monotonic_clock`, `a_day_grants_site_is_held_to_its_end` |
| The browser holding a day grant's site with no end | `a_day_grants_site_is_held_to_its_end` |
| An ended site never let go by the browser | the same |
| The same visit again granting the site anew, with no end | the same |
| The proxy admitting a site past its end | `a_sites_end_is_kept_until_it_is_granted_again` |
| No timer, so the end closing nothing open | `a_site_granted_until_a_time_is_let_go_then` |
| An old timer revoking a site granted again since | `a_sites_end_is_kept_until_it_is_granted_again` |
| Granted again without an end, keeping the old one | the same |
| The same grant read again replacing its end and timer | the same |
| A revoke leaving the site's end behind | the same |
| Sealing leaving the ends behind | the same |
| A tunnel registered past its site's end | the same |
| The broker's clock never consulted (a host that slept) | `a_sites_end_is_kept_until_it_is_granted_again`, `a_day_grants_site_is_held_to_its_end` |
| The monotonic clock never consulted (a clock set back) | `a_sites_end_is_kept_until_it_is_granted_again`, `a_site_granted_until_a_time_is_let_go_then` |
| A day grant's end never said by the broker's clock | `a_day_grants_end_is_said_by_the_monotonic_clock` |
| The broker's clock read as no time at all | `a_sites_end_is_kept_until_it_is_granted_again` |

**Found by these runs:** the first mutation run missed four mutants:
- **A branch that couldn't matter.** The open re-granted a site the browser
  already held, so nothing could tell it apart. It's gone: the browser holds
  a site only by a fresh yes, or as it already held it.
- **The broker's clock** read as a constant (three mutants). A test now pins
  it to the broker's own clock.

## Residual, for the next slices

- **A tunnel open as the host sleeps** past a grant's end is closed within a
  minute of waking (the timer's look at the broker's clock), or at the
  session's next op, whichever is first. Nothing new is admitted after the end.
- **A WSL clock that lags after the host's sleep** reads earlier than it
  should until it catches up, and so can stretch a day grant by the lag. The
  monotonic clock doesn't help there, as it didn't count the sleep either.
- **The day scope isn't offered yet:** slice 2b.
