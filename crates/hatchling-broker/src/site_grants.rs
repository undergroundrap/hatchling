//! The sites the owner always allows the agent to read (WO-028): sealed in
//! a store of their own (`HATCHLING_WEB_GRANTS_DIR`), listed and revoked by
//! the owner alone.
//!
//! - **Only the owner adds one,** by choosing *Allow always* on a site's
//!   approval; no session op reaches this store, so a cell can't grant,
//!   list or revoke a site.
//! - **A site is an origin,** exactly as approved: a grant never covers a
//!   subdomain, another port, or another site.
//! - **Bounded:** at most [`MAX_SITES`]; past that a new one is refused, and
//!   nothing is dropped to make room.
//! - **Revoking one** takes effect at the next read. Only a session that
//!   holds the site for itself (it chose *Allow for task*, or *Allow
//!   always*) keeps it until it ends.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::ui::{AllowedSite, SiteOp, SiteResult};
use hatchling_core::SecretName;
use hatchling_scheduler::Timestamp;
use hatchling_vault::{OpensStores, SealedVault, Secret, Store};
use serde::{Deserialize, Serialize};

use crate::Config;

/// The most sites the owner may always allow: at the longest host names,
/// still within one sealed entry.
pub const MAX_SITES: usize = 100;

/// The store's one entry: every allowed site, in the order allowed.
const ENTRY: &str = "allowed-sites";

/// Serializes the store's writers within a broker.
static STORE: Mutex<()> = Mutex::new(());

/// One allowed site, as kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    site: String,
    allowed_at: i64,
}

/// The sites the owner always allows, in their sealed store.
pub struct SiteGrants<'k> {
    root: &'k Path,
    keys: &'k dyn OpensStores,
}

impl std::fmt::Debug for SiteGrants<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
            .map_err(|error| format!("the allowed sites can't be opened: {error}"))
    }

    fn name() -> Result<SecretName, String> {
        ENTRY
            .parse()
            .map_err(|_| String::from("the allowed sites' entry name is invalid"))
    }

    fn read(vault: &SealedVault<'_>) -> Result<Vec<Kept>, String> {
        match vault.fetch(&Self::name()?) {
            Ok(secret) => serde_json::from_slice(secret.as_bytes())
                .map_err(|error| format!("the allowed sites are corrupt: {error}")),
            Err(hatchling_vault::Error::NotFound { .. }) => Ok(Vec::new()),
            Err(error) => Err(format!("the allowed sites can't be read: {error}")),
        }
    }

    fn write(vault: &SealedVault<'_>, kept: &[Kept]) -> Result<(), String> {
        let bytes = serde_json::to_vec(kept)
            .map_err(|error| format!("the allowed sites don't encode: {error}"))?;
        let secret = Secret::new(bytes)
            .map_err(|error| format!("the allowed sites don't fit: {error}"))?;
        vault
            .store(&Self::name()?, &secret)
            .map_err(|error| format!("the allowed sites can't be written: {error}"))
    }

    /// Every allowed site, with when it was allowed, oldest first.
    ///
    /// # Errors
    ///
    /// Why the store can't be read.
    pub fn list(&self) -> Result<Vec<(String, i64)>, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(Self::read(&self.vault()?)?
            .into_iter()
            .map(|kept| (kept.site, kept.allowed_at))
            .collect())
    }

    /// Whether `site` is allowed.
    ///
    /// # Errors
    ///
    /// Why the store can't be read.
    pub fn holds(&self, site: &str) -> Result<bool, String> {
        Ok(self.list()?.iter().any(|(kept, _)| kept == site))
    }

    /// Allows `site` always, from `now`; one allowed already stays as it was.
    ///
    /// # Errors
    ///
    /// Why it can't be kept: the store's failure, or [`MAX_SITES`] kept.
    pub fn allow(&self, site: &str, now: i64) -> Result<(), String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut kept = Self::read(&vault)?;
        if kept.iter().any(|kept| kept.site == site) {
            return Ok(());
        }
        if kept.len() >= MAX_SITES {
            return Err(format!(
                "{MAX_SITES} sites are allowed already: revoke one first"
            ));
        }
        kept.push(Kept {
            site: site.to_owned(),
            allowed_at: now,
        });
        Self::write(&vault, &kept)
    }

    /// Revokes `site`. Whether it was allowed.
    ///
    /// # Errors
    ///
    /// Why the store can't be read or changed.
    pub fn revoke(&self, site: &str) -> Result<bool, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut kept = Self::read(&vault)?;
        let before = kept.len();
        kept.retain(|kept| kept.site != site);
        if kept.len() == before {
            return Ok(false);
        }
        Self::write(&vault, &kept)?;
        Ok(true)
    }
}

