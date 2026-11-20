# Evidence: approvals bound to grant keys and scopes, the local UI API v9 (WO-036 slice 2b)

What was tested, how, and what came back, for WO-036's slice 2b: every
approval card names the grant a standing yes would make and offers only the
scopes its kind allows; the owner can allow a site for 24 hours; and the
owner's standing grants are listed with their scope and age and revoked by
key.

| | |
|---|---|
| Base commit | `fbb594d5` (`feat(broker): a day grant's end reaches every running task (#545)`) |
| Tested tree | `00cc4aff` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user; the app on Windows 11 |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup); Node (the app's `npm run check`) |

This record was added after the runs, in its own commit. It changes no code.

The hand-breaks and the mutation run were on tree `f1e09883`. The tested tree
differs from it only by one line of `docs/arch/protocol.md` (the API's version,
v8 to v9), which the gate's audit found. The gate, the unit runs and the app's
check were on the tested tree.

The maintainer approved the protocol change (UI schema 8 to 9, and the app's
`src/protocol/models.ts`, re-hashed in `PROTOCOL-LOCK.json`) for WO-036 slice 2
on 2026-11-19.

## The threat

A yes must cover no more than the owner chose, and the owner must see what a
standing yes keeps. Before this slice:

- **What a card offered travelled twice.** The proposal carried
  `session` and `always`, and the card was built from the same arguments, not
  from what the ledger kept and checks. Nothing tied the scopes a card showed
  to the ones a decision was checked against.
- **The owner never saw the grant** a standing yes would make, only "Allow
  always".
- **A standing yes that couldn't be kept** (a full store, or one the broker
  couldn't open) was only logged: the app still showed it as kept.
- **Grants were listed as sites**, with no scope, end or state, and revoked by
  a site string.

## What changed

- **One list of scopes, the ledger's.** A proposal says the scopes it may be
  approved for and the grant key it would make. The ledger keeps both, every
  card is built from what the ledger kept, and a decision is accepted only for
  a scope in that list (`Decision::scope`). A denial is always the owner's.
- **Which scopes, by kind:**
  - a site offers once and for this task, and for 24 hours and always where
    the broker keeps grants and the site has a key;
  - every other kind offers once.
- **The grant key is the broker's**, made from the site it resolved
  (`web.visit:https://a.example`), never from the agent's page or words.
- **24 hours can be chosen** (`approve_day`). It's kept as a day grant the
  moment the owner decides, as always is.
- **The answer says whether a standing yes was kept** (`decided.kept`). The
  app tells the owner when it wasn't, and only the request shown is covered.
- **`manage_grants`** lists every grant not ended, with:
  - its key, kind and target;
  - its scope;
  - when it was made, and when a day grant ends;
  - its state: `covering`, or `clock_behind` for a day grant the clock check
    holds back, which is listed so it can still be revoked.

  It revokes by key, as `Display` writes one. Any other spelling, kind or
  target is `not_found`, and nothing is revoked. A revoke still reaches every
  running task (1b).
- **Activity lines** kept before v9 name the request `manage_sites`, and
  still read.
- **The app:**
  - the card offers *Allow for 24 hours* in the existing row and names the
    grant key;
  - Permissions shows each grant's scope and age, and revokes it by key;
  - the demo's card offers the day too.
- `docs/local-api.md`, `docs/arch/distribution.md` and the changelog say so.
  The changelog line names the break for API scripts.

## Results

| Check | Result |
|---|---|
| `hatchling-core` lib tests; the broker's `site_grants`, `web_ops`, `browser_ops`, `approval_board`, `approvals`, `api_ops`, `api`, `grant_reach`, `browser_proxy`, `decisions` tests | core 223 and broker 268 passed, on 1.98.1 and on 1.85.0 |
| Hand-breaks | 12 broken, 12 caught, each by the tests named for it (below) |
| Targeted mutation (`--in-diff`, core and broker) | 40 mutants: 20 caught, 0 missed, 0 timeouts, 20 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,245 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |
| `npm run check` (typecheck, lint, design, protocol, the lock, tests, coverage) | 872 passed, 0 failed; coverage 100% |

### Hand-breaks

| Break | Caught by |
|---|---|
| Any scope accepted, whatever the card offered | `a_scope_is_offered_only_where_the_request_says_so`, `always_is_not_offered_without_a_store` |
| A denial refused unless offered as a scope | `a_scope_is_offered_only_where_the_request_says_so` |
| The card offering its own scopes, not the ledger's | `a_sites_confirmation_shows_the_site_then_the_agents_words` |
| A day and always offered where no grants are kept | `always_is_not_offered_without_a_store` |
| The key made from the agent's page | `always_is_kept_for_every_session_until_revoked` |
| A day kept as always | `the_owners_always_is_kept_when_decided`, `a_day_is_kept_when_the_owner_chooses_it` |
| A day kept as nothing | the same, and `a_standing_yes_says_whether_it_was_kept` |
| Whether a standing yes was kept never said | `a_standing_yes_says_whether_it_was_kept` |
| A grant that couldn't be kept said kept | the same |
| A key of any kind read as a site's | `a_key_is_read_only_as_it_is_written` |
| Day grants listed after they ended | `the_owners_operations` |
| A day grant held back by the clock listed as covering | the same |

**Found by these runs:** the first mutation run missed two mutants, both test
gaps, now closed:
- **The list's check for an always grant**, turned the other way: a test
  counted the grants listed past a day without saying which one stayed. It
  now does.
- **A decision's scope** read as none: the core crate's own tests never said
  each decision's scope, and a mutation of the core runs only its own tests.
  They do now.

## Residual, for the next slices

- **Covered actions in Activity** (a request a grant covered, shown with the
  grant that covered it) are slice 2c, with their own bounded log.
- **A signed-in profile's card** still offers once only, and its yes lasts the
  task. Saying so truthfully (`[task]`) is slice 3.
- **The `kept` answer** is said by the broker and shown by the app. It's
  tested at the owner's decision (`decide_as_owner`) and in the app. The line
  that copies it into the API's answer is tested only through the app's
  handling of it.
