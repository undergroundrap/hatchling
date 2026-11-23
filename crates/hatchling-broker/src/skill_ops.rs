//! Broker-side skill operations: the session handler for [`SkillOp`].
//!
//! The broker owns the [`Skills`]; the agent names a skill and a
//! capability, and the broker decides. Every call is checked here, at
//! call time, against the admitted manifest (the 380-class lesson:
//! admission without use-time enforcement is a missing check).
//!
//! Denials are typed replies, never session errors: an unknown skill
//! and an undeclared capability each get their own answer, and the
//! session continues. Each is logged loudly where it is decided.

use hatchling_core::framing::fits_one_frame;
use hatchling_core::{
    CellId, MAX_SKILL_READ_BYTES, SessionResponse, SkillEntry, SkillOp, SkillResult,
};
use hatchling_skill::SkillId;

use crate::skill_invoke::{SkillDenial, check_skill_call};
use crate::skills::{SkillSet, Skills, read_skill_file};

/// Handles one skill op: exact id lookup, call-time capability check,
/// then execution.
///
/// The id is matched exactly (`name@version`); a malformed id names no
/// skill. The capability arrives already typed: a malformed name never
/// decoded into a request.
///
/// A call that passes every check runs in a fresh skill cell (see
/// [`crate::skill_run`]); any failure there is the opaque
/// [`SkillError`](SkillResult::SkillError), with the reason in the
/// broker's log.
///
/// A harness skill (, `claude-code@1`, `codex-cli@1`) is run by the
/// broker's harnesses instead; see [`crate::harness`].
///
/// `keys` reads a real CLI harness's vendor key for its relay.
#[must_use]
pub fn handle_skill_op(
    skills: &Skills,
    op: &SkillOp,
    cell_id: &CellId,
    keys: crate::harness::HarnessKeys<'_>,
) -> SkillResult {
    handle(skills, op, cell_id, keys, true)
}

/// Handles one skill op from a run's session: as [`handle_skill_op`],
/// but a harness's id names no skill (WO-046). A harness spends the
/// operator's vendor key, and its grant was never a run's.
#[must_use]
pub fn handle_run_skill_op(
    skills: &Skills,
    op: &SkillOp,
    cell_id: &CellId,
    keys: crate::harness::HarnessKeys<'_>,
) -> SkillResult {
    handle(skills, op, cell_id, keys, false)
}

/// Handles one skill op, a harness's too when `harnesses`.
fn handle(
    skills: &Skills,
    op: &SkillOp,
    cell_id: &CellId,
    keys: crate::harness::HarnessKeys<'_>,
    harnesses: bool,
) -> SkillResult {
    let (skill, capability, input) = match op {
        SkillOp::List => return list(&skills.snapshot(), cell_id),
        SkillOp::Read { skill, path } => {
            return read(&skills.snapshot(), skill, path.as_deref(), cell_id);
        }
        SkillOp::Invoke {
            skill,
            capability,
            input,
        } => (skill, capability, input),
    };
    if harnesses {
        if let Some(result) = skills.handle_harness(op, keys) {
            return result;
        }
    }
    let Ok(id) = skill.parse::<SkillId>() else {
        tracing::warn!(
            cell_id = %cell_id,
            // The caller chose it: logged escaped, so it can't forge a line.
            skill = ?skill,
            "denied skill op: malformed skill id"
        );
        return SkillResult::UnknownSkill;
    };
    // One snapshot for the whole op: the check and the run see the same
    // set, whatever a concurrent trust-root reload swaps in.
    let set = skills.snapshot();
    match check_skill_call(set.registry(), &id, capability) {
        Ok(()) => match skills.run_in(&set, &id, input, keys.held_hit()) {
            Ok(output) => {
                tracing::info!(
                    cell_id = %cell_id,
                    skill = %id,
                    capability = %capability,
                    bytes = output.len(),
                    "skill ran"
                );
                SkillResult::Output { output }
            }
            Err(err) => {
                tracing::error!(
                    cell_id = %cell_id,
                    skill = %id,
                    capability = %capability,
                    %err,
                    "skill run failed"
                );
                SkillResult::SkillError
            }
        },
        Err(denial @ SkillDenial::UnknownSkill { .. }) => {
            tracing::warn!(cell_id = %cell_id, ?denial, "denied skill op");
            SkillResult::UnknownSkill
        }
        Err(denial @ SkillDenial::UndeclaredCapability { .. }) => {
            tracing::warn!(cell_id = %cell_id, ?denial, "denied skill op");
            SkillResult::CapabilityDenied
        }
    }
}
/// The skills the owner enabled that this set runs, in the owner's
/// listing's order (by name), with what the agent needs to use each. The
/// registry holds only what the owner enabled at its digest now
/// ([`crate::skills::load_skills`]), so a skill it holds is one.
fn enabled(set: &SkillSet) -> Vec<(SkillId, SkillEntry)> {
    set.listing()
        .iter()
        .filter_map(|listing| {
            let id = SkillId::new(&listing.name, &listing.version);
            let manifest = set.registry().get(&id)?.admission().manifest();
            let instructions = manifest.instructions.as_deref();
            let program = manifest.entry_point();
            let entry = SkillEntry {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                description: manifest.description.clone(),
                playbook: instructions.is_some(),
                capabilities: manifest.capabilities().to_vec(),
                files: manifest.files.iter().any(|file| {
                    Some(file.path.as_str()) != instructions && Some(file.path.as_str()) != program
                }),
            };
            Some((id, entry))
        })
        .collect()
}

