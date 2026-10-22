//! The sign-ins the broker fills for signed-in profiles (WO-030, part 2),
//! sealed in a store of their own (`HATCHLING_BROWSER_CREDENTIALS_DIR`).
//!
//! - **Only the owner keeps one,** in the app, sent to the broker as its own
//!   request: never through a conversation, and no session op reaches this
//!   store, so a cell can't make, read or change a sign-in.
//! - **One profile, one exact origin.** A sign-in is kept for one of its
//!   profile's sites, as an `https` origin, and is filled only into that
//!   profile's browser, on that origin.
//! - **Listed by label.** The app is told a sign-in's label and origin,
//!   never its username or password, and the broker's log names it by its
//!   id and label.
//! - **Bounded:** at most [`MAX_CREDENTIALS`] sign-ins, each part within its
//!   bound; past them a new one is refused.
//! - **Deleting a profile deletes its sign-ins.**

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::SecretName;
use hatchling_core::ui::{Concealed, CredentialOp, CredentialRefusal, CredentialResult, SiteCredential};
use hatchling_egress::web::WebUrl;
use hatchling_vault::{OpensStores, SealedVault, Secret, Store};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::Config;
use crate::browser_profiles::{Profile, Profiles};

/// The most sign-ins the broker keeps.
pub const MAX_CREDENTIALS: usize = 64;

/// The most characters in a sign-in's label.
pub const MAX_LABEL_CHARS: usize = 64;

/// The most bytes in a sign-in's username.
pub const MAX_USERNAME_BYTES: usize = 512;

/// The most bytes in a sign-in's password.
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// The store's index: every sign-in, in the order kept.
const INDEX: &str = "credentials";

/// Serializes the store's writers within a broker.
static STORE: Mutex<()> = Mutex::new(());

/// A sign-in, as the store lists it: never its username or password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    /// Its id.
    pub id: String,
    /// The profile it signs in.
    pub profile_id: String,
    /// The exact origin it's filled on, `https://host[:port]`.
    pub origin: String,
    /// What the owner calls it.
    pub label: String,
    /// When it was kept, in seconds.
    pub created_at: i64,
}

/// A sign-in's secrets: never shown, and wiped when dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignIn {
    /// The username.
    pub username: Concealed,
    /// The password.
    pub password: Concealed,
}

/// The sign-ins the broker keeps, in the store `config` gives it.
pub struct Credentials<'k> {
    root: &'k Path,
    keys: &'k dyn OpensStores,
}

impl std::fmt::Debug for Credentials<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl<'k> Credentials<'k> {
    /// The store `config` gives the broker, opened with `keys`; `None` when
    /// the operator gave it none, and no sign-in is kept.
    #[must_use]
    pub fn of(config: &'k Config, keys: &'k dyn OpensStores) -> Option<Self> {
        let root = config.web.credentials.as_path();
        (!root.as_os_str().is_empty()).then_some(Self { root, keys })
    }

