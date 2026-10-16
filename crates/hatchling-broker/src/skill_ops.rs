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

use hatchling_core::{CellId, SkillOp, SkillResult};
use hatchling_skill::SkillId;

use crate::skill_invoke::{SkillDenial, check_skill_call};
use crate::skills::Skills;

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
    if let Some(result) = skills.handle_harness(op, keys) {
        return result;
    }
    let SkillOp::Invoke {
        skill,
        capability,
        input,
    } = op;
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
        Ok(()) => match skills.run(&set, &id, input) {
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
