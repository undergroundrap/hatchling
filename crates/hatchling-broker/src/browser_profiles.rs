//! Signed-in profiles (WO-030, part 1), sealed in a store of their own.
//!
//! A profile is a site the owner signed in to, with the sites its sign-in
//! went through, and its session kept as cookies
//! (`HATCHLING_BROWSER_PROFILES_DIR`).
//!
//! - **Only the owner makes one,** with *Keep me signed in* on a browser
//!   they drove and signed in on themselves: no session op reaches this
//!   store, so a cell can't make, read or change a profile.
//! - **A profile's cookies are its sites' alone.** A cookie is kept only if
//!   its domain is one of the profile's hosts, or a parent of one (a sign-in
//!   on `login.a.example` sets cookies for `.a.example`); any other is
//!   dropped as the jar is kept, and again as it's read back, and named in
//!   the log by its name and domain, never its value.
//! - **Nothing but cookies.** No profile directory, cache, service worker or
//!   storage is kept: every session starts from an empty profile, and a
//!   compromise lives as long as its session.
//! - **Bounded:** at most [`MAX_PROFILES`] profiles, [`MAX_PROFILE_SITES`]
//!   sites each, and [`MAX_JAR_COOKIES`] cookies in a jar; past them a new
//!   one is refused, and nothing is dropped to make room.
//! - **Signing out** drops a profile's jar, and keeps its sites; **deleting**
//!   drops both.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::SecretName;
use hatchling_core::browser::Cookie;
use hatchling_vault::{OpensStores, SealedVault, Secret, Store};
use serde::{Deserialize, Serialize};

use crate::Config;

/// The most profiles the owner may keep.
pub const MAX_PROFILES: usize = 32;

/// The most sites a profile holds: its own, and those its sign-in goes
/// through.
pub const MAX_PROFILE_SITES: usize = 16;

/// The most cookies a profile's jar keeps.
pub const MAX_JAR_COOKIES: usize = 256;

/// The most sealed entries a jar spans: each holds what fits one.
const MAX_JAR_ENTRIES: usize = 64;

/// The store's index: every profile, in the order made.
const INDEX: &str = "profiles";

/// Serializes the store's writers within a broker.
static STORE: Mutex<()> = Mutex::new(());

/// A signed-in profile: its sites, as `host:port`, the one signed in on
/// first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Its id, the broker's: `profile-` and random hex.
    pub id: String,
    /// Its sites.
    pub sites: Vec<String>,
    /// When it was made, in seconds since the epoch.
    pub created_at: i64,
    /// When its jar was last kept.
    pub last_used: i64,
    /// How many times the owner signed it out. A browser that took the
    /// profile before the latest keeps nothing more for it (WO-030).
    #[serde(default)]
    pub signed_out: u64,
}

impl Profile {
    /// Whether the profile holds `site`, a `host:port`.
    #[must_use]
    pub fn holds(&self, site: &str) -> bool {
        self.sites.iter().any(|held| held == site)
    }
}

/// Whether `cookie` is one of `sites`' (each `host:port`): its domain, less
/// a leading dot, is one of their hosts or a parent of one, and has a dot
/// of its own (no top-level domain).
#[must_use]
pub fn kept_for(cookie: &Cookie, sites: &[String]) -> bool {
    let domain = cookie.domain.trim_start_matches('.').to_ascii_lowercase();
    if !domain.contains('.') {
        return false;
    }
    let under = format!(".{domain}");
    sites.iter().any(|site| {
        let host = site
            .rsplit_once(':')
            .map_or(site.as_str(), |(host, _)| host)
            .to_ascii_lowercase();
        host == domain || host.ends_with(&under)
    })
}

/// The owner's signed-in profiles, in their sealed store.
pub struct Profiles<'k> {
    root: &'k Path,
    keys: &'k dyn OpensStores,
}

