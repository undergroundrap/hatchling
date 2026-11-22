# Evidence: Windows Setup's install never prints the app's address (#583)

What was found, what changed, and how it was tested, for
[#583](https://github.com/undergroundrap/hatchling/issues/583), found in
the v0.9.0 release candidate's physical-key check.

| | |
|---|---|
| Base | `main` at `8de0e034` (v0.9.0) |
| Date | 2026-11-22 (UTC) |
| Machine | x86_64, Windows 11 Home, WSL 2 |
| Release files | the v0.9.0 release candidate's (run 45052217363): programs, app and Room image; the installer made from each tested commit |

This record was added after the change, in its own commit. It changes no code.

## The problem

Setup keeps the app's address off the screen: it holds the token every
app connection needs, and the Start menu's *Open Hatchling* fetches it each
time. `setup.ps1` drops it from the installer's output with a filter. On
a first install with a hosted model, it can't: the OpenRouter key's prompt
needs the console, so the output is shown as it comes, and
`scripts/install.sh` ended by printing the address.

## The change

- `scripts/install.sh` takes `--windows-setup`: after *Hatchling is
  running, and starts at boot.* it stops, and the address isn't printed.
- `installer/windows/setup.ps1` passes it on every path. The filter stays
  for the rest of what's only of use in a Linux shell.
- CI's service job, after the hosted install, runs the installer as Setup
  does (`--keep-model --windows-setup`): it finishes, and no line holds
  `hatchling-ui=`.

Setup and its installer come from the same release, so Setup never passes
the option to an installer that doesn't know it.

## Tests

Windows Setup's own script, `setup.ps1 -Hosted -BundledRoom -Unattended`,
on a fresh throwaway distro each time (a copy of a prepared Ubuntu 24.04
with its Hatchling removed, deleted after). A stand-in key, random and
made for the run, was typed at the key's prompt once it showed, as a
person pastes one. Only the setup scripts differ between the two runs.

| | Setup's scripts from `main` (`8de0e034`) | From this change (`738fdc77`) |
|---|---|---|
| Setup's exit | 0 | 0 |
| `Nothing found wrong.`, `Hatchling is running`, `Done.` | all | all |
| Lines of Setup's console holding the app's address | **1** | **0** |
| Lines holding the stand-in key | 0 | 0 |
| The service, and the hosted model's endpoint in its settings | active, set | active, set |

`sh -n scripts/install.sh` and PowerShell's parser on `setup.ps1` pass.
CI's service job runs the new check on the pull request.
