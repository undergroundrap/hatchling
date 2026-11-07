# Scorecard

What Hatchling really does on a fresh install, one row at a time.

**Parity means the core agent:** talk, remember, schedule, tools, its own computer, files, skills, the browser with its rail, and the vault. Surfaces and catalogue breadth, such as phone apps, messaging platforms, a large connector library, media and payments, aren't parity criteria unless a phase lists them. Breadth is a roadmap, not a debt.

**The rule:** a row is ✅ only when a person did it on a fresh install. Code, protocol names and passing tests don't count, however complete they look. This page is updated for every release, and a release that changes a row has to prove it again.

| Mark | Means |
|---|---|
| ✅ | A person did it on a fresh install |
| 🔵 | An agent drove the real app on a fresh install, and no person has yet |
| ⬜ | Built, not yet tried on a fresh install |
| 🚧 | Built, but off by default, or part of it is missing |
| ❌ | Not there |

**Last checked:** Windows setup, 2026-11-07, ahead of 0.8.0. Linux installs are covered by CI, which isn't a person, so their rows wait for someone to do them.

## Install and vault

| What | Status | Notes |
|---|---|---|
| Install with the Windows setup | ✅ | |
| Update with the Windows setup | ✅ | On a key-protected install, with a backup first |
| Uninstall, keeping or deleting data | ✅ | |
| Protect the vault with a security key | ✅ | Needs usbipd by hand |
| Unlock after a restart | ✅ | 6 steps on Windows today: the target is one gesture |
| Secure store: see what's kept, by name | ❌ | From a names-only listing by the keeper |
| Add a key or a password from the app's Add | ❌ | Terminal commands for now: an interim, not a pass |
| Make a backup | ✅ | |
| Restore a backup | ⬜ | |
| Distro sealed from Windows | ⬜ | Tested by setup run, not yet by a person |
| The author's fresh install, end to end | ✅ | Install, protect, update, unlock, chat |
| A stranger's fresh install, end to end | ⬜ | Someone other than the author |
| The full vault flow: unlock, add a key and a site password, the agent uses both, lock, unlock, both still work | ❌ | Needs a way to add them and the browser on |

## Talking to it

| What | Status | Notes |
|---|---|---|
| Main chat | ✅ | |
| Stop a running turn | ❌ | |
| Side chats | ❌ | |
| Search | ⬜ | Over what the app has loaded |
| Choose or switch the model | ⬜ | |

## What it keeps

| What | Status | Notes |
|---|---|---|
| Memory, with your approval | 🔵 | Kept, then recalled in a new conversation. No daily notes or import |
| Forget a memory | ⬜ | |
| Goals: add, edit, close | ⬜ | No progress log, briefings or subgoals |
| Scheduled work, with your approval | ⬜ | No pause, edit or run now |
| Feed: a daily post, with your prompt | ⬜ | From goals and memory |
| Ideas: suggest and dismiss | ⬜ | Starting one only fills the chat box |
| Activity and Approvals | ⬜ | In the app only, no notifications |

## What it can do

| What | Status | Notes |
|---|---|---|
| Subagents | ⬜ | |
| A computer of its own | ❌ | Next: 0.9. v1 has no network or installs, and workspace programs don't execute |
| Read files you attach | ❌ | Next: 0.9 |
| Give you files, in chat and the Library | ❌ | Next: 0.9 |
| Documents: markdown and plain HTML | 🚧 | Builders aren't installed, and no file reaches you |
| Other document types | ❌ | |
| Read the web, and search | 🚧 | Off until you set it up |
| Browser you can watch and take over | 🚧 | Off until you set it up, and Chromium isn't installed |
| Saved sign-ins for the browser | 🚧 | Needs the browser |
| Signed skills | 🚧 | None ship |
| Coding harnesses | 🚧 | Not reachable from chat |
| Deep research | 🚧 | Nothing in the app starts it |
| From chat, the agent launches a coding harness in a cell and returns a patch for review | 🚧 | An option, unsized and uncommitted |
| Web artifacts | ❌ | |
| Images, video, audio | ❌ | |

## Proof and trust

| What | Status | Notes |
|---|---|---|
| Where each memory came from (provenance) | ❌ | Memory items hold no source yet |
| Taught-it-yesterday: three facts survive to a fresh session | ⬜ | |
| Receipts for every action | ⬜ | Coverage not yet measured |
| Scheduled runs all accounted for | ⬜ | |
| Per-model conformance, published | ❌ | |
| Benchmark: the same tasks and model on Hatchling's loop and an external harness, failures included | ❌ | Includes a non-coding multi-step job. Cited only with conformance: a harness multiplies a model, it doesn't replace one |
| Backup and restore drill | ⬜ | |
| Export and import drill | ❌ | Import memory isn't there |

## Your accounts

| What | Status | Notes |
|---|---|---|
| Connectors | ❌ | |
| Paired devices | 🚧 | In the broker, with no client |
| Notifications | ❌ | |
| Wallet and payments | ❌ | Waits for a confirmation rail in the browser |

## Open findings

| Finding | Status |
|---|---|
| A held secret in a file given to a harness could reach its AI vendor | Fixed in 0.8.0 at intake |
| The Keep sign-in form took a password in the web app | Fixed in 0.8.0: the form is gone, and sign-ins are added at the terminal |
| Signing in by Take control sends what you type through the app as keystrokes | Open, for the keeper's own window |
| Memory, goals, feed posts, decisions, ideas, run specs and harness temp files aren't sealed at rest | Open. Continuity doesn't ship onto these |
| A default install's vault opens from a key file on the same disk | Open, for the front-door release |
| Authenticator codes don't protect a stolen disk, and there's no passphrase option | Open, for the front-door release |
| A saved sign-in password enters the browser's cell at fill time, for the origin the cell reports | Accepted for now, with a design-out question in Phase 4 |

## Private threat models

Some threat models stay private while they describe open gaps, and each is published, sanitised, with the release that closes them. They're counted here so none is forgotten.

| Model | Open questions |
|---|---|
| The agent's computer | 1 |
| Memory | Not yet written |
