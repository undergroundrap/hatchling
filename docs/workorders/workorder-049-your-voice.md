# WO-049: Your voice: talk with the same agent, with speech models on your machine

**Status:** QUEUED
**Area:** `frontend/` (the call, the microphone, playback, the avatar), `crates/hatchling-host` and `crates/hatchling-broker` (a voice channel; the speech models' install, admission and runs), `crates/hatchling-channel`, the installer and Windows Setup (fetching the models when voice is turned on), `docs/`
**Issue:** [#610](https://github.com/undergroundrap/hatchling/issues/610)
**Trust boundary:** yes

## Problem

Hatchling is reached only by typing. People talk to an agent they live with: on the move, hands busy, or because speaking is simply easier. Hatchling has no voice of any kind, and nothing reserves a place for it.

Voice isn't a second agent. A spoken request is the owner's words, and the answer is the agent's words read aloud. Everything the agent is (its conversation, memory, tools, grants and approvals) stays as it is, and only the ends change: a microphone instead of a keyboard, a speaker beside the screen.

The custody question is what makes it Hatchling's. A person's voice is among the most identifying things they produce, and sending it to a hosted speech service would put it outside everything Hatchling keeps on the owner's machine. So voice runs on the owner's machine, or not at all.

## Scope

**In scope, design and threat model first, approved by the maintainer before code:**

- **A channel, not an agent.** Speech is transcribed and enters the conversation as the owner's message, with the same standing as typed words; the answer is the agent's usual text, spoken. The run can't tell how its task arrived, and nothing about the agent changes for a call.
- **Local speech models, fetched when the owner turns voice on.** One speech-to-text model and one text-to-speech model, exact builds pinned by hash, with their own SBOM and licence record, fetched and verified only when the owner turns voice on, never in the base install. A small set of voices to choose from in settings.
- **On the owner's side of the wall.** The microphone and the models run where the app and the broker are, never in the Room: the owner's voice is never the agent's computer's content. Audio is transcribed and dropped; only the transcript is kept, as any message is. No audio is kept anywhere unless the owner is asked and chooses it.
- **Approvals stay on the screen.** A call can read an approval card aloud, and a spoken *yes* can at most bring the card forward; the card's tap, bound to the exact action shown, is still the approval. Voice never authorizes anything, and a voice that isn't the owner's can't either.
- **A call that feels like talking.** A Call button in the app; hands-free, with mute, and ending the call by saying so; the agent keeps its usual manner; the transcript is the conversation.
- **The avatar, driven by what's true.** It shows the agent's real state (idle, listening, working, speaking) from the broker's events, never a loop that pretends; its mouth follows the spoken audio's level. Original art made for Hatchling.
- **One feature, landing whole:** call in, the answer spoken, the avatar; not released in pieces.

**Not in scope:** telephone calls; a phone number; video; a wake word or a microphone that listens outside a call; hosted speech services; reading the owner's screen during a call.

## Acceptance criteria

- [ ] The design and threat model are written and approved by the maintainer before any code lands, covering at least: who can speak to the agent through a call (any voice in the room), what a transcript may carry, the models' supply chain, and the microphone's permission and indicator.
- [ ] A spoken request and the same typed request give the run identical input; a test compares them.
- [ ] No audio is written to disk during or after a call, and none reaches the Room; a test watches both.
- [ ] A spoken *yes* to an approval doesn't approve it; only the card does; a test says yes and checks the decision is still pending.
- [ ] The speech models are admitted only when their hashes match the pins, and voice is off until the owner turns it on; tests cover a changed model and a fresh install.
- [ ] The avatar's states follow the broker's events in a test, and the mouth follows the audio level.
- [ ] A real call on a fresh install, on Windows and Linux: speak, hear the answer, mute, end by saying so; recorded in evidence.
- [ ] Each new guard fails its test when removed.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- On v0.12.0, before the first connectors (v0.13.0), by the maintainer's decision of 2026-11-22: voice runs entirely on the owner's machine, while connectors wait on the credential design (#604), which is written during v0.12.0.
