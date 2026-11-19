//! The owner's revoke of a site, reaching every running task (WO-036).
//!
//! A site the owner revokes is let go by everything that holds it:
//!
//! - **The store:** its standing grant is gone ([`crate::site_grants`]).
//! - **Approvals not yet used:** every approved request to read it that no
//!   session has claimed is withdrawn, whatever its task.
//! - **A browser's proxy:** it loses the site, and every tunnel it had open
//!   to it, both ways, at once.
//! - **Each session:** its web and its browser take the revoke before they
//!   next decide anything. A site held for the task is let go. A browser on
//!   the site's page is closed, so nothing more is done on a page loaded
//!   before the revoke.
//!
//! What a session was doing when the owner revoked finishes: a read in
//! flight is read, as a CONNECT proxy can't take back bytes it passed.
//!
//! **Order.** A revoke is numbered, from one counter. Each hold records the
//! number seen before what allowed it was read, and a revoke reaches only
//! a hold that saw an earlier number: a yes the owner gives after revoking
//! the site is theirs, and stands. A revoke that races a hold being made
//! reaches it, at the latest when that session's op ends.
//!
//! A signed-in profile's sites aren't reached: the owner keeps them with
//! the profile, which a session holds for its task (slice 3).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use hatchling_egress::web::WebUrl;

use crate::Config;
use crate::approval_board::Approved;
use crate::browser_proxy::Sites;

/// The latest revoke's number: each is one more.
static REVOKES: AtomicU64 = AtomicU64::new(0);

/// Every running session's part, by the store of grants it was allowed by.
static LIVE: Mutex<Vec<(PathBuf, Weak<Reach>)>> = Mutex::new(Vec::new());

/// The number of the latest revoke: read before what allows a hold is,
/// it's the number the hold saw.
#[must_use]
pub fn seen() -> u64 {
    REVOKES.load(Ordering::SeqCst)
}

/// Whether a revoke numbered `revoked` reaches a hold that saw `held`: it
/// came after, or with, what the hold was allowed by.
#[must_use]
pub const fn reaches(held: u64, revoked: u64) -> bool {
    held < revoked
}

/// One session's part: the sites revoked since it last took them, each
/// with its revoke's number, and its browser's proxy, if it has one.
#[derive(Debug, Default)]
pub struct Reach(Mutex<Part>);

#[derive(Debug, Default)]
struct Part {
    revoked: BTreeMap<String, u64>,
    proxy: Option<Proxy>,
}

/// A browser's proxy, which a revoke reaches at once, and the sites a
/// signed-in profile holds there, which it doesn't.
#[derive(Debug)]
struct Proxy {
    sites: Sites,
    kept: BTreeSet<String>,
}

impl Reach {
    fn part(&self) -> MutexGuard<'_, Part> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The sites revoked since the session last took them, each with its
    /// revoke's number. They're the session's to let go now.
    #[must_use]
    pub fn take(&self) -> BTreeMap<String, u64> {
        std::mem::take(&mut self.part().revoked)
    }

    /// Watches the browser's proxy `sites`, all but those in `kept`, which
    /// a signed-in profile holds: a revoke takes its site from them at once.
    pub fn watch(&self, sites: Sites, kept: BTreeSet<String>) {
        self.part().proxy = Some(Proxy { sites, kept });
    }

    /// `site` (`destination`, as the proxy has it) is revoked, numbered
    /// `number`.
    fn revoked(&self, site: &str, destination: &str, number: u64) {
        let mut part = self.part();
        part.revoked.insert(site.to_owned(), number);
        if let Some(proxy) = &part.proxy {
            if !proxy.kept.contains(destination) {
                let closed = proxy.sites.revoke(destination);
                tracing::debug!(%site, closed, "a revoked site's tunnels are closed");
            }
        }
    }
}

/// The store of grants `config`'s revokes come from; `None` when the
/// broker keeps none, and nothing can be revoked.
fn root(config: &Config) -> Option<PathBuf> {
    let root = &config.web.grants;
    (!root.as_os_str().is_empty()).then(|| root.clone())
}

/// A running session's part in `config`'s revokes, from now until it's
/// dropped; `None` when the broker keeps no grants.
#[must_use]
pub fn join(config: &Config) -> Option<Arc<Reach>> {
    let root = root(config)?;
    let reach = Arc::new(Reach::default());
    let mut live = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    live.retain(|(_, part)| part.strong_count() > 0);
    live.push((root, Arc::downgrade(&reach)));
    drop(live);
    Some(reach)
}

