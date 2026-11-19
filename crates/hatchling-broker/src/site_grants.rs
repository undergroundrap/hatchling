//! The owner's standing grants (WO-028, WO-036): what the owner allows the
//! agent past one use, sealed in a store of their own
//! (`HATCHLING_WEB_GRANTS_DIR`), listed and revoked by the owner alone.
//!
//! - **A grant is a key and a scope.** The key names what is allowed: a
//!   kind and its target, made by the broker from the request it froze,
//!   never from the agent's words ([`GrantKey`]). Today the only kind is
//!   reading a site, whose target is its origin exactly as approved, in
//!   its one canonical form: a grant never covers a subdomain, another
//!   port, or another site. The scope says for how long: a day, or always
//!   ([`Scope`]); once and for a task are never kept here.
//! - **Only the owner adds one,** by their decision on an approval (#542),
//!   whatever became of the session that asked; no session op reaches this
//!   store, so a cell can't grant, list or revoke one, and checking a grant
//!   writes nothing.
//! - **A day is 24 hours by the broker's clock** ([`DAY_SECS`]). A clock
//!   seen to go back, against the latest time this broker or the store has
//!   seen, makes every day grant cover nothing until it catches up: a
//!   clock set back never stretches one.
//! - **Bounded:** at most [`MAX_SITES`]; past that a new one is refused,
//!   and nothing in force is dropped to make room. Day grants that ended
//!   are dropped by the owner's own operations, never by a check.
//! - **Revoking one** takes effect at the next read, for every session, the
//!   one that asked included. Only a session the owner allowed the site for
//!   its task keeps it until it ends.
//! - **Kept before WO-036** as one list of sites: each becomes an always
//!   grant, and the old list is removed, before anything else the store
//!   does, so nothing is ever read from both.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use hatchling_core::SecretName;
use hatchling_core::ui::{AllowedSite, SiteOp, SiteResult};
use hatchling_egress::web::WebUrl;
use hatchling_scheduler::Timestamp;
use hatchling_vault::{OpensStores, SealedVault, Secret, Store};
use serde::{Deserialize, Serialize};

use crate::Config;

/// The most grants the owner may keep: at the longest host names, still
/// within one sealed entry.
pub const MAX_SITES: usize = 100;

/// How long a day grant covers, in seconds.
pub const DAY_SECS: i64 = 24 * 60 * 60;

/// The latest time a grant is made at, in Unix seconds: the end of the year
/// 9999. A store that says later can't be one this broker wrote.
const LATEST: i64 = 253_402_300_799;

/// The store's one entry: every grant, in the order made.
const ENTRY: &str = "grants";

/// The entry the sites always allowed were kept in before WO-036.
const SITES_ENTRY: &str = "allowed-sites";

/// Serializes the store's writers within a broker.
static STORE: Mutex<()> = Mutex::new(());

/// The latest time this broker has seen its clock say, in Unix seconds,
/// by the store it was seen for.
static SEEN: Mutex<BTreeMap<PathBuf, i64>> = Mutex::new(BTreeMap::new());

/// What a grant allows: its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// Reading a site.
    #[serde(rename = "web.visit")]
    WebVisit,
}

impl Kind {
    /// The kind as a key names it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WebVisit => "web.visit",
        }
    }
}

/// What a grant allows: a kind and its target, in their one canonical
/// form. Made only from what the broker froze.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantKey {
    kind: Kind,
    target: String,
}

impl GrantKey {
    /// The key for reading `site`, an origin (`https://host[:port]`) in its
    /// canonical form: `None` for anything else, which no grant covers.
    #[must_use]
    pub fn site(site: &str) -> Option<Self> {
        canonical_site(site).map(|target| Self {
            kind: Kind::WebVisit,
            target,
        })
    }

    /// Its kind.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// Its target: for a site, its origin.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }
}

impl fmt::Display for GrantKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind.as_str(), self.target)
    }
}

/// `site` if it is an origin in its one canonical form, as [`WebUrl`]
/// writes it: nothing else is a site's target.
fn canonical_site(site: &str) -> Option<String> {
    let url = WebUrl::parse(site).ok()?;
    let origin = url.origin().to_string();
    (origin == site).then_some(origin)
}

/// How long a grant covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// For [`DAY_SECS`] from when it was made.
    Day,
    /// Until the owner revokes it.
    Always,
}

/// One grant, as kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Grant {
    #[serde(rename = "k")]
    kind: Kind,
    #[serde(rename = "t")]
    target: String,
    #[serde(rename = "s")]
    scope: Scope,
    #[serde(rename = "m")]
    made_at: i64,
}

