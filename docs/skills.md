# Skills

A skill is know-how the agent can follow: a short playbook for a kind of
work, with any files it refers to, and sometimes a program, all signed by
whoever wrote it. The agent sees a list of the skills you enabled, reads a
playbook when a task calls for one, and uses a skill's files in its
computer.

**A skill grants nothing.** Its playbook is guidance for a task you asked
for. It can't widen the agent's tools, its grants or its approvals, and it
ranks below your own words. A playbook that tells the agent to do something
it isn't granted is refused at the grant, as the same request from a web
page would be. [WO-046](workorders/workorder-046-signed-playbook-skills.md)
has the design and its threat model.

## The skills Hatchling ships

An install comes with three playbooks, signed by Hatchling's maintainer:

| Skill | For |
|---|---|
| `write-a-report` | A report file from your material and granted web sources, every load-bearing claim traced to a source the agent read |
| `tidy-a-folder` | A plan of moves, removals and what's left alone, shown to you and agreed before anything changes, then checked |
| `research-a-question` | An answer in the chat from sources read in full and cross-checked, with what couldn't be verified said plainly |

**Each one is off until you enable it.** In the app, **Settings › Skills**
shows each skill: what it's for, who signed it, its digest and every file
it holds. Read the playbook, then enable it. From a terminal, as yourself
(not `sudo`), `hatchling-broker skills list`, `skills enable <name>` (it
shows the skill and asks you to type yes) and `skills disable <name>` do
the same. A skill changed since you
enabled it, by an update or anything else, stops and waits for you again.

They're installed in `/usr/local/share/hatchling/skills`, and the
maintainer's public key in `/etc/hatchling/skill-trust-roots`. An update
replaces Hatchling's own skills by name, and keeps your trust-roots file as
it is. To trust none of them, take the maintainer's key out of that file,
leaving its first line and `end`. Hatchling reads it again within two
seconds and drops every skill that key signed. Deleting the file doesn't
do it: a file that's gone, or can't be read, changes nothing, since it's
indistinguishable from an accident.

```text
hatchling-trust-roots v1
end
```

## Writing a skill

A skill is a directory named for it. Names keep to letters, digits, `.`,
`_` and `-`, and don't start with a dot. It holds:

- `manifest.json`, which you write and the signer completes;
- the playbook, a Markdown file named in the manifest, up to 32 KiB;
- any files the playbook refers to: up to 64 files and 1 MiB in all.

```json
{
  "name": "write-a-report",
  "version": "1",
  "description": "Use when the owner asks for a report ... Not for ...",
  "instructions": "SKILL.md"
}
```

A skill may also have a program, which runs in a cell of its own:
`"program": {"path": "main", "capabilities": ["..."]}`. You'll see the
capabilities it asks for before you enable it.

**What makes a playbook work.** Hatchling's own follow this:

1. **Name and version.** Short and stable. Any change to any file is a new
   version, signed again, and enabled again.
2. **The description is the trigger:** when it applies, when a similar task
   belongs elsewhere, and what the owner has at the end. One line, up to 512
   bytes. A description that only says what the skill is gets read for the
   wrong tasks and missed for the right ones.
3. **Purpose and inputs:** one line on what a finished run looks like; what
   to establish before starting; what to ask the owner when an input is
   missing, rather than guessing.
4. **Steps:** imperative, ordered, each one checkable, naming the tools as
   the agent knows them (`run`, `read`, `search`, `write`, `edit`,
   `give_file`, `web_search`, `web_fetch`), and saying what to do when a
   step fails: retry once, skip and say so, or stop.
5. **Guardrails:** what the playbook never does, in the same voice as its
   steps.
6. **Stop and ask, and the answer:** when to end early and go back to the
   owner, and what the answer must hold: what was done, where the result
   is, and what couldn't be verified.

Refer to supporting files by name rather than putting them in the playbook:
the agent reads it in the middle of a task, so it stays short. In a chat,
the agent finds a skill's files read-only at `/workspace/skills/<name>/` in
its computer, and can run a helper script there with an interpreter
(`python3 /workspace/skills/<name>/outline.py`).

**Two rules follow from "a skill grants nothing":**

- **Write the fallback, not the grant.** A playbook never has a step that
  needs an approval, a grant or a tool the run may not hold. It says what
  to do instead: "if this site isn't granted, ask through the usual request
  and go on with what you have". A scheduled run has no computer: say what
  to do without one.
- **A playbook serves the owner's task.** One that tells the agent to hide
  its reasoning, skip a check, or present something unverified as verified
  is hostile, whoever signed it. No scanner is trusted to find one: what
  holds is that a skill grants nothing, and that you read a playbook before
  you enable it.

## Signing a skill

Signing happens in your own terminal, never in Hatchling. You need the
`hatchling` program, from the `hatchling-runtime` crate, on Linux:

```sh
cargo install hatchling-runtime
```

Make a key once. It's written to a new file only you can read, and the
command prints its public key, for a trust-roots file, and its fingerprint,
as the app shows it:

```sh
hatchling skill key new ~/hatchling-skill-signing.key
```

Keep the key file to yourself, and keep a copy somewhere safe: whoever
holds it can sign skills your Hatchling trusts. Never put it in a
repository. Then sign, and check the skill as Hatchling will load it:

```sh
hatchling skill sign ./my-skill --key ~/hatchling-skill-signing.key
hatchling skill check ./my-skill --root <your public key>
```

`sign` lists every file in the directory with its size and SHA-256 and
signs the manifest. The key is read from its file only, never from an
argument or the environment. `check` names any file the manifest doesn't
list: Hatchling never reads one.

## Installing your own

As root, put the skill's directory beside Hatchling's, owned by root and
writable by nobody else:

```sh
sudo cp -R ./my-skill /usr/local/share/hatchling/skills/
sudo chown -R root:root /usr/local/share/hatchling/skills/my-skill
sudo chmod -R u=rwX,go=rX /usr/local/share/hatchling/skills/my-skill
```

Hatchling trusts a skill only when it verifies under a key in
`/etc/hatchling/skill-trust-roots`. The file holds at most two keys: a
`primary` line and a `previous` line, either of which verifies a skill. To
trust your key as well as the maintainer's, add yours as the line that's
free; to trust yours alone, make it the `primary` and remove the other:

```text
hatchling-trust-roots v1
primary <the maintainer's public key>
previous <your public key>
end
```

The file must stay root's and writable by nobody else, or Hatchling
refuses it. It's read again within two seconds of a change, so a key you
take out stops its skills then. Then enable the skill in **Settings ›
Skills**.

**The agent can help write one.** Ask it to draft a skill in its computer
and hand you the folder. Read it, then sign and install it yourself, as
above: the agent never signs a skill, never holds a signing key, and never
installs one.
