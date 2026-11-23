# Evidence: the owner enables each skill (WO-046 slice 2a)

What was added, and how it was tested, for WO-046's owner enablement on
the broker's side.

| | |
|---|---|
| Base | `main` at `86726b12` (slice 1b, the signer) |
| Date | 2026-11-23 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, umask 022), toolchains 1.98.1 and 1.85.0; the frontend on Windows, Node 22 |

This record was added after the change, in its own commit. It changes no code.

## What it is

- **A skill runs only when the owner enabled it, at its digest now.** The
  digest is the SHA-256 of the skill's signed manifest, which lists every
  file's size and hash and the capabilities its program declares. Every
  skill whose signature verifies is found, and listed `enabled`, `waiting`
  (never enabled) or `changed` (enabled at another digest). Only `enabled`
  registers. A skill changed after it was enabled stops, and waits again.
- **Enabling is the grant.** An enabled skill is admitted with exactly the
  capabilities its program declares, which the owner is shown.
  `HATCHLING_SKILL_GRANTS` is refused at start, saying what to do instead.
- **Harnesses keep a grant of their own.** The built-in and manifest coding
  harnesses took their grant from that setting (`claude-code:code`); they
  now take it from `HATCHLING_HARNESS_GRANTS` (`claude-code,codex-cli`). An
  entry that isn't a harness's name alone, the old form among them, is
  refused, saying what to write.
- **The owner's choices are a sealed store** (`HATCHLING_SKILL_ENABLEMENTS_DIR`,
  `Store::SKILLS`, the broker's key): each skill's name, the digest
  enabled, and its version, signer and capabilities then. At most 64; a
  65th is refused and nothing is dropped. A store holding what it can't
  (two records for one name, a digest not lowercase hex SHA-256, an empty
  or over-long name, a negative time) enables nothing. The directory is
  required with skills, must be absolute, and can't be inside the skills
  dir or hold it.
- **The API** (`manage_skills`, UI schema 10, the owner's alone): `list`
  reads the skills dir as it is and lists every skill found; `enable` names
  a skill and the digest the owner saw, reads the dir again, and is
  `changed` unless that digest is the skill's now; `disable` stops it before
  the owner is answered. No session op reaches the store.
- **At the broker's start** the enablements are read once the keys are in
  hand. A store that can't be read enables nothing, and says so in the log.
- **One rebuild at a time, from the last good roots.** Enabling, disabling,
  a list's read of the dir and a trust-roots reload each rebuild the set
  under one lock, so a reload can't put back a skill the owner just
  disabled. A rebuild for the owner uses the trust-roots file last applied,
  so disabling works while the file on disk is bad.
- **No more code pins across reloads.** A skill's code was pinned from the
  broker's first sight of it, so the same `name@version` with other bytes
  was refused until a restart. The digest the owner enabled now fixes every
  byte: other bytes are another digest, listed `changed`, and never run.
  The registry still pins the program's hash at registration, and each run
  checks it.

## Tests

| Check | 1.98.1 | 1.85.0 |
|---|---|---|
| `cargo fmt --all --check` | pass | |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass | pass |
| `hatchling-dev protocol-schema` | pass | |
| `hatchling-core`, `hatchling-skill`, `hatchling-vault` | 469 passed | 469 passed |
| `hatchling-broker --lib` | 1799 passed | 1799 passed |
| `hatchling-broker`: `artifacts` 31, `harness` 14, `harness_cli` 7, `harness_keeper` 12, `leaving_keeper` 17, `skill_envelope` 1, `skill_rotation` 7, `skill_run` 15, `skill_session` 5 | 109 passed | 109 passed |

The frontend's generated protocol was regenerated from the schema
(`npm run protocol:generate`) and checked (`protocol:check`), with
`typecheck`, `lint`, `format:check`, `seam:check`, `actions:check` and
`npm test` (100% coverage) passing.

Among the new tests: a skill found and waiting, enabled at its digest and
running with what it declares, listed changed at another digest, and
changed again when re-signed declaring more; the listing's fields, sorted
by name; enabling refused as changed or not found, kept across a restart
and handed over at the start; disabling stopping it at once; a skill put
in the dir since showing at the next list; a skill changed between the
owner's list and their enable refused; the store's bound of 64 and every
unsound record enabling nothing; a reload under new roots keeping what was
enabled, and a disable taking effect while the roots file is bad; the
retired setting refused by name, in `Config::parse` too; a harness grant
by name, and the old form and non-names refused; the store in the broker's
sealed stores; `manage_skills` refused to a caller who isn't the owner;
the action's and results' wire forms; and, in the integration suites, a
session finding a skill the owner hasn't enabled unknown, and a skill
running only at the digest enabled across reloads.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| A skill counted enabled at any digest | `a_skill_runs_only_enabled_at_its_digest_now`, `a_changed_skill_waits_for_the_owner_again` |
| Every skill found registered, enabled or not | `skills_load_holds_the_admitted_registry`, `enabling_runs_a_skill_and_disabling_stops_it_both_kept`, `an_unsound_store_enables_nothing` |
| `enable` ignoring the digest the owner saw | `enabling_runs_...`, `a_changed_skill_waits_...` |
| `disable` not rebuilding the set | `enabling_runs_...` |
| `list` and `enable` not reading the dir again | `a_skill_put_there_since_shows_at_the_next_list`, `a_changed_skill_waits_...` |
| The owner's rebuild using the file as last seen, not as last applied | `skills_load_holds_the_admitted_registry` |
| `HATCHLING_SKILL_GRANTS` not refused | `the_broker_refuses_the_retired_skill_grants_by_name` |
| Every configured harness granted | three `harness::tests` |
| The store unbounded | `the_store_is_bounded` |

The rebuild lock (a reload can't swap in a set built before a disable) is
not exercised by a test: it would need the two to interleave on purpose.
It is one mutex held across each of the four rebuilds, read in review.

**Mutation**, on the lines changed against `86726b12`:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-broker` (library tests) | 96 | 68 | 3 | 0 | 25 |
| the three, again, with two new tests, and `handshake` and `smoke` | 6 | 5 | 0 | 0 | 1 |
| `hatchling-core` | 1 | 0 | 0 | 0 | 1 |
| `hatchling-skill` | 2 | 2 | 0 | 0 | 0 |
| `hatchling-vault` | 2 | 2 | 0 | 0 | 0 |

The three the library's tests missed: the store's `Debug` output, and
`of` opening a store when skills are off but a directory is set, now each
tested; and `run_serve` returning `Ok(())`, which only the tests that start
the real broker reach (`handshake`, `smoke`), and which they catch. The
unviable mutants replace a result with a `Default` the types don't have.
