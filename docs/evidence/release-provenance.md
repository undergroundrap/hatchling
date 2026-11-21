# Evidence: install.sh checks who built the release when gh can (#556)

What was tested, how, and what came back, for `install.sh`'s check of a
release's build attestation.

| | |
|---|---|
| Base commit | `4d442312` (`feat(install): install the agent's computer's image from a release on Linux; close WO-038 (#555)`) |
| Tested tree | `537af38c` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user with systemd visible |
| Tools | dash, shellcheck, Python 3 |

This record was added after the runs, in its own commit. It changes no code.

## The threat

`install.sh` checks every file against the release's `SHA256SUMS`, which
comes from the same release. Someone with a stolen token could replace a
release's files with their own and a `SHA256SUMS` to match, and the check
would pass. The release's build attestation, signed for this repository's
release workflow on GitHub's runners, is what tells those apart; checking
it was a manual step.

## What changed

- **`packaging/release-provenance.sh`**, sourced by `install.sh` after the
  `SHA256SUMS` check and before anything is installed:
  - with `gh` installed and signed in as the owner, it runs
    `gh attestation verify SHA256SUMS --repo undergroundrap/hatchling
    --signer-workflow …/release.yml`. The workflow attests `SHA256SUMS`
    with the rest, and every file `install.sh` installs has just matched
    it, so one check covers all of them;
  - a `SHA256SUMS` that doesn't verify stops the install, with nothing
    installed, saying what `gh` said;
  - without `gh`, or with `gh` signed out or GitHub not reached, it says it
    couldn't check, and how to, and goes on as before.
- **`gh` runs as the owner,** the user who ran `sudo`, in their home: root
  has no sign-in of its own.
- **Not for Windows Setup's payload,** which is verified with Setup.
- **CI:** the helper's tests run in the install job, which also checks that
  its install, of an unattested layout with `gh` signed out, says it
  couldn't check.
- **Docs:** `docs/install.md`, `docs/updating.md`, and
  `docs/releasing.md`'s "Verifying a release".

## Results

| Check | Result |
|---|---|
| `dash -n` on `install.sh` and the helper | 0 |
| shellcheck on the helper, warnings and above | clean |
| The helper's tests (`scripts/test_provenance.py`), with stand-ins for `gh` and `runuser` | 5 passed |
| `get.sh`'s tests | 23 passed |
| Hand-breaks | 6 broken, 6 caught |

### The helper's tests

| Case | What happened |
|---|---|
| Attested by the release workflow | goes on, saying so; asked with `--repo` and `--signer-workflow` |
| Not attested | stops, "nothing was installed", with `gh`'s own words |
| Who runs `gh` | the owner, by `runuser`, with their home, both times |
| `gh` signed out | goes on, saying it couldn't check, and `gh auth login`; nothing verified |
| No `gh` | goes on, saying how to check by hand |

### Hand-breaks

| Break | Caught by |
|---|---|
| Not attested, and the install goes on | `test_not_attested_stops_the_install_saying_why` |
| Any workflow of the repository taken, not the release workflow | `test_attested_by_the_release_workflow_goes_on_saying_so` |
| `gh` run as root | `test_gh_runs_as_the_owner_in_their_home` |
| Verified with nobody signed in | `test_signed_out_goes_on_saying_it_couldnt_check` |
| What `gh` said never looked at | `test_not_attested_stops_the_install_saying_why` |
| Without `gh`, nothing said | `test_without_gh_it_goes_on_saying_how_to_check` |

## Residual

- **`install.sh` calling the helper** is proven by CI's install job, which
  checks its words, not hand-broken here.
- **The first `curl` + `sh` install:** whoever can replace a release can
  replace `get-hatchling.sh` too. The install guide's verify-first step is
  that check.
- **Someone who can run the release workflow, or GitHub itself,** makes a
  release that verifies. An offline signing key for that was considered,
  and isn't planned.
- **The tag isn't pinned:** an older release's genuine files verify, as the
  owner chose that release.
- **Signed in with GitHub unreachable after the sign-in check,** the
  verify fails and the install stops, saying what `gh` said: run it again.
