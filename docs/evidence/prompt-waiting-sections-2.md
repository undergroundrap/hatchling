# Evidence: the rest of the sections waiting for their runs (#714)

| | |
|---|---|
| Base | `main` at `9f8fcf69` (#713), with #715 and #716 under it |
| Date | 2026-11-26 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Issue | [#714](https://github.com/undergroundrap/hatchling/issues/714), its last part |

## What it does

The last 34 sections go in, and the pack has 122. None of them reaches a prompt: every kind's prompt is byte for byte what it was. Each section is pinned, with its domain, and held out of every prompt by the waiting check until its run exists.

| Group | Sections | Bytes |
|---|---|---|
| The Ideas run | `ideas-pipeline`, `ideas-research`, `ideas-plan`, `ideas-draft`, `ideas-rate`, `ideas-style`, `ideas-capabilities`, `ideas-feasibility` | 30284 |
| The Ideas domain playbooks | `ideas-domain-health`, `ideas-domain-career`, `ideas-domain-finance`, `ideas-domain-family`, `ideas-domain-shopping`, `ideas-domain-relationships`, `ideas-domain-creation`, `ideas-domain-learning`, `ideas-domain-productivity`, `connector-connect-devices` | 30898 |
| Connectors | `connector-inventory`, `connector-action-surface`, `connector-credentials`, `connector-setup-flow` | 7026 |
| Sharing lessons off the machine (no publishing path) | `egress-generalize`, `egress-judge` | 5529 |
| Signals at the end of a background run | `proactive-notifications`, `serendipity-signals` | 1948 |
| Goal creation | `goals-create`, `goals-chat-rules` | 2650 |
| The rest of the chat rules | `relationships-chat`, `self-evolution`, `credentials-reminder`, `artifact-routing` | 3826 |
| Small runs | `artifact-inspection-repair`, `voice-worker` | 706 |

## Adapted to Hatchling

Four groups of lines changed, and the sections otherwise keep their wording:
- **The maintainer's defaults (2026-11-26):**
  - a credential or card is filled by the vault, and nothing pasted in chat is typed into a site;
  - every order needs the owner's yes;
  - nothing is done on the owner's silence;
  - installing new guidance or skills is proposed for the owner to approve.
- **Capabilities stated as they are:**
  - the agent doesn't generate images or video;
  - its own computer reaches only what the owner granted, so it can't wire an integration up by itself;
  - the browser works once the owner has turned it on and allowed the site;
  - a schedule is one the owner approved.
- **Hatchling's names:** the Library, the skills the owner enabled (there's no catalog) and the vault.

| Section | Was | Now | Why |
|---|---|---|---|
| `connector-credentials` | - When your parent relays the user's choice of transient use for this sign-in and the assigned task either supplies the credentials or explicitly authorizes retrieving them, enter only those values with ordinary fill or type in the intended site's visible username or password controls. | - A password or card the user pasted in chat isn't typed into a site: ask your parent to have the user keep it in the vault, which fills it for this sign-in. | maintainer, 2026-11-26: vault-only on credentials; nothing typed from chat |
| `connector-credentials` | Use transient credentials only for this sign-in. If the site rejects them, report the rejection without their values | If the site rejects a credential the vault filled, report the rejection without its value | the same: no transient credentials |
| `connector-credentials` | from the Secure Vault, | from the vault, | Hatchling's vault |
| `connector-connect-devices` | - **Beyond the catalog:** when no connector skill exists, the agent can research the best integration path itself (an official API, an export the user can schedule, a community CLI it installs and configures), and devices on the user's own network can be reachable over the private network. | - **Beyond the skills:** when no skill exists, the agent can research the best integration path itself (an official API, an export the user can schedule, a community CLI), and the card names the grant or setup the user would give it: the agent's own computer reaches only what the user granted. | there's no skill catalog, and the agent's computer has no network of its own |
| `connector-connect-devices` | the connector-skill catalog, | the skills the user enabled, | there's no catalog |
| `connector-connect-devices` | order the missing hub, cable, or sensor that finishes the setup; | propose ordering the missing hub, cable, or sensor that finishes the setup, for the user's yes; | maintainer, 2026-11-26: money always needs the owner's yes |
| `connector-setup-flow` | making a payment with a secure wallet, | making a payment, | Hatchling has no wallet |
| `connector-setup-flow` | - For stored API access without a connector, use the secure API-access flow and give the user the returned link. After setup succeeds, author the connector doc that calls the API. | - For stored API access without a connector, the user puts the key into the vault themselves, and the broker uses it for the calls through a grant. After setup succeeds, author the connector doc, which names the grant and never the key. | vault-only: the agent never handles a key |
| `connector-setup-flow` | - When neither the skill list nor the catalog has a skill for the requested service, | - When your skill list has no skill for the requested service, | there's no catalog |
| `ideas-capabilities` | - **It drives a real web browser.** Sites with no API are still reachable: | - **It drives a real web browser,** once the user has turned it on and allowed the site. Sites with no API are still reachable: | Hatchling's browser is off until the owner turns it on (#600), and each site is allowed |
| `ideas-capabilities` | - **It runs on a schedule.** | - **It runs on a schedule the user approves.** | a schedule is proposed and the owner approves it |
| `ideas-capabilities` | - **It reads and writes files, generates images, and searches the web.** | - **It reads and writes files, and searches the web.** | Hatchling doesn't generate images |
| `ideas-capabilities` | - **It connects to the user's accounts and devices** through connector skills, and where no skill exists, it can research the best source, install a CLI or call an API, and wire the integration up itself. | - **It reaches the user's accounts and services** through the skills and grants the user gives it. Where none exists, it can research the best source and build what the integration needs, and the card names the grant or setup the user would give it. | no connectors yet (v0.14), and the agent's computer reaches only what's granted |
| `ideas-research` | the skill catalog (search it to check what a section's likely builds would lean on) | the skills the user enabled (list them to check what a section's likely builds would lean on) | there's no catalog |
| `ideas-domain-health` | order the thing the routine actually needs, inside the user's limit. | order the thing the routine actually needs, inside the user's limit and with their yes. | money always needs the owner's yes |
| `ideas-domain-family` | order the supplies the week actually needs; | order the supplies the week actually needs, with the user's yes; | money always needs the owner's yes |
| `ideas-domain-creation` | Image, video, and avatar generation are always in hand, so a generated asset needs nothing connected to start. | What the agent can make with code in its own computer (charts, documents, data, small tools) needs nothing connected to start; it doesn't generate images or video yet. | Hatchling doesn't generate images, video or avatars |
| `ideas-domain-creation` | order the print, the merch, or the materials the project needs; | order the print, the merch, or the materials the project needs, with the user's yes; | money always needs the owner's yes |
| `ideas-domain-learning` | order the materials the next step needs; | order the materials the next step needs, with the user's yes; | money always needs the owner's yes |
| `ideas-domain-finance` | move the recurring payment or update the card on file where the user has authorized it. | move the recurring payment or update the card on file, each time with the user's yes, the card filled from the vault. | money always needs the owner's yes, and the vault alone touches a card |
| `ideas-domain-shopping` | and hand them back so the answering agent can render native shopping cards, following the shopping skill's own card-rendering path: the final return names the resolved results file and the product ids to show in order, plus a short linked list (name and link per pick) so the picks still come through where a card can't render. | and hand them back as a short linked list (name and link per pick), with the full results as a file in the Library when there are more than a few. | the app has no shopping cards; files reach the owner through the Library |
| `ideas-domain-productivity` | over time, items that always get approved start resolving a day early); | over time, items that always get approved start arriving prepared a day early); | maintainer, 2026-11-26: nothing acts on the owner's silence |
| `ideas-domain-productivity` | "handling it now unless you stop me" on the day, | prepared to one tap and asked for on the day, never done on silence, | the same: nothing acts on silence |
| `credentials-reminder` | Use payments and wallet for payment methods, card handling, and wallet setup. | Payment cards are kept and filled by the vault, never handled by you. | no wallet; the vault alone touches a card |
| `artifact-routing` | goes through the artifact tools. | is handed to the user through the Library (give_file, give_folder). | Hatchling delivers through the Library |
| `self-evolution` | creating skills, | proposing skills, | skills are signed and the owner enables them; installing new guidance goes to the owner's review queue (maintainer, 2026-11-26) |
| `self-evolution` | and create and improve your skills. | and propose new skills and improvements to them, which the user approves. | the same |

**Kept as written, for review when each run is built:**
- **The connector flows:** a connect link, an accounts center, a status check. That's v0.14's design to settle.
- **Fleet lessons, read** as other agents' hypotheses by the Ideas and audit runs. Nothing publishes: there's no path off the machine, and none until that has a design and a threat model.
- **The place signals' phone watcher.** Location gets its own threat model before anything is built.
- **The goal-creation contract's category guides,** and the Ideas cards' product icon.

**To merge when they're routed:** some waiting sections say nearly the same as each other.
- `goals-chat-rules` and `goals-tracking-chat`;
- `relationships-chat` and `relationships`;
- `connector-credentials` and `browser-security`'s credential paragraph.

The run that routes them keeps one.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | TESTS |
| `hatchling-dev audit` | all checks passed |

**Hand-breaks**, each caught and the tree restored:
- the finance playbook routed into Chat: caught by the waiting check;
- the productivity playbook's text changed but not its pin: caught by the text pin;
- `credentials-reminder` taken off the waiting list: caught as orphaned.

**Mutation:** no mutants; the change is the pack's text and its tests.