impl std::fmt::Debug for Profiles<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Profiles")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl<'k> Profiles<'k> {
    /// The store `config` gives the broker, opened with `keys`; `None` when
    /// the operator gave it none, and no sign-in is kept.
    #[must_use]
    pub fn of(config: &'k Config, keys: &'k dyn OpensStores) -> Option<Self> {
        let root = config.web.profiles.as_path();
        (!root.as_os_str().is_empty()).then_some(Self { root, keys })
    }

    fn vault(&self) -> Result<SealedVault<'_>, String> {
        SealedVault::open_in(self.root, self.keys, Store::PROFILES)
            .map_err(|error| format!("the signed-in profiles can't be opened: {error}"))
    }

    fn entry(name: &str) -> Result<SecretName, String> {
        name.parse()
            .map_err(|_| format!("the profile entry name {name:?} is invalid"))
    }

    /// The `n`th entry of `id`'s jar.
    fn jar_entry(id: &str, n: usize) -> Result<SecretName, String> {
        Self::entry(&format!("jar-{id}-{n}"))
    }

    fn read_index(vault: &SealedVault<'_>) -> Result<Vec<Profile>, String> {
        match vault.fetch(&Self::entry(INDEX)?) {
            Ok(secret) => serde_json::from_slice(secret.as_bytes())
                .map_err(|error| format!("the signed-in profiles are corrupt: {error}")),
            Err(hatchling_vault::Error::NotFound { .. }) => Ok(Vec::new()),
            Err(error) => Err(format!("the signed-in profiles can't be read: {error}")),
        }
    }

    fn write_index(vault: &SealedVault<'_>, profiles: &[Profile]) -> Result<(), String> {
        let bytes = serde_json::to_vec(profiles)
            .map_err(|error| format!("the signed-in profiles don't encode: {error}"))?;
        let secret = Secret::new(bytes)
            .map_err(|error| format!("the signed-in profiles don't fit: {error}"))?;
        vault
            .store(&Self::entry(INDEX)?, &secret)
            .map_err(|error| format!("the signed-in profiles can't be written: {error}"))
    }

    /// Every profile, oldest first.
    ///
    /// # Errors
    ///
    /// Why the store can't be read.
    pub fn list(&self) -> Result<Vec<Profile>, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        Self::read_index(&self.vault()?)
    }

    /// The profile that holds `site`, if one does.
    ///
    /// # Errors
    ///
    /// Why the store can't be read.
    pub fn covering(&self, site: &str) -> Result<Option<Profile>, String> {
        Ok(self.list()?.into_iter().find(|profile| profile.holds(site)))
    }

    /// Keeps a profile of `sites`, the one signed in on first, from `now`.
    /// One holding that first site already gains the others, and is the
    /// one kept. The profile, as kept.
    ///
    /// # Errors
    ///
    /// Why it can't be kept: no sites, past [`MAX_PROFILE_SITES`] or
    /// [`MAX_PROFILES`], or the store's failure.
    pub fn keep(&self, sites: &[String], now: i64) -> Result<Profile, String> {
        let Some(first) = sites.first() else {
            return Err(String::from("a profile needs a site"));
        };
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut profiles = Self::read_index(&vault)?;
        let kept = if let Some(profile) = profiles.iter_mut().find(|profile| profile.holds(first)) {
            for site in sites {
                if !profile.holds(site) {
                    profile.sites.push(site.clone());
                }
            }
            if profile.sites.len() > MAX_PROFILE_SITES {
                return Err(format!("a profile holds at most {MAX_PROFILE_SITES} sites"));
            }
            profile.clone()
        } else {
            let mut held: Vec<String> = Vec::new();
            for site in sites {
                if !held.contains(site) {
                    held.push(site.clone());
                }
            }
            if held.len() > MAX_PROFILE_SITES {
                return Err(format!("a profile holds at most {MAX_PROFILE_SITES} sites"));
            }
            if profiles.len() >= MAX_PROFILES {
                return Err(format!(
                    "{MAX_PROFILES} profiles are kept already: delete one first"
                ));
            }
            let id = crate::approval::random_hex("profile-")
                .map_err(|error| format!("a profile id can't be minted: {error}"))?;
            let profile = Profile {
                id,
                sites: held,
                created_at: now,
                last_used: now,
                signed_out: 0,
            };
            profiles.push(profile.clone());
            profile
        };
        Self::write_index(&vault, &profiles)?;
        Ok(kept)
    }

    /// The jar kept for the profile `id`: only its sites' cookies, whatever
    /// the store held.
    ///
    /// # Errors
    ///
    /// Why it can't be read: no such profile, or the store's failure.
    pub fn jar(&self, id: &str) -> Result<Vec<Cookie>, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let profile = Self::read_index(&vault)?
            .into_iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| format!("no profile {id}"))?;
        let mut cookies = Vec::new();
        for n in 0..MAX_JAR_ENTRIES {
            match vault.fetch(&Self::jar_entry(id, n)?) {
                Ok(secret) => {
                    let page: Vec<Cookie> = serde_json::from_slice(secret.as_bytes())
                        .map_err(|error| format!("a profile's jar is corrupt: {error}"))?;
                    cookies.extend(page);
                }
                Err(hatchling_vault::Error::NotFound { .. }) => break,
                Err(error) => return Err(format!("a profile's jar can't be read: {error}")),
            }
        }
        Ok(only_its_own(&profile, cookies))
    }

    /// Keeps `cookies` as the profile `id`'s jar, from `now`: only its
    /// sites' cookies, at most [`MAX_JAR_COOKIES`] of them, sealed in as
    /// many entries as they need. The jar it replaces is gone. How many
    /// cookies weren't kept.
    ///
    /// `signed_out` is how many times the profile was signed out when the
    /// browser the jar is read from took it: once the owner signs it out
    /// again, that browser's session is no longer the profile's.
    ///
    /// # Errors
    ///
    /// Why it can't be kept: no such profile, the owner signed it out since
    /// the browser took it, or the store's failure.
    pub fn keep_jar(
        &self,
        id: &str,
        cookies: Vec<Cookie>,
        now: i64,
        signed_out: u64,
    ) -> Result<usize, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut profiles = Self::read_index(&vault)?;
        let profile = profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or_else(|| format!("no profile {id}"))?;
        if profile.signed_out != signed_out {
            return Err(String::from(
                "the owner signed the profile out since this browser took it",
            ));
        }
        let offered = cookies.len();
        let mut kept = only_its_own(profile, cookies);
        kept.truncate(MAX_JAR_COOKIES);
        let pages = pages_of(&kept)?;
        if pages.len() > MAX_JAR_ENTRIES {
            return Err(String::from("a profile's jar doesn't fit its store"));
        }
        for (n, page) in pages.iter().enumerate() {
            let secret = Secret::new(page.clone())
                .map_err(|error| format!("a profile's jar doesn't fit: {error}"))?;
            vault
                .store(&Self::jar_entry(id, n)?, &secret)
                .map_err(|error| format!("a profile's jar can't be written: {error}"))?;
        }
        Self::drop_jar_from(&vault, id, pages.len())?;
        profile.last_used = now;
        Self::write_index(&vault, &profiles)?;
        Ok(offered - kept.len())
    }

    /// Drops the entries of `id`'s jar from the `from`th on.
    fn drop_jar_from(vault: &SealedVault<'_>, id: &str, from: usize) -> Result<(), String> {
        for n in from..MAX_JAR_ENTRIES {
            match vault.remove(&Self::jar_entry(id, n)?) {
                Ok(()) => {}
                Err(hatchling_vault::Error::NotFound { .. }) => break,
                Err(error) => return Err(format!("a profile's jar can't be dropped: {error}")),
            }
        }
        Ok(())
    }

    /// Signs the profile `id` out: its jar is dropped, its sites kept, and
    /// a browser still running it keeps nothing more for it. Whether there
    /// was such a profile.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub fn sign_out(&self, id: &str) -> Result<bool, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut profiles = Self::read_index(&vault)?;
        let Some(profile) = profiles.iter_mut().find(|profile| profile.id == id) else {
            return Ok(false);
        };
        // Counted before the jar goes: a browser that took the profile
        // before can't keep its session again, even if the drop fails.
        profile.signed_out = profile.signed_out.saturating_add(1);
        Self::write_index(&vault, &profiles)?;
        Self::drop_jar_from(&vault, id, 0)?;
        Ok(true)
    }

    /// Deletes the profile `id`: its jar and its sites. Whether there was
    /// such a profile.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub fn delete(&self, id: &str) -> Result<bool, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut profiles = Self::read_index(&vault)?;
        let before = profiles.len();
        profiles.retain(|profile| profile.id != id);
        if profiles.len() == before {
            return Ok(false);
        }
        Self::drop_jar_from(&vault, id, 0)?;
        Self::write_index(&vault, &profiles)?;
        Ok(true)
    }
}