    fn vault(&self) -> Result<SealedVault<'_>, String> {
        SealedVault::open_in(self.root, self.keys, Store::CREDENTIALS)
            .map_err(|error| format!("the sign-ins can't be opened: {error}"))
    }

    fn entry(name: &str) -> Result<SecretName, String> {
        name.parse()
            .map_err(|_| format!("the sign-in entry name {name:?} is invalid"))
    }

    fn read_index(vault: &SealedVault<'_>) -> Result<Vec<Credential>, String> {
        match vault.fetch(&Self::entry(INDEX)?) {
            Ok(secret) => serde_json::from_slice(secret.as_bytes())
                .map_err(|error| format!("the sign-ins are corrupt: {error}")),
            Err(hatchling_vault::Error::NotFound { .. }) => Ok(Vec::new()),
            Err(error) => Err(format!("the sign-ins can't be read: {error}")),
        }
    }

    fn write_index(vault: &SealedVault<'_>, credentials: &[Credential]) -> Result<(), String> {
        let bytes = serde_json::to_vec(credentials)
            .map_err(|error| format!("the sign-ins don't encode: {error}"))?;
        let secret =
            Secret::new(bytes).map_err(|error| format!("the sign-ins don't fit: {error}"))?;
        vault
            .store(&Self::entry(INDEX)?, &secret)
            .map_err(|error| format!("the sign-ins can't be written: {error}"))
    }

    /// Every sign-in, oldest first.
    ///
    /// # Errors
    ///
    /// Why the store can't be read.
    pub fn list(&self) -> Result<Vec<Credential>, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        Self::read_index(&self.vault()?)
    }

    /// Keeps `sign_in` for `profile` on `origin`, labelled `label`, from
    /// `now`: the sign-in as listed, or why it isn't kept.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub fn store(
        &self,
        profile: &Profile,
        origin: &str,
        label: &str,
        sign_in: &SignIn,
        now: i64,
    ) -> Result<Result<Credential, CredentialRefusal>, String> {
        let origin = match refusal(profile, origin, label, sign_in) {
            Ok(origin) => origin,
            Err(refused) => return Ok(Err(refused)),
        };
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut credentials = Self::read_index(&vault)?;
        if credentials.len() >= MAX_CREDENTIALS {
            return Ok(Err(CredentialRefusal::Full));
        }
        let id = crate::approval::random_hex("cred-")
            .map_err(|error| format!("a sign-in id can't be minted: {error}"))?;
        let bytes = Zeroizing::new(
            serde_json::to_vec(sign_in)
                .map_err(|error| format!("a sign-in doesn't encode: {error}"))?,
        );
        let secret = Secret::new(bytes.to_vec())
            .map_err(|error| format!("a sign-in doesn't fit: {error}"))?;
        vault
            .store(&Self::entry(&id)?, &secret)
            .map_err(|error| format!("a sign-in can't be written: {error}"))?;
        let credential = Credential {
            id,
            profile_id: profile.id.clone(),
            origin,
            label: label.trim().to_owned(),
            created_at: now,
        };
        credentials.push(credential.clone());
        Self::write_index(&vault, &credentials)?;
        tracing::info!(credential = %credential.id, label = %credential.label, origin = %credential.origin, "a sign-in is kept");
        Ok(Ok(credential))
    }

    /// The secrets of the sign-in `id`, for the fill alone.
    ///
    /// # Errors
    ///
    /// Why they can't be read: no such sign-in, or the store's failure.
    pub fn sign_in(&self, id: &str) -> Result<SignIn, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        if !Self::read_index(&vault)?.iter().any(|credential| credential.id == id) {
            return Err(format!("no sign-in {id}"));
        }
        let secret = vault
            .fetch(&Self::entry(id)?)
            .map_err(|error| format!("a sign-in can't be read: {error}"))?;
        serde_json::from_slice(secret.as_bytes())
            .map_err(|error| format!("a sign-in is corrupt: {error}"))
    }

    /// Deletes the sign-in `id`. Whether there was one.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub fn delete(&self, id: &str) -> Result<bool, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        Self::drop_where(&vault, |credential| credential.id == id).map(|dropped| dropped > 0)
    }

    /// Deletes every sign-in of the profile `profile_id`. How many there
    /// were.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub fn delete_profiles(&self, profile_id: &str) -> Result<usize, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        Self::drop_where(&vault, |credential| credential.profile_id == profile_id)
    }

    /// Drops every sign-in `gone` picks: its secrets, then its place in
    /// the index. How many.
    fn drop_where(
        vault: &SealedVault<'_>,
        gone: impl Fn(&Credential) -> bool,
    ) -> Result<usize, String> {
        let (dropped, kept): (Vec<Credential>, Vec<Credential>) =
            Self::read_index(vault)?.into_iter().partition(|credential| gone(credential));
        for credential in &dropped {
            match vault.remove(&Self::entry(&credential.id)?) {
                Ok(()) | Err(hatchling_vault::Error::NotFound { .. }) => {}
                Err(error) => return Err(format!("a sign-in can't be deleted: {error}")),
            }
        }
        if !dropped.is_empty() {
            Self::write_index(vault, &kept)?;
        }
        Ok(dropped.len())
    }
}

/// `raw` as an origin a sign-in is kept for, `https://host[:port]`: an
/// `https` URL with nothing past its origin, or `None`.
#[must_use]
pub fn origin_of(raw: &str) -> Option<String> {
    let url = WebUrl::parse(raw.trim()).ok()?;
    (url.target() == "/").then(|| url.origin().to_string())
}

