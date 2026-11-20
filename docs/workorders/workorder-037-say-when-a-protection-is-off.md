# WO-037: Say when a protection is off

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-broker` (`doctor`, the setup and security reads), `frontend/` (the Your security page), `docs/your-security.md`
**Issue:** [#479](https://github.com/undergroundrap/hatchling/issues/479)
**Trust boundary:** yes (what the owner is told about their own protection)

## Problem

Several of Hatchling's protections can be absent, or weaker than the default, without either place the owner looks saying so. `doctor` already says plainly whether cells are confined by a cgroup or by process limits alone; other protections don't get the same: a syscall filter not in force, a disk that isn't sealed, a vault at the key file tier, a Room image that isn't installed, a browser cell started without its own profile. An owner who reads "Your security" should be able to trust that anything missing is named there.

## Scope

**In scope:**

- A pass over every check `doctor` makes and every claim the Your security page and `docs/your-security.md` make, against the code that enforces each protection.
- A finding for every protection that can be off, weaker or unknown without either place saying so, each with where the owner would look.
- Words that fix a finding go in this WO: `doctor`'s lines, the page's text, the doc.

**Not in scope:** a fix that changes what a protection does; each such finding becomes its own issue or work order, linked from the evidence.

## Acceptance criteria

- [ ] The evidence lists each protection checked, where it's enforced (file and function), and what `doctor` and the page say when it's on, weaker, off or unknown.
- [ ] Every finding is fixed in words here, or has its own linked issue.
- [ ] For each protection that can be off, a test makes it off and checks `doctor` says so in plain words.
- [ ] Nothing on the Your security page claims a protection the install doesn't have.

## Gates

As [TEMPLATE.md](TEMPLATE.md), with the frontend gate.

## Evidence

- Slice 1a, `doctor` says when a protection is off or weaker: [doctor-says-off.md](../evidence/doctor-says-off.md)

The rest is filled in at close.
