# Evidence: the skill signer, `hatchling skill` (WO-046 slice 1b)

What was added, and how it was tested, for WO-046's signer.

| | |
|---|---|
| Base | `main` at `24dac828` (slice 1a, format 2) |
| Date | 2026-11-23 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, umask 022), toolchains 1.98.1 and 1.85.0 |

This record was added after the change, in its own commit. It changes no code.

## What it is

Signing happens on the author's side, in their own terminal. The broker has
no signing code and never holds a private key.

- **`hatchling skill key new <file>`**: 32 bytes from the operating system's
  randomness (`getrandom`), written as a key file (`hatchling-skill-signing-key-v1`
  and the key, base64) to a new file of mode 0600, never over another. It
  prints the public key, for a trust-roots file, and its fingerprint as
  admission shows it; never the private key.
- **`hatchling skill sign <dir> --key <file>`**: reads the manifest the
  author wrote (name, version, description, `instructions` and/or
  `program`; a field the format doesn't have is refused), lists every file
  under `<dir>` (sorted, nested, dotfiles included, the manifest itself
  never) with its size and SHA-256, validates, signs, and writes the format
  2 manifest. A link or a special file in the skill is refused, and nothing
  is written unless the manifest validates. The key file must be a regular
  file, not a link, that only its owner can read or write.
- **`hatchling skill check <dir> --root <public-key>`**: verifies as the
  loader will (the signature, then every listed file, not following a
  link), and names any file present but not listed, which the broker never
  reads.

The key is read from its file only, never from an argument or the
environment. Its bytes are wiped when dropped (`zeroize`; `ed25519-dalek`'s
key wipes itself). The code is `crates/hatchling-skill/src/author.rs`;
`hatchling` routes `skill` to it (`crates/hatchling-runtime`).

## Tests

| Check | 1.98.1 | 1.85.0 |
|---|---|---|
| `cargo fmt --all --check` | pass | |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass | pass |
| `hatchling-skill` | 42 passed | 42 passed |
| `hatchling-runtime` (library 6, the built binary 7) | 13 passed | 13 passed |
| `hatchling-broker --lib skills::` | 18 passed | 18 passed |

Among them: a new key's mode, refusing to overwrite, and reading back as
the key whose public half was given; keys others can read or write, a
link, and malformed key files refused; signing listing every file and the
result checking; another key, a changed file and a missing one refused at
check, an unlisted one named; a link in the skill, a manifest with neither
playbook nor program, and an unknown field each signing nothing; the
command's output and exit statuses, and **the private key in none of its
output**; and, in the broker, a skill the signer signed loading under its
key and under no other.

**The built binary, run for real:** `key new` made a key of mode 600;
`sign` listed a two-file skill; `check` verified it; the private key was in
0 lines of all three commands' output; `key new` over an existing key
exited 1.

**Hand-breaks**, each failing its test and passing again when restored: the
key file made 0644; the key's mode check removed; `check` not checking the
files.

**Mutation**, on the lines changed against `24dac828`:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-skill` (`author.rs`) | 25 | 19 | 0 | 0 | 6 |
| `hatchling-runtime` (every test target) | 2 | 1 | 0 | 0 | 1 |

The runtime's `main`, missed when only the library's tests ran, is caught
by the binary's own tests, which now include `hatchling skill`. The
unviable mutants replace a result with a `Default` the types don't have.