/// The owner's `op` on the allowed sites, under `config`, with `keys`.
#[must_use]
pub fn handle_site_op(config: &Config, keys: &dyn OpensStores, op: &SiteOp) -> SiteResult {
    let Some(grants) = SiteGrants::of(config, keys) else {
        return SiteResult::Off;
    };
    let answer = match op {
        SiteOp::List => grants.list().map(|sites| SiteResult::Listed {
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
        SiteOp::Revoke { site } => grants.revoke(site).map(|revoked| {
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

#[cfg(test)]
mod tests {
    use hatchling_vault::MasterKey;

    use super::*;

    /// A config whose allowed sites are kept in a scratch directory.
    fn kept(name: &str) -> (Config, crate::api_auth::tests::Scratch, MasterKey) {
        let scratch = crate::api_auth::tests::scratch(&format!("sites-{name}"));
        let mut config = crate::api_auth::tests::owner_config(None);
        config.web.grants = scratch.0.join("sites");
        (config, scratch, MasterKey::generate().unwrap())
    }

    /// A site is allowed once, listed oldest first, held exactly, and gone
    /// once revoked.
    #[test]
    fn a_site_is_allowed_listed_and_revoked() {
        let (config, _scratch, key) = kept("allow");
        let grants = SiteGrants::of(&config, &key).unwrap();
        assert_eq!(grants.list().unwrap(), []);
        grants.allow("https://a.example", 100).unwrap();
        grants.allow("https://b.example:8443", 200).unwrap();
        grants.allow("https://a.example", 300).unwrap();
        assert_eq!(
            grants.list().unwrap(),
            [
                (String::from("https://a.example"), 100),
                (String::from("https://b.example:8443"), 200)
            ],
            "allowed once, at first"
        );
        assert!(grants.holds("https://a.example").unwrap());
        assert!(!grants.holds("https://www.a.example").unwrap());
        assert!(!grants.holds("https://b.example").unwrap());
        assert!(grants.revoke("https://a.example").unwrap());
        assert!(!grants.revoke("https://a.example").unwrap());
        assert!(!grants.holds("https://a.example").unwrap());
        assert_eq!(grants.list().unwrap().len(), 1);
        // Sealed under the sites store, and kept across opens.
        let again = SiteGrants::of(&config, &key).unwrap();
        assert!(again.holds("https://b.example:8443").unwrap());
        assert!(format!("{again:?}").starts_with("SiteGrants { root: "));
    }

    /// At most [`MAX_SITES`]: past that a new one is refused, and nothing
    /// is dropped.
    #[test]
    fn the_sites_are_bounded_and_none_dropped() {
        let (config, _scratch, key) = kept("bound");
        let grants = SiteGrants::of(&config, &key).unwrap();
        let mut kept: Vec<Kept> = (0..MAX_SITES)
            .map(|n| Kept {
                site: format!("https://s{n}.example"),
                allowed_at: 1,
            })
            .collect();
        SiteGrants::write(&grants.vault().unwrap(), &kept).unwrap();
        assert!(grants.allow("https://new.example", 2).is_err());
        assert!(grants.allow("https://s0.example", 2).is_ok(), "one kept already");
        assert_eq!(grants.list().unwrap().len(), MAX_SITES);
        kept.pop();
        SiteGrants::write(&grants.vault().unwrap(), &kept).unwrap();
        assert!(grants.allow("https://new.example", 2).is_ok(), "room for one");
    }

    /// The store's parts, called directly: an empty store reads as no
    /// sites, and what is written is what is read.
    #[test]
    fn the_stores_parts() {
        let (config, _scratch, key) = kept("parts");
        let grants = SiteGrants::of(&config, &key).unwrap();
        let vault = grants.vault().unwrap();
        assert_eq!(SiteGrants::read(&vault).unwrap(), []);
        let one = vec![Kept {
            site: String::from("https://a.example"),
            allowed_at: 7,
        }];
        SiteGrants::write(&vault, &one).unwrap();
        assert_eq!(SiteGrants::read(&vault).unwrap(), one);
        assert_eq!(SiteGrants::name().unwrap().as_str(), ENTRY);
    }

    /// [`MAX_SITES`] of the longest sites still fit one sealed entry.
    #[test]
    fn the_longest_sites_fit_one_entry() {
        let host = format!("{}.example", "a".repeat(240));
        let kept: Vec<Kept> = (0..MAX_SITES)
            .map(|n| Kept {
                site: format!("https://{n:03}{host}:65535"),
                allowed_at: i64::MAX,
            })
            .collect();
        let bytes = serde_json::to_vec(&kept).unwrap();
        assert!(bytes.len() <= hatchling_core::MAX_SECRET_BYTES, "{}", bytes.len());
    }

    /// The owner's operations: listed with when, revoked, not found; off
    /// with no store; failed when the store can't be read.
    #[test]
    fn the_owners_operations() {
        let (config, _scratch, key) = kept("ops");
        assert_eq!(
            handle_site_op(&config, &key, &SiteOp::List),
            SiteResult::Listed { sites: Vec::new() }
        );
        SiteGrants::of(&config, &key)
            .unwrap()
            .allow("https://a.example", 1_790_000_000)
            .unwrap();
        assert_eq!(
            handle_site_op(&config, &key, &SiteOp::List),
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
        assert_eq!(handle_site_op(&config, &key, &revoke), SiteResult::Revoked);
        assert_eq!(handle_site_op(&config, &key, &revoke), SiteResult::NotFound);
        let mut off = config.clone();
        off.web.grants = std::path::PathBuf::new();
        assert!(SiteGrants::of(&off, &key).is_none());
        assert_eq!(handle_site_op(&off, &key, &SiteOp::List), SiteResult::Off);
        // Another key can't open the sealed store.
        let other = MasterKey::generate().unwrap();
        assert_eq!(
            handle_site_op(&config, &other, &SiteOp::List),
            SiteResult::Failed
        );
        let corrupt = SiteGrants::of(&config, &key).unwrap();
        corrupt
            .vault()
            .unwrap()
            .store(
                &SiteGrants::name().unwrap(),
                &Secret::new(b"not json".to_vec()).unwrap(),
            )
            .unwrap();
        assert!(corrupt.list().is_err());
        assert!(corrupt.allow("https://x.example", 1).is_err());
        assert!(corrupt.revoke("https://x.example").is_err());
    }
}