impl Grant {
    fn key(&self) -> GrantKey {
        GrantKey {
            kind: self.kind,
            target: self.target.clone(),
        }
    }

    /// Whether it covers anything at `now`; `back` when the clock is seen
    /// to have gone back.
    const fn in_force(&self, now: i64, back: bool) -> bool {
        match self.scope {
            Scope::Always => true,
            Scope::Day => !back && self.made_at <= now && now < self.made_at + DAY_SECS,
        }
    }
}

/// The store's entry: every grant, and the latest time the owner's
/// operations have seen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    #[serde(rename = "g")]
    grants: Vec<Grant>,
    #[serde(rename = "h")]
    high_water: i64,
}

/// One site always allowed before WO-036, as it was kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct KeptSite {
    site: String,
    allowed_at: i64,
}

/// Whether the clock says `now` earlier than the latest this broker has
/// seen for the store at `root`, or the store's `high_water`: it went back.
/// Noted, either way.
fn went_back(root: &Path, now: i64, high_water: i64) -> bool {
    let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
    let latest = seen.entry(root.to_path_buf()).or_insert(now);
    let before = *latest;
    *latest = before.max(now);
    drop(seen);
    now < before.max(high_water)
}

/// The owner's standing grants, in their sealed store.
pub struct SiteGrants<'k> {
    root: &'k Path,
    keys: &'k dyn OpensStores,
}