/// `profile` as the owner's app is told it, `signed_in` if a session is
/// kept for it.
#[must_use]
pub fn shown(profile: &Profile, signed_in: bool) -> hatchling_core::ui::SignedInProfile {
    let when = |at: i64| {
        hatchling_scheduler::Timestamp::from_second(at)
            .map(|at| at.to_string())
            .unwrap_or_default()
    };
    hatchling_core::ui::SignedInProfile {
        profile_id: profile.id.clone(),
        sites: profile.sites.clone(),
        signed_in,
        created_at: when(profile.created_at),
        last_used: when(profile.last_used),
    }
}

/// The owner's `op` on the signed-in profiles, under `config`, with `keys`.
#[must_use]
pub fn handle_profile_op(
    config: &Config,
    keys: &dyn OpensStores,
    op: &hatchling_core::ui::ProfileOp,
) -> hatchling_core::ui::ProfileResult {
    use hatchling_core::ui::{ProfileOp, ProfileResult};
    let Some(profiles) = Profiles::of(config, keys) else {
        return ProfileResult::Off;
    };
    let gone = |found: bool, done: ProfileResult| {
        if found { done } else { ProfileResult::NotFound }
    };
    let answer = match op {
        ProfileOp::List => profiles.list().and_then(|listed| {
            listed
                .iter()
                .map(|profile| {
                    profiles
                        .jar(&profile.id)
                        .map(|jar| shown(profile, !jar.is_empty()))
                })
                .collect::<Result<Vec<_>, String>>()
                .map(|profiles| ProfileResult::Listed { profiles })
        }),
        ProfileOp::SignOut { profile_id } => profiles
            .sign_out(profile_id)
            .map(|found| gone(found, ProfileResult::SignedOut)),
        ProfileOp::Delete { profile_id } => profiles.delete(profile_id).and_then(|found| {
            // Its sign-ins go with it: none is left for a profile gone.
            if let Some(credentials) = crate::browser_credentials::Credentials::of(config, keys) {
                credentials.delete_profiles(profile_id)?;
            }
            Ok(gone(found, ProfileResult::Deleted))
        }),
    };
    answer.unwrap_or_else(|error| {
        tracing::error!(%error, "a profile operation failed");
        ProfileResult::Failed
    })
}

