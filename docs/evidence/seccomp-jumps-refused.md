# Evidence: a seccomp filter too long for its jumps is refused (#679)

| | |
|---|---|
| Base | `main` at `3b2b82e2` (#678) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Found by | an outside model's review of `seccomp.rs`, confirmed by measuring every profile |

## What was wrong

`assemble()` compiles a profile to classic BPF. A conditional jump's offsets are eight bits, and `jump_offset` cast with `as u8`. A jump of 256 or more wrapped, silently, and `install()` checked only the program's total length. The second instruction, `if arch != AUDIT_ARCH goto kill`, jumps to the last, so its offset is the program's length less 3. Measured on `main`:

| Profile | Instructions | The architecture check's jump |
|---|---|---|
| agent | 68 | 65 |
| harness | 88 | 85 |
| harness_cli | 177 | 174 |
| browser | 194 | 191 |
| room | 208 | 205 |

About 25 more rules on the Room's profile would have wrapped it. A wrong-architecture call, on x86_64 a 32-bit `int 0x80`, would then have fallen into a scan of x86_64 numbers, which name other syscalls there. Every test would have stayed green. No profile shipped has reached it.

## What it does now

- **Refused, not wrapped.** `jump_offset` and `jump_distance` return `None` for a jump that isn't forward or doesn't fit, and an unplaced label is an error rather than position 0. `assemble`, `build` and `install` return the error, so a profile that can't be compiled exactly is never installed and its cell doesn't start. The layout is unchanged.
- **aarch64 fails loudly.** `harness()`, `harness_cli()` and `browser()` add x86_64-only syscalls, so elsewhere they were the agent's profile under another name. They now return an error there (`verified`). Releases are x86_64.
- **`prlimit64`'s pid is documented.** It isn't checked: a cell's processes share its uid, as with `setpriority`.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | GATE |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | GATE |
| the whole workspace's tests | GATE |
| `hatchling-dev audit` | GATE |

New tests:
- `every_profiles_jumps_land_where_they_should`: every profile's program keeps each jump inside the program, and its architecture check lands on the kill.
- `a_program_too_long_for_its_jumps_is_refused`:
  - 252 allowed calls build a program of 258 instructions, whose check jumps 255, the most it can;
  - 253 are refused;
  - so are a 400-call profile's `build` and `install`.
- `jump_offset_is_target_minus_next`, extended: 255 fits and 256 doesn't, and a jump backward, or one at the end of the index range, is `None`.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The eight-bit check removed (the offset masked to a `u8`, as before) | GATE |
| A jump that doesn't fit taken as 0, not refused | GATE |
| The architecture check's failure sent to allow, not kill | GATE |

**Mutation**, on the lines changed against `3b2b82e2`:

GATE
