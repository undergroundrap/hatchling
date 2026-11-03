//! Feed runs: a feed schedule's fire, answered by the broker.
//!
//! A feed's digest is rules applied to the user's own records, so it
//! needs no cell, no model, and no tools. [`FeedLauncher`] wraps the real
//! launcher: a run that carries a feed spec is generated here, at once,
//! and every other run goes to the wrapped launcher unchanged. The
//! dispatcher delivers a feed's digest like any run's output, with its
//! retry and parking.
//!
//! Generation reads only through [`StoreReader`], whose one method runs a
//! read-only [`FeedQuery`] ([`crate::goals_ops::read_feed`]), and only
//! for the sections whose read grant the run still holds: the
//! schedule's grants minus any revoked when it fired. The principal is
//! the agent's own uid, the user the broker serves.

use hatchling_core::FeedSpec;
use hatchling_feed::{Reader, Spec, Window, generate};
use hatchling_goals::{FeedItem, FeedQuery};
use hatchling_memory::{MemoryPrincipal, SessionId};
use hatchling_scheduler::{Launcher, Run, RunRequest};

use crate::Config;

/// The task text a feed schedule is stored with: what it is, for logs.
pub const FEED_TASK: &str = "feed digest";

/// The read-only path a digest is generated through.
#[derive(Debug)]
pub struct StoreReader<'c> {
    config: &'c Config,
    principal: MemoryPrincipal,
}

impl Reader for StoreReader<'_> {
    fn read(&self, query: &FeedQuery) -> Option<Vec<FeedItem>> {
        crate::goals_ops::read_feed(
            &self.config.goals,
            &self.config.memory,
            &self.principal,
            query,
        )
    }
}

/// The digest of `feed` for the run `request`, or why there is none.
///
/// # Errors
///
/// Returns why: an unusable spec, or no session id for the reads.
pub fn digest(config: &Config, request: &RunRequest, feed: &FeedSpec) -> Result<String, String> {
    let spec = Spec::parse(feed).ok_or_else(|| String::from("the feed spec is unusable"))?;
    let session_id =
        SessionId::generate().map_err(|error| format!("no session id for the feed: {error}"))?;
    let reader = StoreReader {
        config,
        principal: MemoryPrincipal {
            peer_uid: config.agent_uid.as_u32(),
            session_id,
        },
    };
    let window = Window::ending(request.due.as_second(), spec.lookback_secs());
    Ok(generate(&spec, &request.grants, window, &reader).render())
}

/// A run: a feed's digest, ready at once, or the wrapped launcher's run.
#[derive(Debug)]
pub enum FeedRun<R> {
    /// A feed's digest, or why there is none; taken by the first poll.
    Ready(Option<Result<String, String>>),
    /// Any other run.
    Other(R),
}

impl<R: Run> Run for FeedRun<R> {
    fn poll(&mut self) -> Option<Result<String, String>> {
        match self {
            Self::Ready(outcome) => outcome.take(),
            Self::Other(run) => run.poll(),
        }
    }

    /// The wrapped run's ending: every scheduled run passes through here,
    /// and how one failed is what its notice tells the person.
    fn ending(&self) -> Option<hatchling_scheduler::Ending> {
        match self {
            Self::Ready(_) => None,
            Self::Other(run) => run.ending(),
        }
    }
}

/// The real launcher, with feed runs answered by the broker.
#[derive(Debug)]
pub struct FeedLauncher<'c, L> {
    config: &'c Config,
    inner: L,
}

impl<'c, L> FeedLauncher<'c, L> {
    /// `inner`, with feed runs answered from `config`'s stores.
    #[must_use]
    pub const fn new(config: &'c Config, inner: L) -> Self {
        Self { config, inner }
    }
}

impl<L: Launcher> Launcher for FeedLauncher<'_, L> {
    type Run = FeedRun<L::Run>;

    fn launch(&self, request: RunRequest) -> Result<Self::Run, String> {
        match &request.feed {
            Some(feed) => Ok(FeedRun::Ready(Some(digest(self.config, &request, feed)))),
            None => self.inner.launch(request).map(FeedRun::Other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run that failed as `self.0` says.
    struct Ended(hatchling_scheduler::Ending);

    impl Run for Ended {
        fn poll(&mut self) -> Option<Result<String, String>> {
            Some(Err(String::from("failed")))
        }

        fn ending(&self) -> Option<hatchling_scheduler::Ending> {
            Some(self.0)
        }
    }

    /// Found on a real install: a failed run's notice said only that
    /// something went wrong, because the wrapper every scheduled run
    /// passes through dropped how the run ended.
    #[test]
    fn a_wrapped_run_says_how_it_ended() {
        let ending = hatchling_scheduler::Ending::Exited(69);
        let mut wrapped = FeedRun::Other(Ended(ending));
        assert!(matches!(wrapped.poll(), Some(Err(_))));
        assert_eq!(wrapped.ending(), Some(ending));
        let feed = FeedRun::<Ended>::Ready(Some(Ok(String::from("digest"))));
        assert_eq!(feed.ending(), None, "a feed's digest has no run to end");
    }
}