/// `cookies`, only those of `profile`'s sites; each other is logged, by
/// its name and domain, as dropped.
fn only_its_own(profile: &Profile, cookies: Vec<Cookie>) -> Vec<Cookie> {
    cookies
        .into_iter()
        .filter(|cookie| {
            let own = kept_for(cookie, &profile.sites);
            if !own {
                tracing::warn!(
                    profile = %profile.id,
                    name = %cookie.name,
                    domain = %cookie.domain,
                    "a cookie for a site outside the profile: dropped"
                );
            }
            own
        })
        .collect()
}

/// `cookies` as sealed entries, each a JSON list that fits one.
fn pages_of(cookies: &[Cookie]) -> Result<Vec<Vec<u8>>, String> {
    let mut pages = Vec::new();
    let mut page: Vec<&Cookie> = Vec::new();
    for cookie in cookies {
        page.push(cookie);
        let fits = serde_json::to_vec(&page)
            .map_err(|error| format!("a profile's jar doesn't encode: {error}"))?
            .len()
            <= hatchling_core::MAX_SECRET_BYTES;
        if !fits {
            page.pop();
            pages.push(
                serde_json::to_vec(&page)
                    .map_err(|error| format!("a profile's jar doesn't encode: {error}"))?,
            );
            page = vec![cookie];
        }
    }
    if !page.is_empty() {
        pages.push(
            serde_json::to_vec(&page)
                .map_err(|error| format!("a profile's jar doesn't encode: {error}"))?,
        );
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use hatchling_core::browser::{MAX_COOKIE_BYTES, SameSite};
    use hatchling_vault::MasterKey;

    use super::*;

    /// A config whose profiles are kept in a scratch directory.
    fn kept(name: &str) -> (Config, crate::api_auth::tests::Scratch, MasterKey) {
        let scratch = crate::api_auth::tests::scratch(&format!("profiles-{name}"));
        let mut config = crate::api_auth::tests::owner_config(None);
        config.web.profiles = scratch.0.join("profiles");
        (config, scratch, MasterKey::generate().unwrap())
    }

    fn cookie(domain: &str, name: &str, value: &str) -> Cookie {
        Cookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: domain.to_owned(),
            path: String::from("/"),
            expires: Some(1_900_000_000),
            secure: true,
            http_only: true,
            same_site: Some(SameSite::Lax),
        }
    }

    fn sites(sites: &[&str]) -> Vec<String> {
        sites.iter().map(|site| (*site).to_owned()).collect()
    }

    #[test]
    fn a_cookie_is_a_profiles_only_for_its_hosts_and_their_parents() {
        let held = sites(&[
            "login.a.example:443",
            "www.a.example:443",
            "idp.example:443",
        ]);
        for (domain, own) in [
            ("login.a.example", true),
            (".a.example", true),
            ("a.example", true),
            ("A.EXAMPLE", true),
            ("idp.example", true),
            (".idp.example", true),
            ("example", false),
            (".example", false),
            ("evil.example", false),
            ("b.login.a.example", false),
            ("other.a.example", false),
            ("xa.example", false),
            ("", false),
        ] {
            assert_eq!(kept_for(&cookie(domain, "n", "v"), &held), own, "{domain}");
        }
        assert!(!kept_for(&cookie("a.example", "n", "v"), &[]));
        assert!(
            kept_for(&cookie("a.example", "n", "v"), &sites(&["a.example"])),
            "a site with no port"
        );
    }

    #[test]
    fn a_profile_is_kept_once_for_its_first_site_and_listed() {
        let (config, _scratch, key) = kept("keep");
        let profiles = Profiles::of(&config, &key).unwrap();
        assert_eq!(profiles.list().unwrap(), []);
        assert!(profiles.keep(&[], 1).is_err(), "no site");
        let first = profiles
            .keep(
                &sites(&["a.example:443", "idp.example:443", "a.example:443"]),
                100,
            )
            .unwrap();
        assert!(first.id.starts_with("profile-"), "{}", first.id);
        assert_eq!(
            first.sites,
            sites(&["a.example:443", "idp.example:443"]),
            "each once"
        );
        assert_eq!((first.created_at, first.last_used), (100, 100));
        // Kept again for the same site: the same profile, gaining sites.
        let again = profiles
            .keep(&sites(&["a.example:443", "cdn.a.example:443"]), 200)
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(
            again.sites,
            sites(&["a.example:443", "idp.example:443", "cdn.a.example:443"])
        );
        let other = profiles.keep(&sites(&["b.example:443"]), 300).unwrap();
        assert_ne!(other.id, first.id);
        assert_eq!(profiles.list().unwrap(), [again.clone(), other.clone()]);
        assert_eq!(profiles.covering("idp.example:443").unwrap(), Some(again));
        assert_eq!(profiles.covering("b.example:443").unwrap(), Some(other));
        assert_eq!(profiles.covering("c.example:443").unwrap(), None);
        // Sealed, and kept across opens.
        assert_eq!(
            Profiles::of(&config, &key).unwrap().list().unwrap().len(),
            2
        );
        let shown = format!("{profiles:?}");
        assert!(shown.starts_with("Profiles { root: "), "{shown}");
        assert!(Profiles::of(&crate::api_auth::tests::owner_config(None), &key).is_none());
    }

    #[test]
    fn the_profiles_are_bounded_and_none_dropped() {
        let (config, _scratch, key) = kept("bound");
        let profiles = Profiles::of(&config, &key).unwrap();
        let many: Vec<String> = (0..=MAX_PROFILE_SITES)
            .map(|n| format!("s{n}.example:443"))
            .collect();
        assert!(profiles.keep(&many, 1).is_err(), "too many sites");
        let full: Vec<String> = many.iter().take(MAX_PROFILE_SITES).cloned().collect();
        let profile = profiles.keep(&full, 1).unwrap();
        assert_eq!(profile.sites.len(), MAX_PROFILE_SITES);
        assert!(
            profiles
                .keep(&sites(&["s0.example:443", "more.example:443"]), 2)
                .is_err(),
            "no room for another site"
        );
        let listed = profiles.list().unwrap();
        assert_eq!(
            listed.first().map(|p| p.sites.len()),
            Some(MAX_PROFILE_SITES)
        );
        for n in 1..MAX_PROFILES {
            profiles.keep(&[format!("p{n}.example:443")], 1).unwrap();
        }
        assert!(profiles.keep(&sites(&["new.example:443"]), 1).is_err());
        assert_eq!(profiles.list().unwrap().len(), MAX_PROFILES);
        assert!(profiles.delete(&profile.id).unwrap());
        assert!(
            profiles.keep(&sites(&["new.example:443"]), 1).is_ok(),
            "room for one"
        );
    }

    #[test]
    fn a_jar_keeps_only_its_profiles_cookies_across_entries_and_replaces_the_last() {
        let (config, _scratch, key) = kept("jar");
        let profiles = Profiles::of(&config, &key).unwrap();
        let profile = profiles.keep(&sites(&["login.a.example:443"]), 1).unwrap();
        assert_eq!(profiles.jar(&profile.id).unwrap(), [], "nothing kept yet");
        // Big enough to span several sealed entries, with a planted cookie.
        let mut jar: Vec<Cookie> = (0..40)
            .map(|n| {
                cookie(
                    ".a.example",
                    &format!("c{n:02}"),
                    &"v".repeat(MAX_COOKIE_BYTES - 8),
                )
            })
            .collect();
        jar.push(cookie("evil.example", "planted", "x"));
        assert_eq!(
            profiles.keep_jar(&profile.id, jar.clone(), 50, 0).unwrap(),
            1
        );
        let read = profiles.jar(&profile.id).unwrap();
        jar.pop();
        assert_eq!(read, jar);
        assert_eq!(
            profiles.list().unwrap().first().map(|p| p.last_used),
            Some(50)
        );
        // A smaller jar replaces it whole: nothing of the last outlives it.
        let small = vec![cookie("login.a.example", "sid", "2")];
        assert_eq!(
            profiles
                .keep_jar(&profile.id, small.clone(), 60, 0)
                .unwrap(),
            0
        );
        assert_eq!(profiles.jar(&profile.id).unwrap(), small);
        let vault = profiles.vault().unwrap();
        assert!(!vault.exists(&Profiles::jar_entry(&profile.id, 1).unwrap()));
        // At most MAX_JAR_COOKIES kept.
        let many: Vec<Cookie> = (0..=MAX_JAR_COOKIES)
            .map(|n| cookie("login.a.example", &format!("m{n}"), "1"))
            .collect();
        assert_eq!(profiles.keep_jar(&profile.id, many, 70, 0).unwrap(), 1);
        assert_eq!(profiles.jar(&profile.id).unwrap().len(), MAX_JAR_COOKIES);
        assert!(profiles.keep_jar("profile-none", Vec::new(), 1, 0).is_err());
        assert!(profiles.jar("profile-none").is_err());
    }

    #[test]
    fn a_planted_cookie_in_the_store_is_never_read_back() {
        let (config, _scratch, key) = kept("planted");
        let profiles = Profiles::of(&config, &key).unwrap();
        let profile = profiles.keep(&sites(&["a.example:443"]), 1).unwrap();
        let planted = serde_json::to_vec(&[
            cookie("a.example", "sid", "1"),
            cookie("b.example", "x", "2"),
        ])
        .unwrap();
        profiles
            .vault()
            .unwrap()
            .store(
                &Profiles::jar_entry(&profile.id, 0).unwrap(),
                &Secret::new(planted).unwrap(),
            )
            .unwrap();
        assert_eq!(
            profiles.jar(&profile.id).unwrap(),
            [cookie("a.example", "sid", "1")]
        );
    }

    #[test]
    fn signing_out_drops_the_jar_and_deleting_drops_the_profile() {
        let (config, _scratch, key) = kept("out");
        let profiles = Profiles::of(&config, &key).unwrap();
        let profile = profiles.keep(&sites(&["a.example:443"]), 1).unwrap();
        profiles
            .keep_jar(&profile.id, vec![cookie("a.example", "sid", "1")], 2, 0)
            .unwrap();
        assert!(profiles.sign_out(&profile.id).unwrap());
        assert_eq!(profiles.jar(&profile.id).unwrap(), []);
        assert_eq!(
            profiles
                .covering("a.example:443")
                .unwrap()
                .map(|kept| (kept.id, kept.sites)),
            Some((profile.id.clone(), profile.sites.clone())),
            "its sites kept"
        );
        assert!(!profiles.sign_out("profile-none").unwrap());
        // A browser that took the profile before the sign-out keeps nothing
        // more for it: only one that took it since does.
        let since = profiles.covering("a.example:443").unwrap().unwrap();
        assert_eq!(since.signed_out, 1);
        let stale = profiles.keep_jar(&profile.id, vec![cookie("a.example", "sid", "1")], 3, 0);
        assert!(stale.unwrap_err().contains("signed the profile out"));
        assert_eq!(profiles.jar(&profile.id).unwrap(), [], "still signed out");
        profiles
            .keep_jar(&profile.id, vec![cookie("a.example", "sid", "1")], 3, 1)
            .unwrap();
        assert!(profiles.sign_out(&profile.id).unwrap());
        assert_eq!(
            profiles
                .covering("a.example:443")
                .unwrap()
                .unwrap()
                .signed_out,
            2,
            "each sign-out counts"
        );
        assert!(profiles.delete(&profile.id).unwrap());
        assert!(!profiles.delete(&profile.id).unwrap());
        assert_eq!(profiles.list().unwrap(), []);
        let vault = profiles.vault().unwrap();
        assert!(
            !vault.exists(&Profiles::jar_entry(&profile.id, 0).unwrap()),
            "the jar went with it"
        );
    }

    #[test]
    fn the_stores_parts() {
        let (config, _scratch, key) = kept("parts");
        let profiles = Profiles::of(&config, &key).unwrap();
        let vault = profiles.vault().unwrap();
        assert_eq!(Profiles::read_index(&vault).unwrap(), []);
        let one = vec![Profile {
            id: String::from("profile-1"),
            sites: sites(&["a.example:443"]),
            created_at: 1,
            last_used: 2,
            signed_out: 3,
        }];
        Profiles::write_index(&vault, &one).unwrap();
        assert_eq!(Profiles::read_index(&vault).unwrap(), one);
        // An index from before sign-outs were counted reads as never
        // signed out.
        let older =
            br#"[{"id":"profile-1","sites":["a.example:443"],"created_at":1,"last_used":2}]"#;
        vault
            .store(
                &Profiles::entry(INDEX).unwrap(),
                &Secret::new(older.to_vec()).unwrap(),
            )
            .unwrap();
        let older = Profiles::read_index(&vault).unwrap();
        let counts: Vec<u64> = older.iter().map(|profile| profile.signed_out).collect();
        assert_eq!(counts, [0], "an older index reads as never signed out");
        assert_eq!(
            Profiles::jar_entry("profile-1", 3).unwrap().as_str(),
            "jar-profile-1-3"
        );
        assert!(Profiles::entry("bad name/").is_err());
        Profiles::drop_jar_from(&vault, "profile-1", 0).unwrap();
        // A page fits one sealed entry, and a jar spans as many as it needs.
        let big: Vec<Cookie> = (0..20)
            .map(|n| {
                cookie(
                    "a.example",
                    &format!("c{n}"),
                    &"v".repeat(MAX_COOKIE_BYTES - 8),
                )
            })
            .collect();
        let pages = pages_of(&big).unwrap();
        assert!(pages.len() > 1);
        assert!(
            pages
                .iter()
                .all(|page| page.len() <= hatchling_core::MAX_SECRET_BYTES)
        );
        let back: Vec<Cookie> = pages
            .iter()
            .flat_map(|page| serde_json::from_slice::<Vec<Cookie>>(page).unwrap())
            .collect();
        assert_eq!(back, big);
        assert_eq!(pages_of(&[]).unwrap(), Vec::<Vec<u8>>::new());
        let corrupt = Profiles::of(&config, &key).unwrap();
        corrupt
            .vault()
            .unwrap()
            .store(
                &Profiles::entry(INDEX).unwrap(),
                &Secret::new(b"not json".to_vec()).unwrap(),
            )
            .unwrap();
        assert!(corrupt.list().is_err());
        assert!(corrupt.keep(&sites(&["x.example:443"]), 1).is_err());
        assert!(corrupt.sign_out("profile-1").is_err());
        assert!(corrupt.delete("profile-1").is_err());
        // Another key can't open the sealed store.
        let other = MasterKey::generate().unwrap();
        assert!(Profiles::of(&config, &other).unwrap().list().is_err());
    }

    #[test]
    fn the_owners_operations_list_sign_out_and_delete() {
        use hatchling_core::ui::{ProfileOp, ProfileResult};
        let (config, _scratch, key) = kept("ops");
        assert_eq!(
            handle_profile_op(&config, &key, &ProfileOp::List),
            ProfileResult::Listed {
                profiles: Vec::new()
            }
        );
        let profiles = Profiles::of(&config, &key).unwrap();
        let profile = profiles
            .keep(&sites(&["a.example:443"]), 1_790_000_000)
            .unwrap();
        profiles
            .keep_jar(
                &profile.id,
                vec![cookie("a.example", "sid", "1")],
                1_790_000_060,
                0,
            )
            .unwrap();
        let listed = handle_profile_op(&config, &key, &ProfileOp::List);
        assert_eq!(
            listed,
            ProfileResult::Listed {
                profiles: vec![hatchling_core::ui::SignedInProfile {
                    profile_id: profile.id.clone(),
                    sites: sites(&["a.example:443"]),
                    signed_in: true,
                    created_at: String::from("2026-09-21T14:13:20Z"),
                    last_used: String::from("2026-09-21T14:14:20Z"),
                }]
            }
        );
        let wire = serde_json::to_string(&listed).unwrap();
        assert!(!wire.contains("sid"), "no cookie in the list: {wire}");
        let out = ProfileOp::SignOut {
            profile_id: profile.id.clone(),
        };
        assert_eq!(
            handle_profile_op(&config, &key, &out),
            ProfileResult::SignedOut
        );
        let ProfileResult::Listed { profiles: after } =
            handle_profile_op(&config, &key, &ProfileOp::List)
        else {
            unreachable!("listed");
        };
        assert_eq!(
            after.iter().map(|p| p.signed_in).collect::<Vec<_>>(),
            [false]
        );
        let delete = ProfileOp::Delete {
            profile_id: profile.id,
        };
        assert_eq!(
            handle_profile_op(&config, &key, &delete),
            ProfileResult::Deleted
        );
        assert_eq!(
            handle_profile_op(&config, &key, &delete),
            ProfileResult::NotFound
        );
        assert_eq!(
            handle_profile_op(&config, &key, &out),
            ProfileResult::NotFound
        );
        let off = crate::api_auth::tests::owner_config(None);
        assert_eq!(
            handle_profile_op(&off, &key, &ProfileOp::List),
            ProfileResult::Off
        );
        let other = MasterKey::generate().unwrap();
        assert_eq!(
            handle_profile_op(&config, &other, &ProfileOp::List),
            ProfileResult::Failed
        );
    }

    #[test]
    fn only_a_profiles_own_cookies_are_kept() {
        let profile = Profile {
            id: String::from("profile-1"),
            sites: sites(&["a.example:443"]),
            created_at: 1,
            last_used: 1,
            signed_out: 0,
        };
        let kept = only_its_own(
            &profile,
            vec![
                cookie("a.example", "sid", "1"),
                cookie("b.example", "x", "s3cret"),
            ],
        );
        assert_eq!(kept, [cookie("a.example", "sid", "1")]);
        assert!(profile.holds("a.example:443"));
        assert!(!profile.holds("a.example:8443"));
    }
}
