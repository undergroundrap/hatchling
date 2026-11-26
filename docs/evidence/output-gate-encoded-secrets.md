# Evidence: the output gate finds a held secret percent-encoded, or as hex written any way (#705)

| | |
|---|---|
| Base | `main` at `5288773a` (#703) |
| Date | 2026-11-26 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Found by | an outside model's review of the output gate, confirmed in the code. It reported the percent-encoded form; the spaced-hex form was found checking it |

## What was wrong

Everything sent to a person passes the output gate, which redacts a held secret. `output_gate::holds_secret` looked for one in two ways:
- `SecretSet::exposed_in`: the secret raw, as unbroken hex in upper and lower case, and as standard or URL-safe base64 at each alignment;
- the reply's letters and digits, lowercased (`normalize`), containing the secret's own letters and digits.

A held secret written either of two ordinary ways matched neither:
- **percent-encoded**, `%6e%69%67%68…`, the form a link carries;
- **hex with separators**, `6e 69 67 68…` or `6E:69:67:68`.

Folded, both leave the secret's hex, and the folded comparison only knew the secret's letters. So a reply could carry a held secret to the owner, or in a link to whoever the agent named.

## What it does now

`holds_secret` reads the reply as a site reads a link: as written, and with its `%` escapes undone (and `+` as a space), through the held check's own `as_read`. The check on a web read already did this. Then:
- every form `SecretSet` knows is looked for in both readings;
- the folded form of both readings is compared with each secret's letters, as before, and with its hex, for a secret of at least 8 bytes (`MIN_BLOCK_CHARS`).

A shorter secret's hex isn't looked for folded: seven bytes are fourteen hex digits, which ordinary text holds often enough. Its unbroken hex is still found, as before, and so is the secret itself once percent-decoded. The hex form is made at its final size and wiped when dropped, like the folded secret.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4501 passed, 0 failed |
| `hatchling-dev audit` | all checks passed |

New tests in `output_gate.rs`:
- `a_secret_is_found_percent_encoded_or_as_hex_written_any_way`:
  - **found:** a held secret percent-encoded in a link, in lower and upper case; partly escaped, as a link's encoder leaves it; as hex with spaces; as hex with colons; and a binary secret, which has no letters to fold, spaced and percent-encoded;
  - **passed:** hex that isn't a held secret's;
  - **a seven-byte secret:** its spaced hex passes, while its unbroken hex, its percent-encoding and the secret itself are found.
- `a_secrets_hex_form_is_its_bytes_in_lower_case_at_its_final_size`.

**Hand-breaks**, each failing `a_secret_is_found_percent_encoded_or_as_hex_written_any_way` and passing again when restored:

| Break | Caught |
|---|---|
| The reply checked only as written, not percent-decoded | yes |
| The hex form not looked for | yes |
| The byte bound removed, so a short secret's hex is looked for too | yes |

The third first passed with a three-byte test secret: its six hex digits are already under the folded form's own minimum of eight. The test now uses a seven-byte secret, whose fourteen digits only the byte bound keeps out.

**Mutation**, on the lines changed against `5288773a` (`cargo mutants --in-diff`, `hatchling-broker`, `--lib`): 17 mutants, **13 caught, 0 missed, 0 timed out, 4 unviable.**

## What it doesn't do

No filter on text stops an encoding the agent invents (reversed, rot13, one character per line). The gate isn't the main guard: the cell holds surrogates, never real keys, and a secret reaches it only if the owner grants one or it turns up in what the agent reads. This closes the two ordinary encodings of a URL and of bytes, so the gate is at least as strict as the check on a web read.
