# Evidence: the quickstart's first task can't race the stand-in model, and first-run.py answers for its own task (#701)

| | |
|---|---|
| Base | `main` at `324d0348` (#695) |
| Date | 2026-11-26 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, Python 3.14), as an unprivileged user |
| Found by | CI: "without root" failed on #698 (run 45714207195), and once before on another branch. The first fix's own CI run found the third problem ("upgrade from the last release" on #702) |

## What was wrong

1. **The broker could ask the stand-in model before it listened.** Both quickstart sections ran `python3 docs/examples/stand-in-model.py 18089 &` and started the broker right after it. On the failing run, the agent's first request reached `localhost:18089` 230 ms after the stand-in was started, and was refused: `local model endpoint unreachable`, then the run exited 69 ("the model endpoint failed its capability handshake").
2. **`first-run.py` then waited five minutes, saying nothing.** The broker tells the chat a run failed as a `chat_message` from `system`. `first-run.py` waited only for one from the agent, and timed out with no reason given.
3. **After a restart, `first-run.py` took an earlier answer for its own.** A broker with a conversation store replays the conversation it kept at the start of the stream, and that can reach the client after its task was accepted. `first-run.py` printed the first agent message it saw, so after a restart it reported the *replayed* answer and exited 0, whether its own run answered or failed. CI's upgrade job runs `first-run.py` after restarting the broker, and that check passed without a run answering it.

   The first fix here (a `system` message after the task was accepted ends it) met that replay on the upgrade job: it stopped at "Kept from before the broker restarted", which isn't about its task.

## What it does now

1. `stand-in-model.py --detach` binds its port, then returns and serves from a child. Both README sections use it, so the broker starts only once the model can answer. A port already in use fails the command. Without `--detach` it behaves as before, so CI's own launchers, which start it with `&` and stop it by its pid, are unchanged.
2. `first-run.py` takes the task's own message, as the stream shows it (the `message_id` its acceptance names), as the start of its part of the chat. Everything before it is earlier and passed over, the replay included, however late it arrives. After it:
   - the agent's message is the answer: printed, exit 0;
   - a `system` message is Hatchling's word on the run, as when it failed: printed as `hatchling: <text>`, exit 1 at once.

   Its message can also come before its acceptance, and that's handled too.

## Tests

Retriggered before and after, with the files from `main` ("old") and from this branch ("new"):
- **The race:** 20 times each, the stand-in is started and its port is tried at once, with no retry, as the broker would.
- **`first-run.py`:** run against a fake local API ordered as the upgrade job's broker was:
  1. the task accepted;
  2. a kept conversation replayed: a `system` message, an earlier task and that task's answer;
  3. the task's own message;
  4. then either the run's failure (`system`) or its answer (`agent`).

  It's run again with the task's own message before its acceptance. Each run is held to 10 seconds.

```text
Python 3.14.4
old (& then the broker at once): refused 20 of 20
new (--detach): refused 0 of 20
second --detach on a used port: exit 1
-- the kept conversation replayed, the task's own message late
old first-run.py, run failed: exit 0 after 0s; last line: agent: Hello from before the restart.
new first-run.py, run failed: exit 1 after 0s; last line: hatchling: The model couldn't be reached.
old first-run.py, answered: exit 0 after 0s; last line: agent: Hello from before the restart.
new first-run.py, answered: exit 0 after 0s; last line: agent: Hello from the fake.
-- the kept conversation replayed, the task's own message early
old first-run.py, run failed: exit 0 after 0s; last line: agent: Hello from before the restart.
new first-run.py, run failed: exit 1 after 0s; last line: hatchling: The model couldn't be reached.
old first-run.py, answered: exit 0 after 0s; last line: agent: Hello from before the restart.
new first-run.py, answered: exit 0 after 0s; last line: agent: Hello from the fake.
done
```

- **The race:** the old order lost it every time, and `--detach` never did.
- **After a restart:** the old `first-run.py` reported the earlier answer as its own and exited 0, even when its run failed. The new one reports its own run in both orders: its answer, or its failure with the broker's reason, at once.

No stand-in or fake process was left running afterwards. CI's README quickstart runs both sections with these files, root and without root, and its upgrade job runs `first-run.py` after a restart.
