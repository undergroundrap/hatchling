# Evidence: the quickstart's first task can't race the stand-in model, and a failed one says why (#701)

| | |
|---|---|
| Base | `main` at `324d0348` (#695) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, Python 3.14), as an unprivileged user |
| Found by | CI: "without root" failed on #698 (run 45714207195), and once before on another branch |

## What was wrong

1. **The broker could ask the stand-in model before it listened.** Both quickstart sections ran `python3 docs/examples/stand-in-model.py 18089 &` and started the broker right after it. On the failing run, the agent's first request reached `localhost:18089` 230 ms after the stand-in was started, and was refused: `local model endpoint unreachable`, then the run exited 69 ("the model endpoint failed its capability handshake").
2. **`first-run.py` then waited five minutes, saying nothing.** The broker tells the chat a run failed as a `chat_message` from `system`. `first-run.py` waited only for one from the agent, and timed out with no reason given.

## What it does now

1. `stand-in-model.py --detach` binds its port, then returns and serves from a child. Both README sections use it, so the broker starts only once the model can answer. A port already in use fails the command. Without `--detach` it behaves as before, so CI's own launchers, which start it with `&` and stop it by its pid, are unchanged.
2. `first-run.py` prints a `system` message that comes after its task was accepted, as `hatchling: <text>`, and exits 1 at once. A `system` message before that is the conversation the broker kept, replayed at the start of the stream, and isn't about this task: it's passed over, as is any message before the task was accepted.

## Tests

Retriggered before and after, with the files from `main` ("old") and from this branch ("new"):
- **The race:** 20 times each, the stand-in is started and its port is tried at once, with no retry, as the broker would.
- **`first-run.py`:** run against a fake local API that replays a kept conversation's `system` message, accepts the task, then either says the run failed (`system`) or answers (`agent`). Each run is held to 10 seconds.

```text
Python 3.14.4
old (& then the broker at once): refused 20 of 20
new (--detach): refused 0 of 20
second --detach on a used port: exit 1
old first-run.py, run failed: exit 124 after 10s; last line: (none)
new first-run.py, run failed: exit 1 after 0s; last line: hatchling: The model couldn't be reached.
old first-run.py, answered: exit 0 after 0s; last line: agent: Hello from the fake.
new first-run.py, answered: exit 0 after 0s; last line: agent: Hello from the fake.
done
```

- The old order lost the race every time; `--detach` never did.
- The old `first-run.py` hung on a failed run until it was killed (124), having printed nothing; the new one stopped at once with the broker's reason.
- Both answer when the agent answers, and the new one wasn't stopped by the replayed `system` message before its task was accepted.

No stand-in or fake process was left running afterwards. CI's README quickstart runs both sections with these files, root and without root.
