# Evidence: the warning rule, a profile's card said truly, and WO-036's close (WO-036 slice 3)

What was tested, how, and what came back, for WO-036's last slice:
- a request that carries a warning asks every time, whatever is granted (the
  work order's third criterion);
- a signed-in profile's card offers what its yes is;
- the work order's close, each criterion against the evidence that proves it.

| | |
|---|---|
| Base commit | `d428cff9` (`feat(broker): keep what standing grants covered, and show it in Activity (#547)`) |
| Tested tree | `f824387c` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit, and the work order's
close in one after it. Neither changes code.

## The threat

- **A yes too broad for what it covers.** A standing grant covers a site's
  pages. Some requests are risky enough that no standing yes should cover
  them, and the agent must not be able to decide which those are.
- **A long address is the easiest way out.** With a site allowed always, the
  agent can put what it read into the address it asks for, and be let through
  without the owner seeing it. Slice 2c records it; a warning asks first.
- **A card that says less than its yes does.** A signed-in profile's card
  offered *Allow once*, but its yes lasts the task. The owner was shown a
  smaller yes than they gave.

## What changed

- **`warnings.rs`:** the broker's list of warnings, a constant of its build.
  - A warning is found from the request the broker resolved alone: the address
    it parsed. A session, its words, a grant and the operator's config are
    never inputs, so nothing an agent sends adds one or takes one away.
  - Today the list has one entry, the maintainer's choice of 2026-11-20: a
    site's page whose address is over 1,024 characters.
- **Checked before any grant** (`web_ops::allow`). A request with a warning
  isn't covered by any of these:
  - a site held for the task;
  - a 24-hour or always grant;
  - in the browser, the same visit, or a signed-in profile.

  Instead the owner is asked. The card says the warning, the broker's field
  `Warning`, and offers *Allow once* alone. A yes is used once, and the same
  request asks again.
- **A signed-in profile's card offers for the task alone,** since that's how
  long its yes lasts. An act signed in is still offered once.
- `docs/local-api.md` and the changelog say so.
- **The close:** every criterion is ticked against its slice's record, the work
  order is closed, and its queue row matches.

## Results

| Check | Result |
|---|---|
| `warnings`, `web_ops`, `browser_ops`, `approval_board` and the slice's other modules | core 224 and broker 285 passed, on 1.98.1 and on 1.85.0 |
| Hand-breaks | 9 broken, 9 caught, each by the tests named for it (below) |
| Targeted mutation (`--in-diff`) | 26 mutants: 18 caught, 0 missed, 0 timeouts, 8 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,258 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |

### Hand-breaks

| Break | Caught by |
|---|---|
| A site held for the task covering a warned request | `a_long_address_asks_every_time_whatever_is_granted` |
| A standing grant covering a warned request | the same |
| A warned request offered for the task, a day or always | the same |
| The card never saying the warning | the same |
| The card's warning not the broker's | the same |
| The same visit covering a warned page | `a_long_address_is_asked_for_on_the_same_visit` |
| An address of exactly 1,024 characters warned | `a_long_address_carries_the_warning` |
| No warning ever found | that, and both tests above |
| A profile's card offering once | `a_profiles_card_offers_the_task_alone` |

## WO-036, criterion by criterion

| Criterion | Proven by |
|---|---|
| Every card shows the grant key it would make and offers only its kind's scopes | [grants-v9.md](grants-v9.md); this record (a profile's for the task, a warned request's once) |
| A covered request runs without a card and is in Activity with its grant | [covered.md](covered.md) |
| A warned request asks every time; checked before any grant; the broker's list; an agent can't change it | this record |
| Grants listed with scope and age, revoked alone, a revoke reaching running tasks | [revokes-reach.md](revokes-reach.md), [grants-v9.md](grants-v9.md) |
| A 24-hour grant ends at 24 hours by the broker's clock, a clock set back never stretching it | [grants-kept.md](grants-kept.md), [day-ends.md](day-ends.md), [grants-v9.md](grants-v9.md) |
| Sites kept as always are kept as always grants | [grants-kept.md](grants-kept.md) |
| Each guard hand-broken, its test failing for the right reason | each record's hand-breaks |

## Residual

- **Commands:** the list of commands that carry a warning, and the rule for a
  pipeline, come with the first tool that asks to run a command, as agreed on
  2026-11-19. This slice's rule is what they plug into.
- **A restore** brings back grants revoked since that backup. A notice after a
  restore is a follow-up.
- **A WSL clock that lags after the host's sleep** can stretch a 24-hour grant
  by the lag (2a's residual).
- **Browser pages after the first** pass through the proxy, which sees only
  their sites. A long address the page itself makes is never seen, so it isn't
  warned. A page the agent asks to open is.
