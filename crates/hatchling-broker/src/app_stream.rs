//! What the scheduler tells the owner's app: each scheduled run's start and
//! end, as the app's activity shows it.
//!
//! The scheduler's loop has no stream of its own: the API owns the app's.
//! As with the approval board, each broker has one of these, found by its
//! vault's root; the API attaches its stream when it starts serving, and
//! until then nothing is sent. What's sent is the broker's own record (a
//! schedule's id and how its run went), never what a run wrote.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use hatchling_core::ui::{ScheduleNotice, UiEventBody};
use hatchling_scheduler::{TickReport, Timestamp};

use crate::Config;
use crate::api_stream::Bus;

/// A failed or undelivered run's detail, as the app's activity says it.
pub const KEPT: &str = "It didn't finish, or its result couldn't be delivered: it's kept for you to review.";

/// One broker's stream to the owner's app, once the API has attached it.
#[derive(Debug, Default)]
pub struct AppStream {
    bus: Mutex<Option<Arc<Bus>>>,
}

/// The streams, by the vault root of the broker each belongs to.
static STREAMS: Mutex<BTreeMap<String, Arc<AppStream>>> = Mutex::new(BTreeMap::new());

/// The stream of the broker `config` configures.
#[must_use]
pub fn stream(config: &Config) -> Arc<AppStream> {
    stream_at(&config.vault.root.display().to_string())
}

/// The stream of the broker whose vault is at `root`.
fn stream_at(root: &str) -> Arc<AppStream> {
    Arc::clone(
        STREAMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(root.to_owned())
            .or_default(),
    )
}

impl AppStream {
    /// Sends to the owner's app on `bus` from now on.
    pub fn attach(&self, bus: Arc<Bus>) {
        *self.bus.lock().unwrap_or_else(PoisonError::into_inner) = Some(bus);
    }

    /// Tells the app what one scheduler tick did: each run started, each
    /// result delivered, and each kept. Whether an app was attached.
    pub fn tell(&self, report: &TickReport) -> bool {
        let Some(bus) = self.bus.lock().unwrap_or_else(PoisonError::into_inner).clone() else {
            return false;
        };
        let notices = report
            .launched
            .iter()
            .map(|(id, _)| ScheduleNotice::Fired {
                schedule_id: id.clone(),
            })
            .chain(report.delivered.iter().map(|id| ScheduleNotice::Completed {
                schedule_id: id.clone(),
            }))
            .chain(report.parked.iter().map(|id| ScheduleNotice::Failed {
                schedule_id: id.clone(),
                detail: String::from(KEPT),
            }));
        for notice in notices {
            bus.push(Timestamp::now().to_string(), UiEventBody::Schedule(notice));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notices(bus: &Bus) -> Vec<ScheduleNotice> {
        bus.after(None)
            .into_iter()
            .filter_map(|event| {
                if let UiEventBody::Schedule(notice) = event.body {
                    Some(notice)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn a_tick_is_told_to_the_app_once_one_is_attached() {
        let report = TickReport {
            launched: vec![(String::from("sch-1"), false)],
            delivered: vec![String::from("sch-2")],
            parked: vec![String::from("sch-3")],
            removed: vec![String::from("sch-2")],
            ..TickReport::default()
        };
        let stream = AppStream::default();
        assert!(!stream.tell(&report), "no app yet: nothing sent");
        let bus = Arc::new(Bus::starting_at(1));
        stream.attach(Arc::clone(&bus));
        assert!(stream.tell(&report));
        assert_eq!(
            notices(&bus),
            [
                ScheduleNotice::Fired {
                    schedule_id: String::from("sch-1")
                },
                ScheduleNotice::Completed {
                    schedule_id: String::from("sch-2")
                },
                ScheduleNotice::Failed {
                    schedule_id: String::from("sch-3"),
                    detail: String::from(KEPT)
                },
            ]
        );
        // A quiet tick says nothing.
        assert!(stream.tell(&TickReport::default()));
        assert_eq!(notices(&bus).len(), 3);
    }

    #[test]
    fn each_broker_has_its_own_stream() {
        assert!(Arc::ptr_eq(&stream_at("/one"), &stream_at("/one")));
        assert!(!Arc::ptr_eq(&stream_at("/one"), &stream_at("/two")));
    }
}