impl fmt::Debug for SiteGrants<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SiteGrants")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl<'k> SiteGrants<'k> {
    /// The store `config` gives the broker, opened with `keys`; `None` when
    /// the operator gave it none, and nothing is kept.
    #[must_use]
    pub fn of(config: &'k Config, keys: &'k dyn OpensStores) -> Option<Self> {
        let root = config.web.grants.as_path();
        (!root.as_os_str().is_empty()).then_some(Self { root, keys })
    }

    fn vault(&self) -> Result<SealedVault<'_>, String> {
        SealedVault::open_in(self.root, self.keys, Store::SITES)
            .map_err(|error| format!("the grants can't be opened: {error}"))
    }

    fn name(entry: &str) -> Result<SecretName, String> {
        entry
            .parse()
            .map_err(|_| format!("the grants' entry name {entry:?} is invalid"))
    }

    fn read(vault: &SealedVault<'_>) -> Result<Kept, String> {
        let kept: Kept = match vault.fetch(&Self::name(ENTRY)?) {
            Ok(secret) => serde_json::from_slice(secret.as_bytes())
                .map_err(|error| format!("the grants are corrupt: {error}"))?,
            Err(hatchling_vault::Error::NotFound { .. }) => Kept::default(),
            Err(error) => return Err(format!("the grants can't be read: {error}")),
        };
        let sound = kept.grants.len() <= MAX_SITES
            && (0..=LATEST).contains(&kept.high_water)
            && kept.grants.iter().all(|grant| {
                (0..=LATEST).contains(&grant.made_at)
                    && match grant.kind {
                        Kind::WebVisit => canonical_site(&grant.target).is_some(),
                    }
            });
        if sound {
            Ok(kept)
        } else {
            Err(String::from("the grants hold what grants can't"))
        }
    }

    fn write(vault: &SealedVault<'_>, kept: &Kept) -> Result<(), String> {
        let bytes = serde_json::to_vec(kept)
            .map_err(|error| format!("the grants don't encode: {error}"))?;
        let secret =
            Secret::new(bytes).map_err(|error| format!("the grants don't fit: {error}"))?;
        vault
            .store(&Self::name(ENTRY)?, &secret)
            .map_err(|error| format!("the grants can't be written: {error}"))
    }

    /// Opens the store, with its lock held, the sites kept before WO-036
    /// moved in first.
    fn opened<T>(
        &self,
        then: impl FnOnce(&SealedVault<'_>, Kept) -> Result<T, String>,
    ) -> Result<T, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let kept = Self::moved_in(&vault)?;
        then(&vault, kept)
    }

    /// The store's grants, each site always allowed before WO-036 moved in
    /// as an always grant, made when it was allowed, and the old list
    /// removed once they are written: a crash between the two leaves both,
    /// which move in as the same grants again.
    fn moved_in(vault: &SealedVault<'_>) -> Result<Kept, String> {
        let mut kept = Self::read(vault)?;
        let old = Self::name(SITES_ENTRY)?;
        let sites: Vec<KeptSite> = match vault.fetch(&old) {
            Ok(secret) => serde_json::from_slice(secret.as_bytes())
                .map_err(|error| format!("the sites allowed before are corrupt: {error}"))?,
            Err(hatchling_vault::Error::NotFound { .. }) => return Ok(kept),
            Err(error) => {
                return Err(format!("the sites allowed before can't be read: {error}"));
            }
        };
        for site in sites {
            let Some(key) = GrantKey::site(&site.site) else {
                tracing::warn!("a site allowed before isn't one a grant can name: it isn't kept");
                continue;
            };
            if kept
                .grants
                .iter()
                .any(|grant| grant.key() == key && grant.scope == Scope::Always)
            {
                continue;
            }
            kept.grants.retain(|grant| grant.key() != key);
            kept.grants.push(Grant {
                kind: key.kind,
                target: key.target,
                scope: Scope::Always,
                made_at: site.allowed_at.clamp(0, LATEST),
            });
        }
        Self::write(vault, &kept)?;
        match vault.remove(&old) {
            Ok(()) | Err(hatchling_vault::Error::NotFound { .. }) => {}
            Err(error) => {
                return Err(format!(
                    "the sites allowed before can't be removed: {error}"
                ));
            }
        }
        tracing::info!("the sites always allowed are kept as grants now");
        Ok(kept)
    }

    /// Every site a grant in force at `now` covers, with when the grant was
    /// made, oldest first.
    ///
    /// # Errors
    ///
    /// Why the store can't be read.
    pub fn list(&self, now: i64) -> Result<Vec<(String, i64)>, String> {
        self.opened(|_, kept| {
            let back = went_back(self.root, now, kept.high_water);
            Ok(kept
                .grants
                .into_iter()
                .filter(|grant| grant.in_force(now, back))
                .map(|grant| (grant.target, grant.made_at))
                .collect())
        })
    }

    /// Whether a grant in force at `now` covers `key`. Checking writes
    /// nothing.
    ///
    /// # Errors
    ///
    /// Why the store can't be read.
    pub fn covers(&self, key: &GrantKey, now: i64) -> Result<bool, String> {
        self.opened(|_, kept| {
            let back = went_back(self.root, now, kept.high_water);
            Ok(kept
                .grants
                .iter()
                .any(|grant| grant.key() == *key && grant.in_force(now, back)))
        })
    }

    /// Whether a grant in force at `now` covers reading `site`.
    ///
    /// # Errors
    ///
    /// Why the store can't be read.
    pub fn holds(&self, site: &str, now: i64) -> Result<bool, String> {
        GrantKey::site(site).map_or(Ok(false), |key| self.covers(&key, now))
    }

    /// Allows `site` always, from `now`; one allowed always already stays as
    /// it was.
    ///
    /// # Errors
    ///
    /// Why it can't be kept: not a site, the store's failure, or
    /// [`MAX_SITES`] kept.
    pub fn allow(&self, site: &str, now: i64) -> Result<(), String> {
        let key = GrantKey::site(site).ok_or_else(|| format!("{site:?} isn't a site"))?;
        self.grant(&key, Scope::Always, now)
    }

    /// Grants `key` for `scope`, from `now`. An always grant of it stays as
    /// it was; a day grant of it is replaced, so a day starts again. Day
    /// grants that ended are dropped first.
    ///
    /// # Errors
    ///
    /// Why it can't be kept: the store's failure, or [`MAX_SITES`] in force.
    pub fn grant(&self, key: &GrantKey, scope: Scope, now: i64) -> Result<(), String> {
        let now = now.clamp(0, LATEST);
        self.opened(|vault, mut kept| {
            Self::tidy(&mut kept, now);
            if kept
                .grants
                .iter()
                .any(|grant| grant.key() == *key && grant.scope == Scope::Always)
            {
                return Ok(());
            }
            kept.grants.retain(|grant| grant.key() != *key);
            if kept.grants.len() >= MAX_SITES {
                return Err(format!(
                    "{MAX_SITES} grants are kept already: revoke one first"
                ));
            }
            kept.grants.push(Grant {
                kind: key.kind,
                target: key.target.clone(),
                scope,
                made_at: now,
            });
            Self::write(vault, &kept)
        })
    }

    /// Drops the day grants that ended by `now`, or by the latest time the
    /// store has seen, and moves that time on to `now`.
    fn tidy(kept: &mut Kept, now: i64) {
        let latest = kept.high_water.max(now);
        kept.grants
            .retain(|grant| grant.scope == Scope::Always || latest < grant.made_at + DAY_SECS);
        kept.high_water = latest;
    }

    /// Revokes every grant of `site`, at `now`. Whether one was in force.
    ///
    /// # Errors
    ///
    /// Why the store can't be read or changed.
    pub fn revoke(&self, site: &str, now: i64) -> Result<bool, String> {
        let Some(key) = GrantKey::site(site) else {
            return Ok(false);
        };
        let now = now.clamp(0, LATEST);
        self.opened(|vault, mut kept| {
            let back = went_back(self.root, now, kept.high_water);
            let was = kept
                .grants
                .iter()
                .any(|grant| grant.key() == key && grant.in_force(now, back));
            let before = kept.grants.len();
            kept.grants.retain(|grant| grant.key() != key);
            if kept.grants.len() == before {
                return Ok(false);
            }
            Self::tidy(&mut kept, now);
            Self::write(vault, &kept)?;
            Ok(was)
        })
    }
}