/// What `origin` is as a sign-in for `profile`, labelled `label`: its
/// canonical origin, or why it isn't kept.
fn refusal(
    profile: &Profile,
    origin: &str,
    label: &str,
    sign_in: &SignIn,
) -> Result<String, CredentialRefusal> {
    let label = label.trim();
    if label.is_empty() || label.chars().any(char::is_control) {
        return Err(CredentialRefusal::BadLabel);
    }
    if sign_in.password.expose().is_empty() {
        return Err(CredentialRefusal::NoPassword);
    }
    if label.chars().count() > MAX_LABEL_CHARS
        || sign_in.username.expose().len() > MAX_USERNAME_BYTES
        || sign_in.password.expose().len() > MAX_PASSWORD_BYTES
    {
        return Err(CredentialRefusal::TooLong);
    }
    let canonical = origin_of(origin).ok_or(CredentialRefusal::BadOrigin)?;
    let url = WebUrl::parse(&canonical).map_err(|_| CredentialRefusal::BadOrigin)?;
    if !profile.holds(&url.origin().destination()) {
        return Err(CredentialRefusal::NotTheProfiles);
    }
    Ok(canonical)
}

/// `credential` as the owner's app is told it.
#[must_use]
pub fn shown(credential: &Credential) -> SiteCredential {
    SiteCredential {
        credential_id: credential.id.clone(),
        profile_id: credential.profile_id.clone(),
        origin: credential.origin.clone(),
        label: credential.label.clone(),
        created_at: hatchling_scheduler::Timestamp::from_second(credential.created_at)
            .map(|at| at.to_string())
            .unwrap_or_default(),
    }
}

/// The owner's `op` on the sign-ins, under `config`, with `keys`.
#[must_use]
pub fn handle_credential_op(
    config: &Config,
    keys: &dyn OpensStores,
    op: &CredentialOp,
) -> CredentialResult {
    let Some(credentials) = Credentials::of(config, keys) else {
        return CredentialResult::Off;
    };
    let answer = match op {
        CredentialOp::List => credentials.list().map(|listed| CredentialResult::Listed {
            credentials: listed.iter().map(shown).collect(),
        }),
        CredentialOp::Store {
            profile_id,
            origin,
            label,
            username,
            password,
        } => {
            let Some(profiles) = Profiles::of(config, keys) else {
                return CredentialResult::Off;
            };
            let sign_in = SignIn {
                username: username.clone(),
                password: password.clone(),
            };
            profiles.list().and_then(|listed| {
                let Some(profile) = listed.iter().find(|profile| &profile.id == profile_id) else {
                    return Ok(CredentialResult::Refused {
                        reason: CredentialRefusal::NoProfile,
                    });
                };
                let now = hatchling_scheduler::Timestamp::now().as_second();
                credentials
                    .store(profile, origin, label, &sign_in, now)
                    .map(|kept| match kept {
                        Ok(credential) => CredentialResult::Stored {
                            credential: shown(&credential),
                        },
                        Err(reason) => CredentialResult::Refused { reason },
                    })
            })
        }
        CredentialOp::Delete { credential_id } => credentials.delete(credential_id).map(|found| {
            if found {
                CredentialResult::Deleted
            } else {
                CredentialResult::NotFound
            }
        }),
    };
    answer.unwrap_or_else(|error| {
        tracing::error!(%error, "a sign-in operation failed");
        CredentialResult::Failed
    })
}

#[cfg(test)]
mod tests {
    use hatchling_core::ui::ProfileOp;
    use hatchling_vault::MasterKey;

    use super::*;

    /// A config keeping profiles and sign-ins in a scratch directory, the
    /// key sealing them, and a profile of `a.example` and its sign-in site.
    fn kept(name: &str) -> (Config, crate::api_auth::tests::Scratch, MasterKey, Profile) {
        let scratch = crate::api_auth::tests::scratch(&format!("credentials-{name}"));
        let mut config = crate::api_auth::tests::owner_config(None);
        config.web.profiles = scratch.0.join("profiles");
        config.web.credentials = scratch.0.join("credentials");
        let key = MasterKey::generate().unwrap();
        let profile = Profiles::of(&config, &key)
            .unwrap()
            .keep(&[String::from("a.example:443"), String::from("idp.example:443")], 1)
            .unwrap();
        (config, scratch, key, profile)
    }

    fn sign_in(username: &str, password: &str) -> SignIn {
        SignIn {
            username: Concealed::new(username.to_owned()),
            password: Concealed::new(password.to_owned()),
        }
    }

    fn store_op(profile_id: &str, origin: &str, label: &str, password: &str) -> CredentialOp {
        CredentialOp::Store {
            profile_id: profile_id.to_owned(),
            origin: origin.to_owned(),
            label: label.to_owned(),
            username: Concealed::new(String::from("ada@a.example")),
            password: Concealed::new(password.to_owned()),
        }
    }

