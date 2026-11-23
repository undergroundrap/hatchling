# Evidence: the agent uses the skills the owner enabled (WO-046 slice 3a)

What was added, and how it was tested, for WO-046's discovery and use.

| | |
|---|---|
| Base | `main` at `5bcfdc5c` (slice 2a) |
| Date | 2026-11-23 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, umask 022), toolchains 1.98.1 and 1.85.0; the frontend on Windows, Node 22 |

This record was added after the change, in its own commit. It changes no code.

## What it is

- **Two session ops (protocol v34).**
  - `SkillOp::List` answers each skill the owner enabled and the broker
    runs, by name. Each entry has its version, when it applies (in its
    signer's words), whether it has a playbook, the capabilities its
    program declares, and whether it lists helper files. As many as one
    frame carries; the rest are counted.
  - `SkillOp::Read` gives an enabled skill's playbook, or a file it lists.
    The file is read and checked against its signed hash at the read, not
    only at load.
- **Fenced by the broker.** A read comes back in `[SKILL …]` and
  `[/SKILL]`, labelled with the skill, the file and its signer's
  fingerprint. A forged marker inside, in any case, is made inert. Labels
  can't leave their place. The text says it's guidance that grants no
  tool, grant or approval, and that the system prompt and the owner's
  request win.
- **What a read refuses:**
  - a file the skill doesn't list, or no playbook: `NoSuchFile`;
  - over 32 KiB, or too large for one frame: `TooLarge`;
  - not UTF-8: `NotText`;
  - changed since load: an opaque error, with the reason in the log.
- **The agent's tools:** `skill_list` and `skill_read`, beside
  `skill_invoke`.
- **The prompt:** at a run's start, the agent lists the enabled skills.
  Its prompt says which they are, fenced as data, within 12 KiB, and how
  many more there are past that. A skill with helper files is listed only
  where the run has a computer.
- **Every run reaches the skills.** Before this, every run's session,
  chat and scheduled, was served with no skills, though its tool list
  offered `skill_invoke`. The broker now publishes its skills once at
  start, and serves each run's session with them: list, read and invoke,
  as the maintainer chose for every run. A coding harness's id names no
  skill in a run's session: a harness spends the operator's vendor key,
  and its grant is the main session's.
- **Activity:** a skill's use is tallied under `Used a skill`, its reads
  included. The broker's log names the skill and file of each read.

## Tests

| Check | 1.98.1 | 1.85.0 |
|---|---|---|
| `cargo fmt --all --check` | pass | |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass | pass |
| `hatchling-dev audit`, `protocol-schema` | pass | |
| `hatchling-core`, `hatchling-skill`, `hatchling-agent` | pass | pass |
| `hatchling-broker --lib` | 1799 passed | 1799 passed |
| `hatchling-broker`: `artifacts`, `harness`, `harness_cli`, `harness_keeper`, `leaving_keeper`, `skill_envelope`, `skill_rotation`, `skill_run`, `skill_session`, `handshake`, `smoke` | pass | |

The frontend's generated protocol was regenerated, and `npm run check`
passes in full.

Among the new tests:
- **Through a real broker session:**
  - only the enabled skills are listed;
  - a playbook and a listed file are read, fenced once and labelled, with
    a forged marker made inert;
  - an unlisted file, a path out of the skill, a binary file, a file over
    the bound, a skill with no playbook, a waiting skill and an unknown
    one are each refused as what they are;
  - a playbook changed after load isn't read;
  - the session survives all of it.
- **Harness reach:** a run's skill ops miss a harness that the main
  session reaches, with the same skills.
- **The renderers:** markers neutralized in any case, labels that can't
  leave their place, the list's budget and its count of the rest, and
  skills needing a computer left out without one.
- **The agent:** the list and read ops, their wire forms, and each answer
  mapped; the tools' schemas and outputs.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| A read not checked against the signed hash | `a_session_lists_and_reads_only_what_the_owner_enabled` |
| A read with no size bound | the same |
| A read of a file that isn't text | the same |
| A run's session reaching a harness | `nothing_but_the_main_session_reaches_a_harness` |
| A read's text not neutralized | `a_read_is_fenced_once_and_labelled_with_its_skill_and_signer` |
| A label not filtered | the same |
| A skill with helpers listed where there's no computer | `the_list_is_fenced_says_how_to_use_each_and_keeps_to_its_budget` |
| The list past its budget | the same |

A ninth break, listing skills that aren't enabled, failed nothing: only
enabled skills are ever registered, and the list keeps to registered ones.
That filter was removed.

**Mutation**, on the lines changed against `5bcfdc5c`:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-broker` (library, `skill_session`, `harness`) | 31 | 15 | 7 | 0 | 9 |
| the seven, again, with the new tests, `handshake`, `smoke` and `skills_live` (10 matched) | 10 | 10 | 0 | 0 | 0 |
| `hatchling-core` (library) | 32 | 32 | 0 | 0 | 0 |
| `hatchling-agent` (library, `channel`) | 35 | 29 | 0 | 0 | 6 |

The seven the broker's first run missed:
- **Three bounds in a read,** now each tested at its edge: a file of
  exactly 32 KiB is read; one within the bound but six times its size
  once escaped is refused, as no frame carries it; and a playbook alone is
  said to have no helper files.
- **`publish` and `live`,** now tested in a binary of their own, so the
  broker-wide value set there reaches no other test.
- **The main session's entry point,** reached only by the tests that
  start the real broker (`handshake`, `smoke`), which catch it.

Core's first run missed the prompt list's 12 KiB budget, and the agent's
a read exactly one frame long. Both are tested now, and both crates are
at 0 missed. The unviable mutants replace a result with a `Default` the
types don't have.
