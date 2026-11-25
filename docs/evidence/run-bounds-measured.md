# Evidence: WO-051 measured, before and after

The same five tasks, run on the build before WO-051 and the build after it, with two small local models and a large hosted one. Each run was checked on disk, not by what the agent said.

| | |
|---|---|
| Before | `main` at `d0ba8e00` (#648, WO-051's design; no code of it yet): a chat run stops at 25 turns and 10 minutes |
| After | `main` at `28827faf` (#655): slices 2 to 4, the context guard, the clock by kind, the token budget, Stop and the 200-turn cap |
| Also between them | #652: the agent is told its cell's rules, and to stop and say what's missing. Some of the difference may be its |
| Date | 2026-11-24 and 25 (UTC) |
| Box | a fresh install in a throwaway WSL2 distro (Ubuntu 26.04), release builds swapped in place, the Room from v0.9.0 |
| Local models | Ollama on loopback, on the GPU, a 65,536-token window: `qwen3.5:9b` and `gemma4:12b` |
| Hosted model | `nvidia/nemotron-3-ultra-550b-a55b:free` through OpenRouter |

## The tasks

Each is one message to a new conversation, in a folder of its own in the Room's workspace:

| Task | Asked | Checked on disk |
|---|---|---|
| `files30` | write 30 notes, one file each | all 30 files, each with its own line |
| `primes` | write `primes.py` counting primes below 100,000, save the count, write a test and run it | `result.txt` is 9592, and the test passes |
| `edits` | write `lib.py` with ten functions, then give each a docstring, one edit at a time, then import it | ten functions, ten docstrings, the import runs |
| `chain30` | start at 7; 30 steps, each its own run of `python3 -c 'print((X*X+1) % 1000003)'`, each saved to its own file | every step file holds the right value (the last is 711397) |
| `control` | what's 17 × 23, with no tools | the answer is 391 |

`files30`, `edits` and `chain30` need more than 25 turns done step by step, which is what WO-051 is about. `control` needs one.

## Results

✓ is the work done and right on disk. A limit is the run ending on one of Hatchling's bounds: the 25-turn cap or the 10-minute clock before; any of the new bounds after.

### `qwen3.5:9b`, 3 reps each

| Task | Before | After |
|---|---|---|
| `files30` | 2/3 (21 of 30 once) | 3/3 |
| `primes` | 3/3 | 3/3 |
| `edits` | 2/3 (the turn cap once, at 26 calls) | 3/3 |
| `chain30` | 0/3 (the turn cap twice) | 0/3 (no limit) |
| `control` | 3/3 | 3/3 |
| **All** | **10/15, 3 limits** | **12/15, 0 limits** |

### `gemma4:12b`, 3 reps before, 1 after

| Task | Before | After |
|---|---|---|
| `files30` | 1/3 (the turn cap once; once it said it was done with none written) | 0/1 (29 of 30, 31 calls, no limit) |
| `primes` | 2/3 (once no answer from the model in 307 s) | 1/1 |
| `edits` | 3/3 | 1/1 on disk; the run then failed, the model not answering (345 s) |
| `chain30` | 0/3 (the clock once, told at 908 s; the turn cap once; once it couldn't read a command's output) | 0/1 (64 calls, the right last number, no step files) |
| `control` | 3/3 | 1/1 |
| **All** | **9/15, 3 limits** | **3/5, 0 limits** |

One rep after: gemma is slow on this box (an occasional model call takes minutes), and the before reps had shown its pattern.

### `nemotron` (hosted, free), 1 rep each

| Task | Before | After |
|---|---|---|
| `files30` | ✗ the turn cap, 23 of 30 | ✓ 30 of 30, 32 calls, 169 s |
| `primes` | ✓ | ✓ |
| `edits` | ✓ | ✓ |
| `chain30` | ✗ the turn cap, 11 of 30 | ✓ 30 of 30, 63 calls, 218 s, 876,368 input tokens |
| `control` | ✓ | ✓ |
| **All** | **3/5, 2 limits** | **5/5, 0 limits** |

### Turns, time and tokens

Every run's model calls, seconds, input and output tokens and largest request are in the results (kept with the maintainer's notes). In short:

- **Before, the turn cap was the limit met most:** 7 of the 8 limits across 35 runs, at 26 or 27 model calls. The 8th was the clock.
- **After, no run met a limit in 25 runs.** The longest took 64 calls (gemma, `chain30`) and the most tokens 876,368 (nemotron, `chain30`), well inside the 2,000,000-token budget.
- **No request came near a window:** the largest after was 17,250 tokens locally, in a 65,536-token window, and 18,807 hosted.
- **`control` cost the same before and after** (2 calls, about 9,000 input tokens): the new bounds add nothing to a short task.

## What the measurement found

- **A capable model finishes what the turn cap stopped.** Nemotron did both long tasks after, where before it was stopped at 23 of 30 and 11 of 30.
- **A small local model gains less, and fails for other reasons.** Neither 9B nor 12B model did `chain30` before or after. Before, the cap stopped them first. After, nothing stopped them, and they failed anyway: one did the steps in one run against the task's rule, slipped its own file names, or stopped and said it was done, and once the agent was stopped for calling its tools wrongly. That's the models' limit, not a bound's. The behaviour battery is for it.
- **The token budget caught a loop.** In a first round of qwen runs, an `after` `chain30` run looped for 102 model calls and 2,003,773 input tokens, and the budget ended it, as designed. On its way it deleted the other tasks' folders in the workspace, so that round's checks were lost and every qwen rep was run again, each checked as soon as it ended.
- **Stop and the clock waited for a model call in flight** ([#657](https://github.com/undergroundrap/hatchling/issues/657)). Stop 20 s into gemma's long answer was told 49.8 s later; the 10-minute clock, at 908 s. Fixed before this close: told 0.12 to 0.20 s after Stop, and Ollama stops generating ([stop-ends-in-flight-call.md](stop-ends-in-flight-call.md)).
- **A hosted model's rate limit is told as the model being unreachable** ([#656](https://github.com/undergroundrap/hatchling/issues/656)). The free model's daily cap answered `429` after 54 calls, and the owner was told to check the model is running. Open.

## Said plainly

- **Few reps, two machines' worth of noise.** Three reps for the local models, and one for gemma after and the hosted model. The pattern is clear; the percentages aren't precise.
- **Not all of the gain is WO-051's.** #652's prompt change landed between the builds. The limits met before and not after are WO-051's; how well a model does inside them may be both.
- **The measurement's own harness was fixed on the way.** A fresh API connection replays the chat's history, and in one gemma round a late run's reply was read as the next task's. That round was thrown away, and the harness now ignores what it has seen and waits for the Room to be idle before each task.