    #[test]
    fn an_origin_is_an_https_origin_alone() {
        assert_eq!(origin_of("https://a.example"), Some(String::from("https://a.example")));
        assert_eq!(origin_of(" https://A.example/ "), Some(String::from("https://a.example")));
        assert_eq!(
            origin_of("https://a.example:8443"),
            Some(String::from("https://a.example:8443"))
        );
        for refused in [
            "http://a.example",
            "https://a.example/login",
            "https://a.example/?next=x",
            "https://user@a.example",
            "a.example",
            "",
        ] {
            assert_eq!(origin_of(refused), None, "{refused}");
        }
    }

    #[test]
    fn a_sign_in_is_kept_only_for_its_profiles_own_origin_and_within_its_bounds() {
        let (config, _scratch, key, profile) = kept("refusals");
        let credentials = Credentials::of(&config, &key).unwrap();
        let store = |origin: &str, label: &str, sign: &SignIn| {
            credentials.store(&profile, origin, label, sign, 1).unwrap()
        };
        let good = sign_in("ada", "pw");
        for (origin, label, sign, reason) in [
            ("https://b.example", "Work", &good, CredentialRefusal::NotTheProfiles),
            ("https://a.example:8443", "Work", &good, CredentialRefusal::NotTheProfiles),
            ("https://a.example/login", "Work", &good, CredentialRefusal::BadOrigin),
            ("http://a.example", "Work", &good, CredentialRefusal::BadOrigin),
            ("https://a.example", " ", &good, CredentialRefusal::BadLabel),
            ("https://a.example", "a\nb", &good, CredentialRefusal::BadLabel),
            ("https://a.example", "Work", &sign_in("ada", ""), CredentialRefusal::NoPassword),
            (
                "https://a.example",
                &"l".repeat(MAX_LABEL_CHARS + 1),
                &good,
                CredentialRefusal::TooLong,
            ),
            (
                "https://a.example",
                "Work",
                &sign_in(&"u".repeat(MAX_USERNAME_BYTES + 1), "pw"),
                CredentialRefusal::TooLong,
            ),
            (
                "https://a.example",
                "Work",
                &sign_in("ada", &"p".repeat(MAX_PASSWORD_BYTES + 1)),
                CredentialRefusal::TooLong,
            ),
        ] {
            assert_eq!(store(origin, label, sign), Err(reason), "{origin} {label:?}");
        }
        assert_eq!(credentials.list(), Ok(Vec::new()), "nothing kept");
        // At the bounds, kept: the sign-in site is the profile's too.
        let edge = sign_in(&"u".repeat(MAX_USERNAME_BYTES), &"p".repeat(MAX_PASSWORD_BYTES));
        let label = "l".repeat(MAX_LABEL_CHARS);
        assert!(store("https://idp.example/", &label, &edge).is_ok());
        assert_eq!(credentials.list().map(|listed| listed.len()), Ok(1));
    }

    #[test]
    fn a_kept_sign_in_is_listed_by_label_read_whole_and_deleted() {
        let (config, _scratch, key, profile) = kept("kept");
        let credentials = Credentials::of(&config, &key).unwrap();
        let kept = credentials
            .store(&profile, "https://a.example", " Work ", &sign_in("ada", "hunter2-secret"), 7)
            .unwrap()
            .unwrap();
        assert_eq!(
            (kept.profile_id.as_str(), kept.origin.as_str(), kept.label.as_str(), kept.created_at),
            (profile.id.as_str(), "https://a.example", "Work", 7)
        );
        assert!(kept.id.starts_with("cred-"), "{}", kept.id);
        assert_eq!(credentials.list(), Ok(vec![kept.clone()]));
        assert_eq!(credentials.sign_in(&kept.id), Ok(sign_in("ada", "hunter2-secret")));
        assert!(credentials.sign_in("cred-none").is_err());
        // Listed, the secrets are nowhere: not in the index, nor what the app is told.
        let listed = format!("{:?} {:?}", credentials.list(), shown(&kept));
        assert!(!listed.contains("hunter2") && !listed.contains("ada"), "{listed}");
        assert_eq!(shown(&kept).created_at, "1970-01-01T00:00:07Z");
        let index = std::fs::read_dir(config.web.credentials.as_path())
            .unwrap()
            .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
            .collect::<Vec<_>>();
        assert!(
            index
                .iter()
                .all(|bytes| !bytes.windows(7).any(|window| window == b"hunter2")),
            "sealed on disk"
        );
        assert_eq!(credentials.delete(&kept.id), Ok(true));
        assert_eq!(credentials.delete(&kept.id), Ok(false));
        assert!(credentials.sign_in(&kept.id).is_err(), "its secrets gone too");
        assert_eq!(credentials.list(), Ok(Vec::new()));
    }

