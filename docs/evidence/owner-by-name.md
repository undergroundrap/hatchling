# Evidence: the owner's setting takes an account's name (WO-033)

What was tested, how, and what came back, for one of
[WO-033](../workorders/workorder-033-install-it-and-leave-it-on.md)'s
open items: the broker's whole configuration is
`/etc/hatchling/hatchling.env`, with no shell and no `id -u`.

| | |
|---|---|
| Base | `main` at `3a5c4445` |
| Tested tree | `0c19a849` |
| Date | 2026-11-21 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no code.

## What changed

`HATCHLING_AGENT_UID` took a local account's name (#280), and
`HATCHLING_OWNER_UID` took only a number: writing the settings file by
hand needed `id -u`. Now the owner's setting is a uid or a local
account's name, looked up in `/etc/passwd` as the agent's is, through one
function (`config::uid_value`). A name no account has is refused, naming
the setting. `SUDO_UID` stays a number: `sudo` sets it, never a person.

## The other half of the item: `HATCHLING_CELL_CGROUP`

The item also said `HATCHLING_CELL_CGROUP` is set in the service's
drop-in, not in the file. It stays there, on purpose. Its value is true
only while that drop-in's `Delegate=yes` and `DelegateSubgroup=broker`
are: `install.sh` installs the drop-in only where systemd has
`DelegateSubgroup` (254 or later). Put in `hatchling.env`, the file the
owner edits, it could be set where the delegation isn't, and the broker
would be handed a cgroup it can't use. It isn't a setting the owner
chooses; it's a fact of how the service is started, kept beside the
lines that make it true. Every setting the owner chooses is in the file.

## Results

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | clean |
| `hatchling-broker` lib tests `owner::`, `config::`, `cell::`, `doctor::`, `environment`, both toolchains | passed |
| Mutation of the lines changed (`--in-diff`, the broker's lib tests) | 6 mutants, 6 caught, 0 missed, 0 timeouts, 0 unviable |

The test of the owner now checks: a name (`root`, in every
`/etc/passwd`) is its uid, and wins over `SUDO_UID`; a name no account
has, and `-1`, are refused naming `HATCHLING_OWNER_UID`; `SUDO_UID` of
`-1`, or of a name, is refused naming `SUDO_UID`. The agent's test now
also checks its refusal names `HATCHLING_AGENT_UID`.

### Hand-breaks

Each was applied, the owner and config tests run, then undone. 4 broken,
4 caught:

| Break | Caught by |
|---|---|
| The owner's name not looked up (a number only, as before) | `the_owner_is_named_or_ran_sudo_or_is_this_user` |
| `SUDO_UID` taking a name | the same |
| The owner's refusal naming the agent's setting | the same |
| The agent's refusal naming the owner's setting | `the_agent_uid_helpers_refuse_only_an_unallowed_nobody`, `invalid_agent_uid_is_an_error` |

## Residual

- `install.sh` still writes the owner as a number. Both work; the number
  is what it already writes, and an update leaves the file as it is.
