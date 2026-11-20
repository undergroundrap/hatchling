# Evidence: what the standing grants covered, kept and shown in Activity (WO-036 slice 2c)

What was tested, how, and what came back, for WO-036's slice 2c: a request a
standing grant lets through without asking is kept, with the address the
agent asked for and the grant that covered it, and the owner sees it in
Activity (the work order's second criterion).

| | |
|---|---|
| Base commit | `5eadf09b` (`feat(approvals): bind approvals to grant keys and scopes, local UI API v9 (#546)`) |
| Tested tree | `8ac5fdc8` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user; the app on Windows 11 |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup); Node (the app's `npm run check`) |

This record was added after the runs, in its own commit. It changes no code.
`list_covered` is added to the local UI API v9, which is unreleased. The
maintainer approved v9 for WO-036 slice 2 on 2026-11-19.

## The threat

A standing site grant is a standing outbound channel: the agent can put
whatever it holds in the address it asks for, and with a grant the owner is
never asked. Before this slice nothing recorded what a grant let through, so
the owner couldn't see it. The design's review (private) set out what the
record must withstand:

- **A looping agent flushing it.** It varies the address and sends hundreds of
  requests, until the telling ones are pushed out of a bounded log. Shared
  with another log, it would push out the owner's decisions or Activity too.
- **A record that couldn't be kept.** If the read went through all the same,
  the record would miss exactly the request that mattered.
- **Another caller reading it.** The addresses are the agent's, and can carry
  what it read. They are the owner's alone, and the live stream reaches
  callers that aren't the owner.

## What changed

- **`covered.rs`:** a log of its own, beside the grants store
  (`<grants dir>.covered`), bounded as every kept log is, with a bound of its
  own (the newest 1,000). Never the activity log or the decisions log.
- **Aggregated by task and grant.** A task's first 20 requests under a grant
  are each kept, with their address. Past those, a record with no address
  counts them as the count reaches a power of two: a task that loops adds a few
  records, not one a request. An address the session already kept writes
  nothing.
- **Kept, or not covered.** In `web_ops::allow`, a request a grant covers is
  recorded first. If the record can't be kept, the grant doesn't cover it and
  the owner is asked with a card.
- **The record says:**
  - the grant's key and scope;
  - the address;
  - whether it was a page read or the agent's browser;
  - the task;
  - the count;
  - when.
- **`list_covered`** is the owner's alone, as `list_decisions` is. It has no
  stream event, so no address reaches a caller without the owner's grants.
- **The app** reads the list on connecting and as runs go on, one request at a
  time, and shows it in Activity as *Allowed by your standing grants*. Each row
  is the address, the grant that covered it and its scope, made with the
  existing row component.
- `docs/local-api.md` and the changelog say so.

## Results

| Check | Result |
|---|---|
| `hatchling-core` lib tests; the broker's `covered`, `kept_log`, `web_ops`, `api_ops` and the slice's other modules | core 224 and broker 280 passed, on 1.98.1 and on 1.85.0 |
| Hand-breaks | 9 broken, 9 caught, each by the tests named for it (below) |
| Targeted mutation (`--in-diff`, core and broker) | 24 mutants: 16 caught, 0 missed, 0 timeouts, 8 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,253 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |
| `npm run check` | 880 passed, 0 failed |
| The live adapter's coverage gate, run by hand from Git Bash, each file listed | 100% lines, branches and functions (`covered.js` and `adapter.js` included) |

### Hand-breaks

| Break | Caught by |
|---|---|
| A covered request never recorded | `a_covered_read_is_recorded_with_its_address_and_grant`, `a_looping_task_adds_few_records`, `a_covered_read_that_cant_be_recorded_asks_the_owner` |
| A request that couldn't be recorded, covered all the same | `a_covered_read_that_cant_be_recorded_asks_the_owner` |
| Every request kept, one a request | `a_tasks_requests_are_kept_first_then_as_they_double`, `a_looping_task_adds_few_records` |
| Past the first, nothing counted | the same |
| Every record with its address, past the first too | `a_looping_task_adds_few_records` |
| The same address kept again | `a_covered_read_is_recorded_with_its_address_and_grant` |
| The browser's request said as a page read | the same |
| A day grant's request said as always | `a_day_is_kept_when_the_owner_chooses_it` |
| The list served to anyone | `only_the_owner_lists_what_the_grants_covered` |

**Found by these runs:** writing the hand-breaks found two with no test to
catch them: a day grant's record said as always, and the list served to a
caller without the owner's grants. Both tests were added before the runs.
Run by hand, the live adapter's coverage gate first found the covered thread
unreached through the adapter, and an adapter test now shows it.

## Residual

- **Pages a browser loads after its first** pass through its proxy, which sees
  only their sites, never their paths. So only the page a browser opened, and
  the sites a page of it needed, are recorded.
- **A request a task grant holds** (*Allow for task*) isn't recorded here: the
  owner chose it for that task on its card.
- **A broker restart** starts each task's count again, so a task that runs
  across a restart can keep up to 20 more addresses.
- **The run's kind** (chat, scheduled, hook) isn't recorded; the task is.