/// The owner revoked `site` from `config`'s store: every running task lets
/// it go.
///
/// Every approval to read it not yet used is withdrawn, and every running
/// session is told, each browser's proxy at once. Called after the store
/// has let it go. How many sessions it reached; none for anything that
/// isn't a site.
pub fn revoke(config: &Config, site: &str) -> usize {
    let Some(root) = root(config) else {
        return 0;
    };
    let Ok(url) = WebUrl::parse(site) else {
        return 0;
    };
    if url.origin().to_string() != site {
        return 0;
    }
    let withdrawn = crate::approval_board::board(config)
        .approvals()
        .withdraw_approved(|run| matches!(run, Approved::Site(visit) if visit.site == site));
    let destination = url.origin().destination();
    let live = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    let number = REVOKES.fetch_add(1, Ordering::SeqCst) + 1;
    let reached: Vec<Arc<Reach>> = live
        .iter()
        .filter(|(of, _)| *of == root)
        .filter_map(|(_, part)| part.upgrade())
        .collect();
    drop(live);
    for reach in &reached {
        reach.revoked(site, &destination, number);
    }
    tracing::info!(
        %site,
        withdrawn,
        sessions = reached.len(),
        "a revoke reached the running tasks"
    );
    reached.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept_in(name: &str) -> (Config, crate::api_auth::tests::Scratch) {
        let scratch = crate::api_auth::tests::scratch(&format!("grant-reach-{name}"));
        let mut config = crate::api_auth::tests::owner_config(None);
        config.vault.root = scratch.0.join("vault");
        config.web.grants = scratch.0.join("sites");
        (config, scratch)
    }

    /// A revoke reaches a hold that saw an earlier one, never one that saw
    /// it: a yes after a revoke stands.
    #[test]
    fn a_revoke_reaches_only_what_was_allowed_before_it() {
        assert!(reaches(4, 5));
        assert!(!reaches(5, 5), "a hold that saw the revoke");
        assert!(!reaches(6, 5));
    }

    /// Every session of the store is told, numbered after what it saw;
    /// another store's isn't, and nothing that isn't a site is a revoke.
    #[test]
    fn every_session_of_the_store_is_told() {
        let (config, _scratch) = kept_in("told");
        let (other, _other_scratch) = kept_in("other");
        let one = join(&config).unwrap();
        let two = join(&config).unwrap();
        let elsewhere = join(&other).unwrap();
        let before = seen();
        assert_eq!(revoke(&config, "https://a.example"), 2);
        for reach in [&one, &two] {
            let taken = reach.take();
            assert_eq!(taken.len(), 1);
            assert!(
                taken
                    .get("https://a.example")
                    .is_some_and(|number| reaches(before, *number))
            );
            assert!(reach.take().is_empty(), "taken once");
        }
        assert!(elsewhere.take().is_empty(), "another store's");
        for not_a_site in ["https://A.example", "https://a.example/", "a.example", ""] {
            assert_eq!(revoke(&config, not_a_site), 0, "{not_a_site:?}");
        }
        assert!(one.take().is_empty());
        drop(two);
        assert_eq!(revoke(&config, "https://b.example"), 1, "a session dropped");
        let _three = join(&config).unwrap();
        let parts = LIVE
            .lock()
            .unwrap()
            .iter()
            .filter(|(of, _)| *of == config.web.grants)
            .map(|(_, part)| part.strong_count())
            .collect::<Vec<_>>();
        assert_eq!(parts, [1, 1], "only the sessions running are kept");
        let mut off = config;
        off.web.grants = PathBuf::new();
        assert!(join(&off).is_none());
        assert_eq!(revoke(&off, "https://a.example"), 0);
    }

    /// A browser's proxy loses the site at once, but not a profile's.
    #[test]
    fn a_proxy_loses_the_site_at_once_but_not_a_profiles() {
        let (config, _scratch) = kept_in("proxy");
        let reach = join(&config).unwrap();
        let sites = Sites::default();
        sites.grant("a.example:443");
        sites.grant("signed.example:443");
        reach.watch(
            sites.clone(),
            BTreeSet::from([String::from("signed.example:443")]),
        );
        revoke(&config, "https://a.example");
        revoke(&config, "https://signed.example");
        assert!(!sites.holds("a.example:443"));
        assert!(sites.holds("signed.example:443"), "the profile's");
    }
}