/// Lists the enabled skills, as many as one frame carries: the rest are
/// counted.
fn list(set: &SkillSet, cell_id: &CellId) -> SkillResult {
    let mut skills: Vec<SkillEntry> = enabled(set).into_iter().map(|(_, entry)| entry).collect();
    let mut more = 0u32;
    loop {
        let reply = SkillResult::Listed { skills, more };
        if skills_fit(&reply) {
            tracing::info!(cell_id = %cell_id, more, "skills listed");
            return reply;
        }
        let SkillResult::Listed {
            skills: mut fewer,
            more: counted,
        } = reply
        else {
            return SkillResult::SkillError;
        };
        if fewer.pop().is_none() {
            return SkillResult::SkillError;
        }
        skills = fewer;
        more = counted.saturating_add(1);
    }
}

/// Whether `result` is a reply one frame carries.
fn skills_fit(result: &SkillResult) -> bool {
    fits_one_frame(&SessionResponse::Skill(result.clone()))
}

/// Reads an enabled skill's playbook, or the file `path` it lists: checked
/// against its signed hash as it is read, and fenced by
/// [`hatchling_core::skill_text::render_read`].
fn read(set: &SkillSet, name: &str, path: Option<&str>, cell_id: &CellId) -> SkillResult {
    let Some((id, _)) = enabled(set)
        .into_iter()
        .find(|(_, entry)| entry.name == name)
    else {
        // The agent chose the name: logged escaped.
        tracing::warn!(cell_id = %cell_id, skill = ?name, "denied skill read: no enabled skill has the name");
        return SkillResult::UnknownSkill;
    };
    let (Some(registered), Some(dir)) = (set.registry().get(&id), set.package(&id)) else {
        return SkillResult::UnknownSkill;
    };
    let admission = registered.admission();
    let manifest = admission.manifest();
    let Some(wanted) = path.or(manifest.instructions.as_deref()) else {
        return SkillResult::NoSuchFile;
    };
    let Some(file) = manifest.file(wanted) else {
        tracing::warn!(cell_id = %cell_id, skill = %id, path = ?wanted, "denied skill read: the skill lists no such file");
        return SkillResult::NoSuchFile;
    };
    if file.bytes > u64::try_from(MAX_SKILL_READ_BYTES).unwrap_or(u64::MAX) {
        return SkillResult::TooLarge { bytes: file.bytes };
    }
    let bytes = match read_skill_file(dir, &file.path, &manifest.name, "listed file") {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(cell_id = %cell_id, skill = %id, %error, "skill read failed");
            return SkillResult::SkillError;
        }
    };
    // What's read is what was signed, now, not only at load.
    if let Err(error) = file.check(&manifest.name, &bytes) {
        tracing::error!(cell_id = %cell_id, skill = %id, %error, "skill file changed since it was loaded: not read");
        return SkillResult::SkillError;
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return SkillResult::NotText;
    };
    let reply = SkillResult::Text {
        text: hatchling_core::skill_text::render_read(
            &manifest.name,
            &manifest.version,
            &file.path,
            &admission.signer_fingerprint(),
            &text,
        ),
    };
    if !skills_fit(&reply) {
        return SkillResult::TooLarge { bytes: file.bytes };
    }
    tracing::info!(cell_id = %cell_id, skill = %id, path = %file.path, bytes = file.bytes, "skill read");
    reply
}
