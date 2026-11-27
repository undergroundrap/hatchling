# WO-056 and WO-055, tried with a real model

Closes [#535](https://github.com/undergroundrap/hatchling/issues/535) (WO-056), with the live check of WO-055's batch card ([#753](https://github.com/undergroundrap/hatchling/issues/753), closed).

## How

- **Where:** on 2026-11-27, with the maintainer's yes, on the demo distro, in a side install of its own. It had its own state, sockets, vault and port, under `/var/tmp/hfr`. The real install was never restarted or touched, and the side install was removed afterwards.
- **Built from:** main at `36f7864c` with #763 (slice 3) and #765 (the creator line) merged, as release binaries in the sandbox. Each staged file was checked by SHA-256 on the demo.
- **The model:** `gemma4:12b` on the demo's Ollama, chosen in the app's Settings, with a 64K window.

## What happened

| Asked | Seen |
| --- | --- |
| "Who are you, who made you, and which model are you running on right now?" | "I'm Hatchling, made by Ocean Bennett, running right now on gemma4:12b." |
| The ring after that answer | 13%. Its name: "8.3K of 64K tokens of context (13%) used on the last answer. Show what filled it." The chip: `gemma4:12b`. |
| The panel | "13% used · 8.3K of 64K". Conversation about 23, Tools about 2.1K, Memory about 0, System about 6.2K, Free 57.2K. |
| "Please remember three things about me: …Pepper is a cat …metric, because I live in Spain …I don't have a car" | One card, "Remember these 3 things?", with three items and a Keep box on each. The agent said they'd be saved if approved. |
| The third unticked | The button: "Keep 2". Approved: "Remembered: … Pepper, is a cat." and "Remembered: … metric …", the two alone. |
| "Please remember that I don't have a car." | Asked again, as "The user does not have a car.": other words than the skipped one, so a different memory. See below. |
| A new conversation: "a weekend trip about 5 miles from home with my pet. How should I get there, how far is that, and what should I pack for Pepper?" | "about 8 kilometers away (5 miles) and don't have a car, walking … public transport … check if they allow cats". A carrier and a litter kit. |
| The panel after that | 14%. Memory about 475, System about 6.5K, Tools about 2.2K, Conversation about 40. |
| Each answer's line | "Used: Recalled your memory" (#749's fix), and "Asked to remember something". |

## Findings

- **A skip refuses those words, not the idea.** Fingerprints are of the exact memory, so asked again in other words, a memory is put to the owner again. That's right when the owner asks for it themselves, as here, but the CHANGELOG and docs said "never asked again". This PR says "in those words" everywhere it was said.
- **The instructions are most of a first prompt:** about 6.2K of 8.3K tokens. That's fine in 64K and worth trimming before 32K windows, and the panel now shows it.
- **Message actions' names carry raw message ids** ("Reply to message msg-e275…"), for the app's next accessibility pass.