/// The owner's `op` on the allowed sites, under `config`, with `keys`, at
/// `now`.
#[must_use]
pub fn handle_site_op(
    config: &Config,
    keys: &dyn OpensStores,
    op: &SiteOp,
    now: i64,
) -> SiteResult {
    let Some(grants) = SiteGrants::of(config, keys) else {
        return SiteResult::Off;
    };
    let answer = match op {
        SiteOp::List => grants.list(now).map(|sites| SiteResult::Listed {
            sites: sites
                .into_iter()
                .map(|(site, at)| AllowedSite {
                    site,
                    allowed_at: Timestamp::from_second(at)
                        .map(|at| at.to_string())
                        .unwrap_or_default(),
                })
                .collect(),
        }),
        SiteOp::Revoke { site } => grants.revoke(site, now).map(|revoked| {
            if revoked {
                SiteResult::Revoked
            } else {
                SiteResult::NotFound
            }
        }),
    };
    answer.unwrap_or_else(|error| {
        tracing::error!(%error, "a site operation failed");
        SiteResult::Failed
    })
}

/// Keeps the owner's choice on a site's approval, made at `now` (#542).
///
/// *Allow always* is a grant from the moment the owner decides, whatever
/// becomes of the session that asked. Anything else keeps nothing here.
///
/// # Errors
///
/// Why an always grant couldn't be kept.
pub fn keep_decision(
    config: &Config,
    keys: &dyn OpensStores,
    site: &str,
    decision: hatchling_core::ui::Decision,
    now: i64,
) -> Result<(), String> {
    if decision != hatchling_core::ui::Decision::ApproveAlways {
        return Ok(());
    }
    let grants = SiteGrants::of(config, keys)
        .ok_or_else(|| String::from("no store is given for always-allowed sites"))?;
    grants.allow(site, now)
}

#[cfg(test)]
mod tests {
    use hatchling_core::ui::Decision;
    use hatchling_vault::MasterKey;

    use super::*;

    /// A time grants are made at in these tests.
    const T: i64 = 1_790_000_000;

    /// A config whose grants are kept in a scratch directory.
    fn kept(name: &str) -> (Config, crate::api_auth::tests::Scratch, MasterKey) {
        let scratch = crate::api_auth::tests::scratch(&format!("sites-{name}"));
        let mut config = crate::api_auth::tests::owner_config(None);
        config.web.grants = scratch.0.join("sites");
        (config, scratch, MasterKey::generate().unwrap())
    }

    fn key(site: &str) -> GrantKey {
        GrantKey::site(site).unwrap()
    }

    /// What the store keeps, read as it is: nothing moved in, nothing
    /// written.
    fn raw(grants: &SiteGrants<'_>) -> Kept {
        SiteGrants::read(&grants.vault().unwrap()).unwrap()
    }

    /// As a broker started again: what it saw of the clock is gone.
    fn restarted(grants: &SiteGrants<'_>) {
        SEEN.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(grants.root);
    }

    /// A key is a site's origin in its one canonical form, and nothing
    /// else: no other spelling of it, no path, no other scheme.
    #[test]
    fn a_key_is_a_site_in_its_one_form() {
        let site = key("https://a.example");
        assert_eq!(site.kind(), Kind::WebVisit);
        assert_eq!(site.target(), "https://a.example");
        assert_eq!(site.to_string(), "web.visit:https://a.example");
        assert_eq!(
            key("https://a.example:8443").target(),
            "https://a.example:8443"
        );
        for other in [
            "https://A.example",
            "https://a.example:443",
            "https://a.example/",
            "https://a.example/page",
            "http://a.example",
            "a.example",
            "https://a.example.",
            "",
        ] {
            assert!(GrantKey::site(other).is_none(), "{other:?} made a key");
        }
    }

