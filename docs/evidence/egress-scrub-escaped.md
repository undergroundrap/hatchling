# An echo's escaped forms scrubbed, and nothing held grown (#683)

Closes [#683](https://github.com/undergroundrap/hatchling/issues/683), hardening in `crates/hatchling-egress/src/scrub.rs` from an outside review, confirmed in the code. The chunk-boundary logic was found right.

## The threats

1. **An echo in an escaped form wasn't found.**
   - The scan found each verbatim occurrence of the credential.
   - An upstream that echoed it JSON-escaped (`\/`, `\u00XX`) or percent-encoded (`%2B`, `%2F`, `%3D`) handed it to the cell, which can unescape it and send it anywhere it may send.
   - Today's model keys are letters, digits, `-` and `_`, which nothing escapes. But egress carries any secret the owner grants a destination, and a base64 token or a Basic credential holds `+`, `/` and `=`.
   - This scan is the last line for an egress credential, which never reaches the broker's gates.
2. **A buffer that grew left a copy behind.** `Scrubber::push` appended with `extend_from_slice`, so a reallocation freed the old buffer unwiped. It could hold the upstream's echo, unscrubbed.

## The fix

- **`Forms`**: every form of a credential an upstream may echo, the verbatim one first, each a wiped copy.
  - A credential holding a byte outside URL's unreserved set (letters, digits, `-._~`) also gets these forms, with duplicates dropped:
    - its JSON escape (`\"`, `\\`, `\/`, a control byte as `\u00XX`);
    - each such byte as `\u00XX`, and as `%XX`, each in lower and upper case.
  - A credential with none has only itself, scanned exactly as before.
  - `scrub` and `scrub_text` mask every form. `Scrubber` holds back the longest form's length less one.
- **Nothing held is grown.** Each piece is joined with what's held in one buffer made at the size it needs, scrubbed, and the tail kept in a new buffer made at its size. Each is wiped when dropped, and none is ever reallocated.
- `Scrubber` no longer borrows the secret: it keeps its forms, so its callers are unchanged.

## Tests

In `scrub::tests`:
- `a_credential_with_bytes_that_get_escaped_has_each_escaped_form`:
  - `tok+en/abc==`'s six forms, in order, with the longest one's length;
  - a plain key has itself alone;
  - unreserved bytes aren't rewritten;
  - JSON's quote, backslash and control escapes, and a space left as it is.
- `each_escaped_echo_is_masked`: each form inside a JSON error is masked whole and its length kept, in bytes and in text.
- `each_escaped_echo_split_anywhere_is_found`: each form, split at every point across two pieces.
- `what_is_held_is_never_grown`: after every piece, what's held has exactly its own size, never a grown buffer's.
- The existing tests (verbatim, every split, byte by byte, a clean stream) pass unchanged, as do the harness relay's 39.

## The gate

On the sandbox:

- `cargo fmt --check`, and clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- The whole workspace's tests: 4599 passed, 0 failed. `hatchling-dev audit`: all checks passed.
- **Hand-breaks**, each caught by `scrub::tests`:
  - no escaped forms;
  - only the verbatim form's length held back;
  - what's held reused and grown;
  - one hex case only;
  - no `\/`.
- **Mutation** on the changed lines of `scrub.rs`: 36 mutants, 31 caught, 5 unviable, none missed.
  - The first pass missed one: a control byte's test, `< 0x20`, as `<= 0x20`, which would write a space as `\u0020`.
  - A test now pins that a space stays a space in the JSON form.
