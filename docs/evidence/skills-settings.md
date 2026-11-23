# Evidence: Settings › Skills (WO-046 slice 2b)

What was added to the app, and how it was tested, for WO-046's owner
enablement.

| | |
|---|---|
| Base | slice 2a (`feat/skill-enablement`, #618) |
| Date | 2026-11-23 (UTC) |
| Tested on | Windows, Node 22; the demo build in the built-in browser at desktop and phone widths |

This record was added after the change, in its own commit. It changes no code.

## What it is

- **A Skills section in Settings** lists each signed skill Hatchling found:
  its name, version, and where it stands (`Enabled`, `Waiting for you`,
  `Changed since you enabled it`).
- **A skill's page** shows:
  - what it is for, in the signer's words;
  - its version, its signer's fingerprint and its digest, each shortened to
    compare by eye, with the full value on hover;
  - what enabling it lets it do, in plain words for the capabilities
    Hatchling names, and by its own name otherwise;
  - every file it holds, with its size, its playbook and program marked.
- **Changed skills** get a notice saying the skill changed since it was
  enabled and doesn't run.
- **Enable asks first.** It names what the skill will be able to do and the
  digest it binds to, and sends that digest. **Disable** asks, then stops it.
- **The signer's words are shown with any hidden character revealed**, as an
  approval's fields are: the name, version, description and file paths. A
  signer can't use bidirectional or invisible characters to make a skill
  read as something else.
- **Live:** `LiveSkills` lists skills when the connection opens, and sends
  `manage_skills` enable and disable. After an answer it lists again. Every
  other outcome (changed, not found, off, failed, refused, another result)
  is told as what it is.
- **Demo:** three sample skills, one in each state, made up and signed by
  no real key. Enable refuses a digest that isn't the one shown. Both
  commands replay after a reload, like a website permission.
- **Protocol:**
  - `AppState` gains `skills` (`Skill`, `SkillFile`, `SkillState`).
  - `Command` gains `skill.enable` (name and digest) and `skill.disable`
    (name).
  - `parseCommand` reads both.
  - Both commands are `human-only`: the agent-control bridge can't run them.
  - `PROTOCOL-LOCK.json` records the `models.ts` and `codec.ts` changes, as
    approved for slice 2.

## Tests

| Check | Result |
|---|---|
| `npm run check`: typecheck, lint, design, actions, protocol, seam, format | pass |
| `npm test` | 616 passed |
| `test:live-coverage` (100% lines, branches, functions) | 263 passed, 100% |
| `test:operator-coverage` | 16 passed, 100% |

New tests (`tests/unit/skills.test.mjs`, `tests/unit/live-adapter.test.mjs`):

- the broker's list loaded on connecting, empty until it answers, a missing
  playbook or program shown as none;
- enable sending the digest as shown, disable, each listing again;
- every other answer told as what it is, changed and not found listing
  again, a list the broker didn't give leaving the last;
- offline, nothing sent;
- the demo's sample skills, one per state, each its own copy;
- the demo refusing a stale digest and an unknown skill, enabling,
  disabling and replaying both;
- the codec reading the commands exactly and refusing malformed ones;
- both commands human-only;
- the pane's words for capabilities, sizes and short hashes.

The end-to-end settings walk (`tests/e2e/test_app.py`) visits Skills too.

**By hand, in the demo build:** the list; a waiting skill's page; Enable's
dialog naming what it grants and the digest; confirming it, the status
turning to Enabled and the button to Disable; a changed skill's notice; and
the same page at phone width.
