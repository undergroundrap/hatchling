# Tidy a folder

**Purpose:** a finished run leaves the folder organised the way the agreed
plan said, with nothing removed that the owner didn't approve by name, and a
summary the owner can check.

**Before you start:** this needs your computer (`run`), and the owner there
to agree the plan. In a run without either, say so, and stop.

## Inputs

- Which folder.
- What "tidy" means here: by kind, by date, by project, or the owner's own
  scheme. Ask if it's unclear.
- Whether removing anything is in scope at all.

## Steps

1. **Inventory first, change nothing.** List the folder recursively with
   `run`: names, sizes, kinds. Note duplicates by content hash
   (`sha256sum`), empty files and folders, and anything that isn't a plain
   file (links, special kinds): those are reported, never moved.
2. **Draft the plan** as three separate lists, shown to the owner in the
   chat: moves and renames (from, to); removals, each with its size and why
   it's proposed; and left alone, anything skipped, with the reason. End your
   turn there, and go on only from the owner's answer on the plan as a
   whole, and on the removals in particular.
3. **Carry out the approved moves and renames.** Create target subfolders
   as needed. If a file has changed since the inventory, stop on that file
   and note it: don't move a file that isn't the file the plan described.
4. **Carry out only the approved removals,** exactly the list approved.
   Nothing is removed on this playbook's authority, and nothing is added to
   the list mid-run.
5. **Verify:** list the folder again, and check the result against the
   plan, item by item. Any mismatch is reported, not silently repaired.

## Guardrails

- Never touch the workspace's `inbox`, and never follow a link out of the
  folder.
- Never remove the only copy of anything without the owner's explicit yes
  on that item.
- Never empty a folder that wasn't in the plan.
- If the folder holds something that looks like a secret, stop and tell
  the owner: don't move it, copy it, or name its contents.

## Stop and ask

- The plan would move or remove more than the owner plausibly meant: a
  folder far larger than the conversation suggests.
- Two files with the same name would collide at a target.
- The inventory can't be completed.

## The answer

What moved, what was removed, what was left alone, and the verification
result.
