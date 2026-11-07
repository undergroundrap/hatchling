//! Hooks: a task that fires on an event, not on a clock.
//!
//! A hook names its event by a [`HookSource`], a closed set of sources
//! the broker observes itself, never a free-form string. At creation a
//! source must be one the broker can authenticate *for this creator*:
//!
//! - **`ChannelMessage { channel }`**: a channel whose messages arrive
//!   through an authenticated ingress, and that the creator reads.
//! - **`ScheduleRunFinished { schedule_id }`**: a schedule that exists.
//!
//! As for schedules, a hook's grants are a subset of the creator's when
//! it is created, and a run gets only those the creator still holds
//! when it fires.

use std::collections::BTreeSet;

use hatchling_core::{HookSource, ScheduleGrant, ScheduleRefusal};
use serde::{Deserialize, Serialize};

use crate::schedule::{check_grants, check_task};

/// One stored hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hook {
    /// The broker-minted id.
    pub id: String,
    /// The event it fires on.
    pub source: HookSource,
    /// The task a run is given.
    pub task: String,
    /// The grants a run may use, as created.
    pub grants: Vec<ScheduleGrant>,
    /// Where a run's output goes.
    pub channel: String,
    /// Held for the owner: a run it fired was stopped for trying to send a
    /// secret Hatchling keeps. It fires on no event until the owner marks
    /// it resolved. Broker-set; never on the wire.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
}

/// The event sources the broker can authenticate for a creator.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    /// Channels whose messages reach the broker through an authenticated
    /// ingress, and that the creator reads.
    pub channels: &'a BTreeSet<String>,
    /// The ids of the schedules that exist.
    pub schedules: &'a BTreeSet<String>,
}

impl Sources<'_> {
    /// Whether `source` is one the broker can authenticate.
    #[must_use]
    pub fn knows(&self, source: &HookSource) -> bool {
        match source {
            HookSource::ChannelMessage { channel } => self.channels.contains(channel),
            HookSource::ScheduleRunFinished { schedule_id } => self.schedules.contains(schedule_id),
        }
    }
}

/// A hook create, validated against the creator's grants and the
/// sources the broker can authenticate.
///
/// Checks run in a fixed order, so each refusal names the first fault:
/// the task, the grant count, the source, the grants, and the delivery
/// channel.
///
/// # Errors
///
/// Returns the [`ScheduleRefusal`] for the first fault:
/// [`ScheduleRefusal::UnknownSource`] for a source the broker cannot
/// authenticate.
pub fn validate_hook(
    id: String,
    source: HookSource,
    task: String,
    grants: Vec<ScheduleGrant>,
    channel: String,
    held: &BTreeSet<ScheduleGrant>,
    sources: &Sources<'_>,
) -> Result<Hook, ScheduleRefusal> {
    check_task(&task, &grants)?;
    if !sources.knows(&source) {
        return Err(ScheduleRefusal::UnknownSource);
    }
    check_grants(&grants, &channel, held)?;
    Ok(Hook {
        id,
        source,
        task,
        grants,
        channel,
        held: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{MAX_GRANTS, MAX_TASK_BYTES};

    fn send(channel: &str) -> ScheduleGrant {
        ScheduleGrant::ChannelSend {
            channel: channel.to_owned(),
        }
    }

    fn egress(destination: &str) -> ScheduleGrant {
        ScheduleGrant::Egress {
            destination: destination.to_owned(),
        }
    }

    fn message(channel: &str) -> HookSource {
        HookSource::ChannelMessage {
            channel: channel.to_owned(),
        }
    }

    fn finished(id: &str) -> HookSource {
        HookSource::ScheduleRunFinished {
            schedule_id: id.to_owned(),
        }
    }

    fn create(
        source: HookSource,
        task: &str,
        grants: Vec<ScheduleGrant>,
    ) -> Result<Hook, ScheduleRefusal> {
        let held = BTreeSet::from([send("local"), egress("api.example.com:443")]);
        let channels = BTreeSet::from([String::from("local")]);
        let schedules = BTreeSet::from([String::from("sch-1")]);
        validate_hook(
            String::from("hk-1"),
            source,
            task.to_owned(),
            grants,
            String::from("local"),
            &held,
            &Sources {
                channels: &channels,
                schedules: &schedules,
            },
        )
    }

    #[test]
    fn a_hook_on_a_known_source_within_the_creator_s_grants_is_stored() {
        let hook = create(message("local"), "triage", vec![send("local")]).unwrap();
        assert_eq!(
            hook,
            Hook {
                id: String::from("hk-1"),
                source: message("local"),
                task: String::from("triage"),
                grants: vec![send("local")],
                channel: String::from("local"),
                held: false,
            }
        );
        assert!(create(finished("sch-1"), "report", vec![send("local")]).is_ok());
    }

    #[test]
    fn a_source_the_broker_cannot_authenticate_is_refused() {
        for source in [
            // No authenticated ingress, or not a channel the creator reads.
            message("telegram"),
            message("Local"),
            // No such schedule.
            finished("sch-2"),
            finished(""),
        ] {
            assert_eq!(
                create(source.clone(), "t", vec![send("local")]).err(),
                Some(ScheduleRefusal::UnknownSource),
                "{source}"
            );
        }
    }

    #[test]
    fn every_hook_refusal_names_its_fault_in_order() {
        let cases = [
            // The task first: before the source, before the grants.
            (
                create(message("telegram"), " ", vec![egress("evil:1")]),
                ScheduleRefusal::BadTask,
            ),
            (
                create(
                    message("local"),
                    &"x".repeat(MAX_TASK_BYTES + 1),
                    vec![send("local")],
                ),
                ScheduleRefusal::BadTask,
            ),
            (
                create(
                    message("telegram"),
                    "t",
                    vec![send("local"); MAX_GRANTS + 1],
                ),
                ScheduleRefusal::TooManyGrants,
            ),
            // The source before the grants.
            (
                create(message("telegram"), "t", vec![egress("evil:1")]),
                ScheduleRefusal::UnknownSource,
            ),
            // A hook may not hold more than its creator.
            (
                create(
                    message("local"),
                    "t",
                    vec![send("local"), egress("evil.example:443")],
                ),
                ScheduleRefusal::ExceedsGrants,
            ),
            (
                create(message("local"), "t", vec![egress("api.example.com:443")]),
                ScheduleRefusal::ChannelNotGranted,
            ),
        ];
        for (index, (result, refusal)) in cases.into_iter().enumerate() {
            assert_eq!(result.err(), Some(refusal), "case {index}");
        }
        // At the bounds: accepted.
        assert!(
            create(
                message("local"),
                &"x".repeat(MAX_TASK_BYTES),
                vec![send("local")]
            )
            .is_ok()
        );
        assert!(create(message("local"), "t", vec![send("local"); MAX_GRANTS]).is_ok());
    }
}
