# Scorecard

What Hatchling really does on a fresh install, one row at a time.

**The rule:** a row is ✅ only when a person did it on a fresh install. Code, protocol names and passing tests don't count, however complete they look. This page is updated for every release, and a release that changes a row has to prove it again.

| Mark | Means |
|---|---|
| ✅ | A person did it on a fresh install |
| 🔵 | An agent drove the real app on a fresh install, and no person has yet |
| ⬜ | Built, not yet tried on a fresh install |
| 🚧 | Built, but off by default, or part of it is missing |
| ❌ | Not there |

**Last checked:** Windows setup, 2026-11-06, ahead of 0.8.0. Linux installs are covered by CI, which isn't a person, so their rows wait for someone to do them.

## Install and vault

| What | Status | Notes |
|---|---|---|
| Install with the Windows setup | ✅ | |
| Update with the Windows setup | ⬜ | Fresh run due before 0.8.0 |
| Uninstall, keeping or deleting data | ✅ | |
| Protect the vault with a security key | ✅ | Needs usbipd by hand |
| Unlock after a restart | ✅ | |
| Make a backup | ✅ | |
| Restore a backup | ⬜ | |
| Distro sealed from Windows | ⬜ | Tested by setup run, not yet by a person |

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
| Memory, with your approval | ⬜ | No daily notes or import |
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
| Backup and restore drill | ⬜ | |
| Export and import drill | ❌ | Import memory isn't there |

## Your accounts

| What | Status | Notes |
|---|---|---|
| Connectors | ❌ | |
| Paired devices | 🚧 | In the broker, with no client |
| Notifications | ❌ | |
| Wallet and payments | ❌ | Waits for a confirmation rail in the browser |

## Private threat models

Some threat models stay private while they describe open gaps, and each is published, sanitised, with the release that closes them. They're counted here so none is forgotten.

| Model | Open questions |
|---|---|
| The agent's computer | 1 |
| Memory | Not yet written |
