# Approved tools

A coding CLI (Claude Code, the Codex CLI) with a vendor API runs in a CLI cell, and it runs programs there: a shell, git, ripgrep, the coreutils. Those programs are the **approved tools**, and nothing else is in the cell. Hatchling doesn't pick or bundle them: you approve each one, and Hatchling makes sure the cell runs the program you approved and can't change it ([WO-019](workorders/workorder-019-approved-tools.md)).

## What approving a tool means

- **It's pinned.** The list records the tool's path, its SHA-256 and when you approved it. That hash is what the tool must be.
- **The cell runs a copy.** At the broker's start each tool is read and checked against its hash. At the first run that gets the tools, the broker copies them into a directory of its own, from bytes that still match, and every CLI cell runs that copy, read and execute only, at the tool's own path. A write to the tool on your machine never reaches a cell.
- **It's checked before every run.** If a tool's file changed, it's hashed again. If it's another program (a package upgrade, or anyone who could write it), the run is refused, and its report names the tool. Runs go on being refused until the file is the program you approved again, or you approve the new one and restart the broker.
- **No network of its own.** A tool reaches the outside, if at all, the way everything in a cell does: through the harness's relay to its vendor, and nothing else.
- **Not pinned:** a tool's shared libraries. They're found with `ldd` and mounted read-only from the host, as for every cell's programs.

## The list

The list is `approved.json` in `<vault dir>.tools/` (beside `HATCHLING_VAULT_DIR`). It names what the agent's coding CLI may run, so it belongs to the user the broker starts as, and no one else may write it: not the file, not its directory. If either is another user's, or writable by a group or by others, the broker refuses to start, and so does every `tools` command. Run as root, that user is root, not `hatchling-broker`: a broker that has given up root can't change what it will run after a restart.

**The first start makes it.** The first time the broker starts with a harness's API set, it records the tools it was given, each with its hash: the paths `HATCHLING_HARNESS_TOOLS` names, or else the defaults this host has (a shell, bash, git, ripgrep and the usual coreutils, each one present). Your cells get the same tools as before.

After that, the list names the tools. `HATCHLING_HARNESS_TOOLS` may stay set only while it names the same tools; once you change the list, unset it, or the broker refuses to start and says why.

## Adding, listing and removing

Run these as the user the broker starts as (with `sudo` for a root broker), with the broker's environment. `hatchling broker tools ...` is the same command.

```bash
hatchling-broker tools add /usr/bin/jq
```

Records `/usr/bin/jq` and its SHA-256. The path must be absolute and a regular file, through its links. Run it again on a tool that changed, and it approves the new program, saying what the hash was.

```bash
hatchling-broker tools list
```

Each tool, its SHA-256 and when it was approved. A tool that's no longer the program approved is marked `CHANGED` with its hash now, and one that can't be read `UNREADABLE`.

```bash
hatchling-broker tools remove /usr/bin/jq
```

Takes it off the list.

**A change takes effect at the broker's next start**, when the tools are read and their copy made. A broker that's running keeps the tools it started with.

If there's no list yet when you first run `add` or `remove`, the command makes it first, as the broker's start would, and says so.
