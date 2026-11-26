# WO-054 slice 2a: Windows says whether it keeps Hatchling running

Part of [#718](https://github.com/undergroundrap/hatchling/issues/718), [WO-054](../workorders/workorder-054-background-runs.md).

## What it is

On Windows, WSL stops Hatchling's distro about 30 seconds after its last window closes. Setup's **Keep Hatchling running while I'm logged in** keeps it up from a Startup-folder shortcut. Its box was already ticked by default (the Inno task has no `unchecked` flag), so the design's "checked by default" needed no change. But the broker can't see Windows, so nothing could tell the owner when keep-running was off.

- **Windows says it.** `Set-KeepRunningMark` (`installer/windows/common.ps1`) writes `on` or `off` to `/etc/hatchling/windows-keep-running`, from whether the Startup shortcut is there. It writes as root, beside the setup's version marker, where nothing in the distro but root can change it. Setup writes it once it has started the keep-alive, and **Open Hatchling** writes it each time, so it follows the owner removing the shortcut. A failure only leaves the last word in place, with a warning.
- **The broker reads it** (`windows_setup::keep_running`) each time it's asked, since the owner can change it: `on`, `off`, or nothing said.
- **Doctor says it** under **keep running**:
  - on: background work wakes on time;
  - off: a note that background work runs only while Hatchling is open, and that running Setup again with the box ticked turns it on;
  - not said: a note to open Hatchling from the Start menu;
  - off Windows: not checked, since the broker's service keeps it up.
- **No new persistence:** the same shortcut, no scheduled task, no service and no admin.

**What's left for slice 2:** the app's notes, when keep-running is off and when Hatchling was stopped while background work came due, from the clock's gaps. They need new protocol types, which are the maintainer's to approve. The design's app switch can't work as written: the broker can't reach Windows to make the shortcut. So the app's note points to Setup, as doctor does.

## Tests

- `windows_setup::tests::keep_running_is_read_as_windows_writes_it`: `on` (with Windows' line ending) and `off`, and anything else or no file is nothing said.
- `doctor::tests::keeping_hatchling_running_is_said_as_windows_said_it`: each of the four.
- `installer/windows/tests/common.tests.ps1`, a new CI step: no shortcut is `off`, Setup's shortcut is `on`, another program's shortcut isn't Hatchling's.

## The gate

RESULTS
