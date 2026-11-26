# Evidence: the UI token is handed over as the file made, never opened again (#691)

| | |
|---|---|
| Base | `main` at `3b2b82e2` (#678) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Found by | an outside model's review of `api_auth.rs`, confirmed in the code |

## What was wrong

A root broker minted the UI token like this:
1. made the file with `create_new`;
2. wrote it and closed it;
3. opened the path again;
4. `fchown`ed whatever it found to the owner.

On a default install the directory is `/var/lib/hatchling`, `0755 root`, so nobody else could get between steps 2 and 3. But the UI socket's place is configurable, and a configured directory wasn't checked. In a directory the owner could write, a link put there between those steps would have had root hand the owner any file: a path from owner to root.

Also, `SocketPath` checked only that a path was absolute, so a path with `..` in it was judged by its spelling, not by where it leads.

## What it does now

- **The file made is the file handed over.** `mint_ui_token_as` gives the `create_new` handle to its owner, then writes the token through that handle. The path is never opened again.
- **A token directory another user could write is refused**, for any broker, root or not. It must be root's or the broker's, and not writable by group or others unless it's sticky.
- **A socket path with a `..` component is refused**, so the check that keeps sockets out of the cells' view reads the path as it is.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass on both |
| the whole workspace's tests | 4492 passed, 0 failed |
| `hatchling-dev audit` | all checks passed |

New tests:
- `a_token_directory_another_user_could_write_is_refused`:
  - `0700`, `0755` and `1777` are accepted;
  - `0777` and `0775` are refused;
  - a directory owned by neither root nor the broker named is refused.
- `a_ui_token_is_not_minted_where_another_user_could_replace_it`: end to end through `mint_ui_token_for`. A `0777` directory is refused and nothing is left there; the same directory at `0700` is accepted.
- `SocketPath`: `/var/lib/hatchling/x/../sock/ui.sock` and `/run/../ui.sock` are refused, while `/run/a..b/ui.sock` is a name and accepted.

The existing `a_ui_token_is_given_to_its_owner` now also covers the handle. As a non-root user, a token for another uid must fail at the hand-over (`SocketSetup`), after the broker's own earlier token is accepted, not by refusing that earlier token.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The token isn't handed over | `a_ui_token_is_given_to_its_owner` |
| The directory check isn't made | `a_ui_token_is_not_minted_where_another_user_could_replace_it` |
| A group- or world-writable directory taken as safe | `a_token_directory_another_user_could_write_is_refused`, `a_ui_token_is_not_minted_where_another_user_could_replace_it` |
| Any owner taken as safe | `a_token_directory_another_user_could_write_is_refused` |
| A `..` taken as part of a socket path | `socket_path_rejects_relative` |

Not shown by a test: the race itself, a link swapped in between the write and a second open. The fix removes the second open, so there is no window left to race, and the first break shows the hand-over is made on the handle.

**Mutation**, on the lines changed against `3b2b82e2` (`cargo mutants --in-diff`, `hatchling-broker` and `hatchling-core`): broker 17 tested, 14 caught, 3 unviable; core 4 tested, 3 caught, 1 unviable; 0 missed, 0 timeouts.

An earlier run missed one: `==` to `!=` in the choice of whose earlier token may be cleared. As a non-root user both versions fail to mint for another uid, so the test that only asked for an error passed either way. It now asks for the hand-over's error, which the mutant doesn't give.

All of the above ran on tree `8620fa81`.