    /// A site is allowed once, listed oldest first, held exactly, and gone
    /// once revoked.
    #[test]
    fn a_site_is_allowed_listed_and_revoked() {
        let (config, _scratch, master) = kept("allow");
        let grants = SiteGrants::of(&config, &master).unwrap();
        assert_eq!(grants.list(T).unwrap(), []);
        grants.allow("https://a.example", T).unwrap();
        grants.allow("https://b.example:8443", T + 1).unwrap();
        grants.allow("https://a.example", T + 2).unwrap();
        assert_eq!(
            grants.list(T + 3).unwrap(),
            [
                (String::from("https://a.example"), T),
                (String::from("https://b.example:8443"), T + 1)
            ],
            "allowed once, at first"
        );
        assert!(grants.holds("https://a.example", T + 3).unwrap());
        assert!(!grants.holds("https://www.a.example", T + 3).unwrap());
        assert!(!grants.holds("https://b.example", T + 3).unwrap());
        assert!(!grants.holds("https://A.example", T + 3).unwrap());
        assert!(grants.allow("https://a.example/page", T + 3).is_err());
        assert!(grants.revoke("https://a.example", T + 4).unwrap());
        assert!(!grants.revoke("https://a.example", T + 4).unwrap());
        assert!(!grants.revoke("not a site", T + 4).unwrap());
        assert!(!grants.holds("https://a.example", T + 4).unwrap());
        assert_eq!(grants.list(T + 4).unwrap().len(), 1);
        // Sealed under the sites store, and kept across opens.
        let again = SiteGrants::of(&config, &master).unwrap();
        assert!(again.holds("https://b.example:8443", T + 5).unwrap());
        let shown = format!("{again:?}");
        assert!(shown.starts_with("SiteGrants { root: "), "{shown}");
    }

    /// A day grant covers for [`DAY_SECS`] from when it was made, and not
    /// before or after; an always grant covers until revoked.
    #[test]
    fn a_day_grant_covers_for_a_day() {
        let (config, _scratch, master) = kept("day");
        let grants = SiteGrants::of(&config, &master).unwrap();
        let day = key("https://day.example");
        grants.grant(&day, Scope::Day, T).unwrap();
        grants
            .grant(&key("https://always.example"), Scope::Always, T)
            .unwrap();
        assert_eq!(DAY_SECS, 86_400, "a day isn't 24 hours");
        assert!(grants.covers(&day, T).unwrap());
        assert!(grants.covers(&day, T + DAY_SECS - 1).unwrap());
        assert!(!grants.covers(&day, T + DAY_SECS).unwrap(), "past its day");
        assert!(
            grants
                .holds("https://always.example", T + 10 * DAY_SECS)
                .unwrap()
        );
        assert_eq!(
            grants.list(T + DAY_SECS).unwrap(),
            [(String::from("https://always.example"), T)],
            "an ended day grant was listed"
        );
        // A store whose latest time is before a grant's, as one written
        // with the clock behind: the grant still covers nothing before it
        // was made.
        let ahead = key("https://ahead.example");
        let mut written = raw(&grants);
        written.grants.push(Grant {
            kind: Kind::WebVisit,
            target: ahead.target().to_owned(),
            scope: Scope::Day,
            made_at: T + 100,
        });
        written.high_water = T;
        SiteGrants::write(&grants.vault().unwrap(), &written).unwrap();
        restarted(&grants);
        assert!(
            !grants.covers(&ahead, T + 50).unwrap(),
            "covered before it was made"
        );
        assert!(grants.covers(&ahead, T + 100).unwrap());
        let early = key("https://early.example");
        restarted(&grants);
        grants.grant(&early, Scope::Day, T + 10 * DAY_SECS).unwrap();
        restarted(&grants);
        assert!(
            !grants.covers(&early, T + 10 * DAY_SECS - 1).unwrap(),
            "a day grant covered before it was made"
        );
    }

