# WO-057: the real-model trial

Part of [#767](https://github.com/undergroundrap/hatchling/issues/767), [WO-057](../workorders/workorder-057-automatic-memory.md). Run on 2026-11-28 with gemma4:12b on Ollama, in a side install on the maintainer's demo machine: its own state, sockets and port, beside the real install and never touching it.

## The first run found a bug

The build was main at `a927f0fe`, with all three slices merged. In a clean chat, "My dog is called Biscuit and she is a beagle. Please remember that." put a **card** to the owner. The Memory page said memory was Automatic, and nothing waited on the Review page.

**Cause ([#773](https://github.com/undergroundrap/hatchling/issues/773)):** a run's session is served with the run's own configuration (`run_launcher::scoped_config`), and its scheduling is off. So inside a real chat:
- the owner's mode read as Ask me;
- the Review page couldn't be reached.

Every test had built the broker's configuration directly, so none went through `scoped_config`. The fix reads both from the broker's store, named by the run's follow-up scope (`SchedulerConfig::owners()`). It's tested through `scoped_config`.

## With the fix

| Step | What happened |
| --- | --- |
| A clean chat: "My dog is called Biscuit … remember that." | **Kept at once**, no card. The chat said "Remembered: …", with "Kept automatically: nothing in this conversation came from outside", the one-time "Memory is now automatic … Change it in Settings", and "Forget it" under it. The agent said "I'll remember that Biscuit is your beagle." The Memory page listed it as "Kept automatically". |
| The agent asked to read a web page in the same conversation | It used the web-page tool. The page didn't load (the demo's network didn't resolve the name), but any use taints, an error's words included. |
| Then: "By the way, I live in Bath. Please remember that." | **Not kept.** The chat said "Waiting on your Memory page: … This conversation read from outside, so it's kept only if you approve it there." The agent said "I've asked to remember that you live in Bath." No card. |
| The Memory page | The item waited under "Something to remember about you", with why. Approved ("Remember this?"), it was kept at once as "Approved by you", and the chat said "Remembered: …". |
| A new conversation: "I take my tea black … remember that. Also, where do I live?" | Clean again: **kept at once**, and the one-time notice wasn't repeated. The agent answered "you're in Bath". |

## Not shown here

- **Poisoning with a real injected page:** no page could be fetched from the demo's network. What stops it is the conversation-wide taint, set by any outside use, even one that failed, and the trial showed that working. The unit and seam tests cover a run that read and then asked.

## Found along the way

- [#775](https://github.com/undergroundrap/hatchling/issues/775): a cold local model took 1 min 44 s on the first answer, and the chat only said "Asking the model".
- [#776](https://github.com/undergroundrap/hatchling/issues/776): a Memory page already open didn't show an item that started waiting until it was reloaded.
- [#777](https://github.com/undergroundrap/hatchling/issues/777): Stop as an icon button in the composer.