    #[test]
    fn the_store_is_bounded_and_a_profiles_sign_ins_go_with_it() {
        let (config, _scratch, key, profile) = kept("bounds");
        let credentials = Credentials::of(&config, &key).unwrap();
        let other = Profiles::of(&config, &key)
            .unwrap()
            .keep(&[String::from("b.example:443")], 1)
            .unwrap();
        for n in 0..MAX_CREDENTIALS - 1 {
            let label = format!("Account {n}");
            assert!(
                credentials
                    .store(&profile, "https://a.example", &label, &sign_in("ada", "pw"), 1)
                    .unwrap()
                    .is_ok()
            );
        }
        assert!(
            credentials
                .store(&other, "https://b.example", "B", &sign_in("bo", "pw"), 1)
                .unwrap()
                .is_ok()
        );
        assert_eq!(
            credentials.store(&profile, "https://a.example", "One more", &sign_in("ada", "pw"), 1),
            Ok(Err(CredentialRefusal::Full))
        );
        assert_eq!(credentials.delete_profiles(&profile.id), Ok(MAX_CREDENTIALS - 1));
        assert_eq!(credentials.delete_profiles(&profile.id), Ok(0));
        let left = credentials.list().unwrap();
        assert_eq!(
            left.iter().map(|credential| credential.profile_id.as_str()).collect::<Vec<_>>(),
            [other.id.as_str()]
        );
    }

    #[test]
    fn the_owners_operations_keep_list_and_delete_and_are_off_without_a_store() {
        let (config, _scratch, key, profile) = kept("ops");
        let stored = handle_credential_op(
            &config,
            &key,
            &store_op(&profile.id, "https://a.example", "Work", "pw"),
        );
        let CredentialResult::Stored { credential } = stored else {
            unreachable!("{stored:?}");
        };
        assert_eq!(credential.origin, "https://a.example");
        assert_eq!(
            handle_credential_op(&config, &key, &CredentialOp::List),
            CredentialResult::Listed {
                credentials: vec![credential.clone()]
            }
        );
        for (op, outcome) in [
            (
                store_op("profile-none", "https://a.example", "Work", "pw"),
                CredentialResult::Refused {
                    reason: CredentialRefusal::NoProfile,
                },
            ),
            (
                store_op(&profile.id, "https://b.example", "Work", "pw"),
                CredentialResult::Refused {
                    reason: CredentialRefusal::NotTheProfiles,
                },
            ),
            (
                CredentialOp::Delete {
                    credential_id: credential.credential_id.clone(),
                },
                CredentialResult::Deleted,
            ),
            (
                CredentialOp::Delete {
                    credential_id: credential.credential_id.clone(),
                },
                CredentialResult::NotFound,
            ),
        ] {
            assert_eq!(handle_credential_op(&config, &key, &op), outcome, "{op:?}");
        }
        // Deleting the profile deletes its sign-ins.
        let again = handle_credential_op(
            &config,
            &key,
            &store_op(&profile.id, "https://a.example", "Work", "pw"),
        );
        assert!(matches!(again, CredentialResult::Stored { .. }), "{again:?}");
        let delete = ProfileOp::Delete {
            profile_id: profile.id.clone(),
        };
        assert_eq!(
            crate::browser_profiles::handle_profile_op(&config, &key, &delete),
            hatchling_core::ui::ProfileResult::Deleted
        );
        assert_eq!(
            handle_credential_op(&config, &key, &CredentialOp::List),
            CredentialResult::Listed {
                credentials: Vec::new()
            }
        );
        // No store, or no profiles: off. Another key: failed.
        let mut off = config.clone();
        off.web.credentials = std::path::PathBuf::new();
        assert_eq!(handle_credential_op(&off, &key, &CredentialOp::List), CredentialResult::Off);
        let mut no_profiles = config.clone();
        no_profiles.web.profiles = std::path::PathBuf::new();
        assert_eq!(
            handle_credential_op(
                &no_profiles,
                &key,
                &store_op("profile-1", "https://a.example", "Work", "pw")
            ),
            CredentialResult::Off
        );
        let other = MasterKey::generate().unwrap();
        assert_eq!(
            handle_credential_op(&config, &other, &CredentialOp::List),
            CredentialResult::Failed
        );
        assert!(format!("{:?}", Credentials::of(&config, &key).unwrap()).contains("Credentials"));
    }
}