    /// A clock that goes back stretches no day grant: against the latest
    /// time this broker saw, or the store's own after a restart, every day
    /// grant covers nothing until the clock catches up. An always grant
    /// isn't a matter of time.
    #[test]
    fn a_clock_set_back_stretches_no_day_grant() {
        let (config, _scratch, master) = kept("clock");
        let grants = SiteGrants::of(&config, &master).unwrap();
        let day = key("https://day.example");
        let always = key("https://always.example");
        grants.grant(&day, Scope::Day, T).unwrap();
        grants.grant(&always, Scope::Always, T).unwrap();
        assert!(grants.covers(&day, T + 100).unwrap());
        assert!(!grants.covers(&day, T + 50).unwrap(), "seen in this broker");
        assert!(grants.covers(&always, T + 50).unwrap());
        assert!(grants.covers(&day, T + 100).unwrap(), "caught up");
        // Started again: the store's own latest time holds.
        grants.revoke("https://other.example", T + 200).unwrap();
        grants
            .grant(&key("https://other.example"), Scope::Always, T + 200)
            .unwrap();
        restarted(&grants);
        assert!(!grants.covers(&day, T + 150).unwrap(), "seen by the store");
        assert!(
            grants
                .list(T + 150)
                .unwrap()
                .iter()
                .all(|(site, _)| site != day.target())
        );
        assert!(grants.covers(&day, T + 201).unwrap());
    }

    /// Granted again: a day starts again; an always grant stays, and one
    /// made over a day grant replaces it.
    #[test]
    fn granted_again_a_day_starts_again_and_always_stays() {
        let (config, _scratch, master) = kept("again");
        let grants = SiteGrants::of(&config, &master).unwrap();
        let site = key("https://a.example");
        grants.grant(&site, Scope::Day, T).unwrap();
        grants.grant(&site, Scope::Day, T + 100).unwrap();
        assert_eq!(raw(&grants).grants.len(), 1);
        assert!(
            grants.covers(&site, T + DAY_SECS + 50).unwrap(),
            "the day began again"
        );
        grants.grant(&site, Scope::Always, T + 200).unwrap();
        grants.grant(&site, Scope::Day, T + 300).unwrap();
        let kept = raw(&grants);
        assert_eq!(
            kept.grants
                .iter()
                .map(|grant| (grant.scope, grant.made_at))
                .collect::<Vec<_>>(),
            [(Scope::Always, T + 200)]
        );
        assert!(grants.covers(&site, T + 10 * DAY_SECS).unwrap());
    }

    /// A revoke says whether the grant was in force: one whose day ended
    /// is removed and said not found, whatever else is in force.
    #[test]
    fn a_revoke_of_an_ended_grant_finds_nothing_in_force() {
        let (config, _scratch, master) = kept("ended");
        let grants = SiteGrants::of(&config, &master).unwrap();
        grants
            .grant(&key("https://day.example"), Scope::Day, T)
            .unwrap();
        grants.allow("https://always.example", T).unwrap();
        assert!(!grants.revoke("https://day.example", T + DAY_SECS).unwrap());
        assert_eq!(
            raw(&grants)
                .grants
                .iter()
                .map(|grant| grant.target.as_str())
                .collect::<Vec<_>>(),
            ["https://always.example"],
            "the ended grant stayed"
        );
        assert!(
            grants
                .revoke("https://always.example", T + DAY_SECS)
                .unwrap()
        );
    }

    /// Checking writes nothing: an ended day grant is dropped by the
    /// owner's next operation, never by a check.
    #[test]
    fn checking_writes_nothing_and_the_owner_tidies() {
        let (config, _scratch, master) = kept("tidy");
        let grants = SiteGrants::of(&config, &master).unwrap();
        let day = key("https://day.example");
        grants.grant(&day, Scope::Day, T).unwrap();
        let before = raw(&grants);
        assert!(!grants.covers(&day, T + DAY_SECS).unwrap());
        assert!(!grants.holds("https://day.example", T + DAY_SECS).unwrap());
        let _ = grants.list(T + DAY_SECS).unwrap();
        assert_eq!(raw(&grants), before, "a check wrote");
        grants.allow("https://new.example", T + DAY_SECS).unwrap();
        let after = raw(&grants);
        assert_eq!(after.grants.len(), 1, "the ended day grant stayed");
        assert_eq!(after.high_water, T + DAY_SECS);
    }

    /// At most [`MAX_SITES`] in force: past that a new one is refused, and
    /// nothing in force is dropped; an ended day grant makes room.
    #[test]
    fn the_grants_are_bounded_and_none_in_force_dropped() {
        let (config, _scratch, master) = kept("bound");
        let grants = SiteGrants::of(&config, &master).unwrap();
        let mut kept = Kept {
            grants: (0..MAX_SITES)
                .map(|n| Grant {
                    kind: Kind::WebVisit,
                    target: format!("https://s{n}.example"),
                    scope: Scope::Always,
                    made_at: T,
                })
                .collect(),
            high_water: T,
        };
        SiteGrants::write(&grants.vault().unwrap(), &kept).unwrap();
        assert!(grants.allow("https://new.example", T).is_err());
        assert!(
            grants.allow("https://s0.example", T).is_ok(),
            "one kept already"
        );
        assert_eq!(grants.list(T).unwrap().len(), MAX_SITES);
        if let Some(first) = kept.grants.first_mut() {
            first.scope = Scope::Day;
        }
        SiteGrants::write(&grants.vault().unwrap(), &kept).unwrap();
        assert!(
            grants.allow("https://new.example", T + DAY_SECS).is_ok(),
            "an ended day grant held a place"
        );
        assert_eq!(raw(&grants).grants.len(), MAX_SITES);
    }

