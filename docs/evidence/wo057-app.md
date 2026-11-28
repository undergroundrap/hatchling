# WO-057 slice 3: the owner's say over automatic memory, in the app

Part of [#767](https://github.com/undergroundrap/hatchling/issues/767), [WO-057](../workorders/workorder-057-automatic-memory.md). Slice 1 is [wo057-taint.md](wo057-taint.md), slice 2 [wo057-routes.md](wo057-routes.md).

## What changed

- **The setting** (UI schema v18):
  - `manage_memory` `{"op": "mode"}` says whether memory is automatic (`automatic` or `ask`), and `{"op": "set_mode", "mode": …}` sets it, the owner's alone, as every `manage_memory` op is.
  - In the app, one switch, "Keep what you tell it automatically", in Settings (General, under Model) and at the top of the Memory page.
  - The app reads the mode each time the connection opens, and again as the Memory page or Settings opens.
- **The chat's Forget link:**
  - The broker's "Remembered: …" notice for a memory it kept automatically names that memory as `remembered` on the `chat_message`, by the id the owner's list gives it.
  - The app follows the notice with one choice, "Forget it". Pressed, it forgets that memory, and the choice shows it's done. A memory already gone is said ("That was no longer kept.") and the offer stays.
  - Only a `system` message's `remembered` is read: an agent's message naming one offers nothing.
- **The Memory page:**
  - Each memory's record now carries `kept_by` (`owner`, `automatic`, or left out for one kept some other way). One kept automatically reads "Kept automatically, <day>", with a "Kept automatically" label.
  - A filter: All, or Kept automatically.
  - The intro says how memory works in the mode chosen.
  - Approving a memory waiting there asks "Remember this?" and says it's kept at once, not when its work next runs.
- **The upgrade's notice:** the first memory kept automatically, while the owner hasn't chosen, adds "Memory is now automatic: … Change it in Settings." Keeping it writes Automatic as the owner's choice, so it's said once.

## Beyond the approved shape

`remembered` on `chat_message` is a field the approved wire shape didn't list: the Forget link needs the memory's id, and the app renders notices as plain text, so it can't be read from the words. It's optional, set only by the broker on its own notice, and recorded in the WO's notes. It needs the maintainer's yes before this merges.

## Tests

| Rule | Test |
| --- | --- |
| The first kept automatically says so once, the mode is then kept, each notice names what it kept as listed, and forgetting by that id forgets it | `the_first_kept_automatically_says_so_once_and_each_names_what_it_kept` |
| An owner who chose is never told | `an_owner_who_chose_is_never_told_it_is_now_automatic` |
| The mode said and set; set, it's how the next memory goes; a failed write, no scheduling, no memory | `the_owner_says_and_sets_the_mode_from_their_app` |
| The list says who kept each: automatic, owner, nobody | `the_owners_list_says_who_kept_each` |
| The wire: `mode`, `set_mode`, the `mode` result, `kept_by`, `remembered` | `the_memory_mode_and_who_kept_each_are_on_the_wire`, `keeping_an_unfinished_chat_has_its_wire_forms` |
| The page's words and labels, the mode read and set, failures said | `live-memory.test.mjs`, `live-review.test.mjs` |
| The chat's offer: only on the broker's notice, forgets by its id, says one gone already | `live-adapter.test.mjs` |

## Hand-breaks

| Break | Failed |
| --- | --- |
| The notice names nothing | 1 |
| The approval board drops what it names | 1 |
| Said every time; the owner's choice never seen | 2 |
| Never said | 2 |
| The mode always says automatic | 1 |
| `set_mode` sets nothing | 1 |
| Unscheduled, setting fails instead of `off` | 1 |
| Kept automatically reads as the owner's | 2 |
| Approved reads as nobody's | 1 |
| The UI schema stays 17 | 1 (core) |
| The chat keeps no memory id | 1 (frontend) |
| Forgetting doesn't mark the offer | 1 (frontend) |
| An automatic memory reads as plain kept | 1 (frontend) |
| The details don't say automatic | 1 (frontend) |
| Setting sends the other mode | 2 (frontend) |
| The mode is read while not connected | 1 (frontend) |
| A failed read says memory is off | 1 (frontend) |
| Approving a memory says it waits for its work | 1 (frontend) |

The frontend breaks were made on copies of the files, each restored and compared, and the worktree's diff was the same before and after.

## Gates

In the WSL2 sandbox, with `HATCHLING_REQUIRE_CELL=1`:

- **The workspace's tests:** 4743 passed, none failed.
- **Clippy, `-D warnings`, all targets and features:** the first pass found one test past 100 lines; with the v18 assertions in their own test, clean on 1.98.1 and 1.85.0. `cargo fmt --check` is clean.
- **`hatchling-dev audit`:** all checks passed.
- **Mutation, `cargo mutants --in-diff`, kill floor 1.0:** 18 mutants, 12 caught, 6 unviable, none missed.
- **Frontend:** `npm run check` green (644, 284 and 16 tests, the live adapters at 100% coverage). Wire types regenerated: UI schema 18.
- **Looked at in a browser,** against a loopback fake broker serving synthetic fixtures: the notice and its "Forget it" (pressed: forgotten, the toast, the choice marked); the Memory page's switch, intro, label, filter and its empty state, and the "Remember this?" dialog; the switch in Settings. That found the mode unread on a fresh load straight to the Memory page, fixed by reading it as the connection opens.