    /// The store's parts, called directly: an empty store reads as no
    /// grants, what is written is what is read, and a store holding what
    /// this broker never writes is refused.
    #[test]
    fn the_stores_parts() {
        let (config, _scratch, master) = kept("parts");
        let grants = SiteGrants::of(&config, &master).unwrap();
        let vault = grants.vault().unwrap();
        assert_eq!(SiteGrants::read(&vault).unwrap(), Kept::default());
        let one = |target: &str, made_at: i64| Kept {
            grants: vec![Grant {
                kind: Kind::WebVisit,
                target: target.to_owned(),
                scope: Scope::Day,
                made_at,
            }],
            high_water: 7,
        };
        SiteGrants::write(&vault, &one("https://a.example", 7)).unwrap();
        assert_eq!(
            SiteGrants::read(&vault).unwrap(),
            one("https://a.example", 7)
        );
        assert_eq!(SiteGrants::name(ENTRY).unwrap().as_str(), ENTRY);
        for unsound in [
            one("https://A.example", 7),
            one("https://a.example/", 7),
            one("https://a.example", LATEST + 1),
            one("https://a.example", -1),
            Kept {
                high_water: LATEST + 1,
                ..one("https://a.example", 7)
            },
        ] {
            SiteGrants::write(&vault, &unsound).unwrap();
            assert!(SiteGrants::read(&vault).is_err(), "{unsound:?} was read");
        }
        let too_many = Kept {
            grants: (0..=MAX_SITES)
                .map(|n| one(&format!("https://s{n}.example"), 7).grants.remove(0))
                .collect(),
            high_water: 7,
        };
        SiteGrants::write(&vault, &too_many).unwrap();
        assert!(
            SiteGrants::read(&vault).is_err(),
            "more than the cap was read"
        );
    }

    /// [`MAX_SITES`] of the longest sites, at the latest time, still fit one
    /// sealed entry.
    #[test]
    fn the_longest_grants_fit_one_entry() {
        // 253 bytes: the longest host, in labels of at most 63.
        let host = |n: usize| {
            format!(
                "{n:03}{}.{}.{}.{}",
                "a".repeat(60),
                "b".repeat(63),
                "c".repeat(63),
                "d".repeat(61)
            )
        };
        let kept = Kept {
            grants: (0..MAX_SITES)
                .map(|n| Grant {
                    kind: Kind::WebVisit,
                    target: format!("https://{}:65535", host(n)),
                    scope: Scope::Always,
                    made_at: LATEST,
                })
                .collect(),
            high_water: LATEST,
        };
        assert!(
            kept.grants
                .iter()
                .all(|grant| canonical_site(&grant.target).is_some())
        );
        let bytes = serde_json::to_vec(&kept).unwrap();
        assert!(
            bytes.len() <= hatchling_core::MAX_SECRET_BYTES,
            "{}",
            bytes.len()
        );
    }

    /// The sites always allowed before WO-036 move in as always grants, made
    /// when they were allowed, before anything else is done, and the old
    /// list goes. A crash that left both moves in the same grants again; a
    /// grant revoked after never comes back.
    #[test]
    fn the_sites_allowed_before_move_in_once() {
        let (config, _scratch, master) = kept("move");
        let grants = SiteGrants::of(&config, &master).unwrap();
        let vault = grants.vault().unwrap();
        let old = SiteGrants::name(SITES_ENTRY).unwrap();
        let sites = |list: &[(&str, i64)]| {
            let kept: Vec<KeptSite> = list
                .iter()
                .map(|(site, at)| KeptSite {
                    site: (*site).to_owned(),
                    allowed_at: *at,
                })
                .collect();
            Secret::new(serde_json::to_vec(&kept).unwrap()).unwrap()
        };
        vault
            .store(
                &old,
                &sites(&[
                    ("https://a.example", 100),
                    ("https://b.example:8443", 200),
                    ("not a site", 300),
                ]),
            )
            .unwrap();
        // A day grant of a site always allowed before becomes always.
        SiteGrants::write(
            &vault,
            &Kept {
                grants: vec![Grant {
                    kind: Kind::WebVisit,
                    target: String::from("https://b.example:8443"),
                    scope: Scope::Day,
                    made_at: 150,
                }],
                high_water: 150,
            },
        )
        .unwrap();
        assert!(grants.holds("https://a.example", T).unwrap());
        assert!(
            matches!(
                vault.fetch(&old),
                Err(hatchling_vault::Error::NotFound { .. })
            ),
            "the old list stayed"
        );
        let moved = raw(&grants);
        assert_eq!(
            moved
                .grants
                .iter()
                .map(|grant| (grant.target.as_str(), grant.scope, grant.made_at))
                .collect::<Vec<_>>(),
            [
                ("https://a.example", Scope::Always, 100),
                ("https://b.example:8443", Scope::Always, 200)
            ]
        );
        // As a crash between the two leaves it: both, the same grants.
        vault
            .store(&old, &sites(&[("https://a.example", 100)]))
            .unwrap();
        assert_eq!(grants.list(T).unwrap().len(), 2);
        assert_eq!(raw(&grants), moved, "moved in twice");
        // Revoked after: gone for good, the old list gone with it.
        assert!(grants.revoke("https://a.example", T).unwrap());
        assert!(!grants.holds("https://a.example", T).unwrap());
        assert!(
            !SiteGrants::of(&config, &master)
                .unwrap()
                .holds("https://a.example", T)
                .unwrap()
        );
    }

    /// The owner's operations: listed with when, revoked, not found; off
    /// with no store; failed when the store can't be read.
    #[test]
    fn the_owners_operations() {
        let (config, _scratch, master) = kept("ops");
        assert_eq!(
            handle_site_op(&config, &master, &SiteOp::List, T),
            SiteResult::Listed { sites: Vec::new() }
        );
        SiteGrants::of(&config, &master)
            .unwrap()
            .allow("https://a.example", T)
            .unwrap();
        assert_eq!(
            handle_site_op(&config, &master, &SiteOp::List, T),
            SiteResult::Listed {
                sites: vec![AllowedSite {
                    site: String::from("https://a.example"),
                    allowed_at: String::from("2026-09-21T14:13:20Z"),
                }]
            }
        );
        let revoke = SiteOp::Revoke {
            site: String::from("https://a.example"),
        };
        assert_eq!(
            handle_site_op(&config, &master, &revoke, T),
            SiteResult::Revoked
        );
        assert_eq!(
            handle_site_op(&config, &master, &revoke, T),
            SiteResult::NotFound
        );
        let off = crate::api_auth::tests::owner_config(None);
        assert!(SiteGrants::of(&off, &master).is_none());
        assert_eq!(
            handle_site_op(&off, &master, &SiteOp::List, T),
            SiteResult::Off
        );
        // Another key can't open the sealed store.
        let other = MasterKey::generate().unwrap();
        assert_eq!(
            handle_site_op(&config, &other, &SiteOp::List, T),
            SiteResult::Failed
        );
        let corrupt = SiteGrants::of(&config, &master).unwrap();
        corrupt
            .vault()
            .unwrap()
            .store(
                &SiteGrants::name(ENTRY).unwrap(),
                &Secret::new(b"not json".to_vec()).unwrap(),
            )
            .unwrap();
        assert!(corrupt.list(T).is_err());
        assert!(corrupt.allow("https://x.example", T).is_err());
        assert!(corrupt.revoke("https://x.example", T).is_err());
    }

    /// The owner's decision keeps an always grant the moment it is made
    /// (#542), and any other keeps nothing; with no store, an always can't
    /// be kept and says so.
    #[test]
    fn the_owners_always_is_kept_when_decided() {
        let (config, _scratch, master) = kept("decided");
        for decision in [Decision::Approve, Decision::ApproveSession, Decision::Deny] {
            keep_decision(&config, &master, "https://a.example", decision, T).unwrap();
        }
        let grants = SiteGrants::of(&config, &master).unwrap();
        assert_eq!(grants.list(T).unwrap(), [], "kept for less than always");
        keep_decision(
            &config,
            &master,
            "https://a.example",
            Decision::ApproveAlways,
            T,
        )
        .unwrap();
        assert!(grants.holds("https://a.example", T).unwrap());
        let off = crate::api_auth::tests::owner_config(None);
        assert!(
            keep_decision(
                &off,
                &master,
                "https://a.example",
                Decision::ApproveAlways,
                T
            )
            .is_err()
        );
        keep_decision(&off, &master, "https://a.example", Decision::Approve, T).unwrap();
    }
}
