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
//! - **The keeper's (WO-031, slice 2).** The store and its key are the
//!   vault keeper's. The broker checks a new sign-in is for one of its
//!   profile's sites, and asks the keeper to keep, list and delete; it
//!   holds no key to the store, and a bug that discloses its memory or its
//!   log finds none there. Until a browser's cell is given a fill itself
//!   (part 2b), the broker asks for one sign-in's secrets to fill, and the
//!   keeper records each such read in the vault's access log.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::SecretName;
use hatchling_core::ui::{Concealed, SignInOp, SignInRefusal, SignInResult, SiteSignIn};
use hatchling_egress::web::WebUrl;
use hatchling_vault::{OpensStores, SealedVault, Secret, Store};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::Config;
use crate::browser_profiles::{Profile, Profiles};
use crate::keeper::Keeper;

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

/// What the broker asks the keeper of the sign-ins it holds (WO-031): the
/// owner's keep, list and delete, a profile's going, and one sign-in's
/// secrets for a fill.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SignInRequest {
    /// Every sign-in, by label.
    List,
    /// Keep `sign_in` for the profile `profile_id` on `origin`, a site of
    /// the profile's, as the broker checked.
    Keep {
        /// The profile it signs in.
        profile_id: String,
        /// The exact origin it's filled on.
        origin: String,
        /// What the owner calls it.
        label: String,
        /// Its secrets.
        sign_in: SignIn,
        /// When, in seconds.
        now: i64,
    },
    /// The secrets of the sign-in `id`, for a fill.
    Read {
        /// The sign-in.
        id: String,
    },
    /// Delete the sign-in `id`.
    Delete {
        /// The sign-in.
        id: String,
    },
    /// Delete every sign-in of the profile `profile_id`.
    DeleteProfiles {
        /// The profile.
        profile_id: String,
    },
}

/// The keeper's answer to a [`SignInRequest`]. A read's secrets follow it,
/// the username and then the password, each in a frame of its own: never
/// in the answer.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SignInAnswer {
    /// Every sign-in, by label.
    Listed {
        /// They, oldest first.
        sign_ins: Vec<Credential>,
    },
    /// The sign-in kept, as listed.
    Kept {
        /// It.
        sign_in: Credential,
    },
    /// Not kept, and why.
    Refused {
        /// Why.
        reason: SignInRefusal,
    },
    /// The sign-in's secrets follow.
    Read,
    /// How many sign-ins went.
    Deleted {
        /// How many.
        count: usize,
    },
    /// The store's failure.
    Failed {
        /// Why.
        reason: String,
    },
}

/// The sign-ins the broker keeps, in the store `config` gives it: the
/// keeper's, when one runs.
pub struct Credentials<'k> {
    root: &'k Path,
    keys: &'k dyn OpensStores,
    /// The keeper that holds them, when one runs: the store is then its
    /// own, and `keys` hold no key to it.
    keeper: Option<&'k Keeper>,
}

impl std::fmt::Debug for Credentials<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("root", &self.root)
            .field("kept", &self.keeper.is_some())
            .finish_non_exhaustive()
    }
}

impl<'k> Credentials<'k> {
    /// The store `config` gives the broker: the keeper's, when this
    /// broker started one, or opened with `keys`; `None` when the operator
    /// gave it none, and no sign-in is kept.
    #[must_use]
    pub fn of(config: &'k Config, keys: &'k dyn OpensStores) -> Option<Self> {
        Self::through(crate::keeper::keeper(), config, keys)
    }

    /// The store `config` gives, held by `keeper`, which is asked for
    /// every sign-in; without one, opened here with `keys`, as the keeper
    /// opens it. `None` when the operator gave it none.
    #[must_use]
    pub fn through(
        keeper: Option<&'k Keeper>,
        config: &'k Config,
        keys: &'k dyn OpensStores,
    ) -> Option<Self> {
        let root = config.web.credentials.as_path();
        (!root.as_os_str().is_empty()).then_some(Self { root, keys, keeper })
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
        if let Some(keeper) = self.keeper {
            return match asked(keeper, SignInRequest::List)? {
                (SignInAnswer::Listed { sign_ins }, _) => Ok(sign_ins),
                (other, _) => Err(out_of_turn(&other)),
            };
        }
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
    ) -> Result<Result<Credential, SignInRefusal>, String> {
        match refusal(profile, origin, label, sign_in) {
            Ok(origin) => self.keep(&profile.id, &origin, label, sign_in, now),
            Err(refused) => Ok(Err(refused)),
        }
    }

    /// Keeps `sign_in` for the profile `profile_id` on `origin`, labelled
    /// `label`, from `now`, the profile's own site, as the broker checked:
    /// the keeper's keep, which checks the rest again.
    ///
    /// # Errors
    ///
    /// The store's failure.
    fn keep(
        &self,
        profile_id: &str,
        origin: &str,
        label: &str,
        sign_in: &SignIn,
        now: i64,
    ) -> Result<Result<Credential, SignInRefusal>, String> {
        let origin = match shape(origin, label, sign_in) {
            Ok(origin) => origin,
            Err(refused) => return Ok(Err(refused)),
        };
        if let Some(keeper) = self.keeper {
            let request = SignInRequest::Keep {
                profile_id: profile_id.to_owned(),
                origin,
                label: label.to_owned(),
                sign_in: sign_in.clone(),
                now,
            };
            return match asked(keeper, request)? {
                (SignInAnswer::Kept { sign_in }, _) => Ok(Ok(sign_in)),
                (SignInAnswer::Refused { reason }, _) => Ok(Err(reason)),
                (other, _) => Err(out_of_turn(&other)),
            };
        }
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut credentials = Self::read_index(&vault)?;
        if credentials.len() >= MAX_CREDENTIALS {
            return Ok(Err(SignInRefusal::Full));
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
            profile_id: profile_id.to_owned(),
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
        if let Some(keeper) = self.keeper {
            let request = SignInRequest::Read { id: id.to_owned() };
            return match asked(keeper, request)? {
                (SignInAnswer::Read, Some(sign_in)) => Ok(sign_in),
                (other, _) => Err(out_of_turn(&other)),
            };
        }
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        if !Self::read_index(&vault)?
            .iter()
            .any(|credential| credential.id == id)
        {
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
        if let Some(keeper) = self.keeper {
            let request = SignInRequest::Delete { id: id.to_owned() };
            return deleted(asked(keeper, request)?).map(|count| count > 0);
        }
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
        if let Some(keeper) = self.keeper {
            let request = SignInRequest::DeleteProfiles {
                profile_id: profile_id.to_owned(),
            };
            return deleted(asked(keeper, request)?);
        }
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
        let (dropped, kept): (Vec<Credential>, Vec<Credential>) = Self::read_index(vault)?
            .into_iter()
            .partition(|credential| gone(credential));
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

/// `request` asked of `keeper`: its answer, and a read's secrets; its
/// store's failure, or the keeper's, as an error.
fn asked(
    keeper: &Keeper,
    request: SignInRequest,
) -> Result<(SignInAnswer, Option<SignIn>), String> {
    match keeper.sign_ins(request) {
        Ok((SignInAnswer::Failed { reason }, _)) => Err(format!("the keeper's sign-ins: {reason}")),
        Ok(answered) => Ok(answered),
        Err(error) => Err(error.to_string()),
    }
}

/// How many sign-ins a deletion's answer says went.
fn deleted(answered: (SignInAnswer, Option<SignIn>)) -> Result<usize, String> {
    match answered {
        (SignInAnswer::Deleted { count }, _) => Ok(count),
        (other, _) => Err(out_of_turn(&other)),
    }
}

fn out_of_turn(answer: &SignInAnswer) -> String {
    format!("the keeper answered out of turn: {answer:?}")
}

/// The keeper's answer to `request` from the sign-ins in `credentials`,
/// its own store: and a read's secrets, which it sends after.
pub(crate) fn answer(
    credentials: &Credentials<'_>,
    request: SignInRequest,
) -> (SignInAnswer, Option<SignIn>) {
    let failed = |reason: String| (SignInAnswer::Failed { reason }, None);
    match request {
        SignInRequest::List => credentials
            .list()
            .map_or_else(failed, |sign_ins| (SignInAnswer::Listed { sign_ins }, None)),
        SignInRequest::Keep {
            profile_id,
            origin,
            label,
            sign_in,
            now,
        } => match credentials.keep(&profile_id, &origin, &label, &sign_in, now) {
            Ok(Ok(sign_in)) => (SignInAnswer::Kept { sign_in }, None),
            Ok(Err(reason)) => (SignInAnswer::Refused { reason }, None),
            Err(reason) => failed(reason),
        },
        SignInRequest::Read { id } => credentials
            .sign_in(&id)
            .map_or_else(failed, |sign_in| (SignInAnswer::Read, Some(sign_in))),
        SignInRequest::Delete { id } => credentials.delete(&id).map_or_else(failed, |found| {
            (
                SignInAnswer::Deleted {
                    count: usize::from(found),
                },
                None,
            )
        }),
        SignInRequest::DeleteProfiles { profile_id } => credentials
            .delete_profiles(&profile_id)
            .map_or_else(failed, |count| (SignInAnswer::Deleted { count }, None)),
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
) -> Result<String, SignInRefusal> {
    let canonical = shape(origin, label, sign_in)?;
    let url = WebUrl::parse(&canonical).map_err(|_| SignInRefusal::BadOrigin)?;
    if !profile.holds(&url.origin().destination()) {
        return Err(SignInRefusal::NotTheProfiles);
    }
    Ok(canonical)
}

/// What `origin` is as a sign-in labelled `label`, whatever its profile:
/// its canonical origin, or why it isn't kept.
fn shape(origin: &str, label: &str, sign_in: &SignIn) -> Result<String, SignInRefusal> {
    let label = label.trim();
    if label.is_empty() || label.chars().any(char::is_control) {
        return Err(SignInRefusal::BadLabel);
    }
    if sign_in.password.expose().is_empty() {
        return Err(SignInRefusal::NoPassword);
    }
    if label.chars().count() > MAX_LABEL_CHARS
        || sign_in.username.expose().len() > MAX_USERNAME_BYTES
        || sign_in.password.expose().len() > MAX_PASSWORD_BYTES
    {
        return Err(SignInRefusal::TooLong);
    }
    origin_of(origin).ok_or(SignInRefusal::BadOrigin)
}

/// `credential` as the owner's app is told it.
#[must_use]
pub fn shown(credential: &Credential) -> SiteSignIn {
    SiteSignIn {
        sign_in_id: credential.id.clone(),
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
    op: &SignInOp,
) -> SignInResult {
    let Some(credentials) = Credentials::of(config, keys) else {
        return SignInResult::Off;
    };
    let answer = match op {
        SignInOp::List => credentials.list().map(|listed| SignInResult::Listed {
            sign_ins: listed.iter().map(shown).collect(),
        }),
        SignInOp::Store {
            profile_id,
            origin,
            label,
            username,
            password,
        } => {
            let Some(profiles) = Profiles::of(config, keys) else {
                return SignInResult::Off;
            };
            let sign_in = SignIn {
                username: username.clone(),
                password: password.clone(),
            };
            profiles.list().and_then(|listed| {
                let Some(profile) = listed.iter().find(|profile| &profile.id == profile_id) else {
                    return Ok(SignInResult::Refused {
                        reason: SignInRefusal::NoProfile,
                    });
                };
                let now = hatchling_scheduler::Timestamp::now().as_second();
                credentials
                    .store(profile, origin, label, &sign_in, now)
                    .map(|kept| match kept {
                        Ok(credential) => SignInResult::Stored {
                            sign_in: shown(&credential),
                        },
                        Err(reason) => SignInResult::Refused { reason },
                    })
            })
        }
        SignInOp::Delete { sign_in_id } => credentials.delete(sign_in_id).map(|found| {
            if found {
                SignInResult::Deleted
            } else {
                SignInResult::NotFound
            }
        }),
    };
    answer.unwrap_or_else(|error| {
        tracing::error!(%error, "a sign-in operation failed");
        SignInResult::Failed
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
            .keep(
                &[
                    String::from("a.example:443"),
                    String::from("idp.example:443"),
                ],
                1,
            )
            .unwrap();
        (config, scratch, key, profile)
    }

    fn sign_in(username: &str, password: &str) -> SignIn {
        SignIn {
            username: Concealed::new(username.to_owned()),
            password: Concealed::new(password.to_owned()),
        }
    }

    fn store_op(profile_id: &str, origin: &str, label: &str, password: &str) -> SignInOp {
        SignInOp::Store {
            profile_id: profile_id.to_owned(),
            origin: origin.to_owned(),
            label: label.to_owned(),
            username: Concealed::new(String::from("ada@a.example")),
            password: Concealed::new(password.to_owned()),
        }
    }

    /// A deletion's answer says how many went; any other answer is out of
    /// turn, and says which.
    #[test]
    fn a_deletions_answer_is_its_count_and_any_other_is_out_of_turn() {
        assert_eq!(deleted((SignInAnswer::Deleted { count: 2 }, None)), Ok(2));
        let other = deleted((SignInAnswer::Read, None));
        assert!(
            other
                .as_ref()
                .is_err_and(|error| error.contains("out of turn") && error.contains("Read")),
            "{other:?}"
        );
        assert_eq!(
            out_of_turn(&SignInAnswer::Deleted { count: 1 }),
            "the keeper answered out of turn: Deleted { count: 1 }"
        );
    }

    #[test]
    fn an_origin_is_an_https_origin_alone() {
        assert_eq!(
            origin_of("https://a.example"),
            Some(String::from("https://a.example"))
        );
        assert_eq!(
            origin_of(" https://A.example/ "),
            Some(String::from("https://a.example"))
        );
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
            (
                "https://b.example",
                "Work",
                &good,
                SignInRefusal::NotTheProfiles,
            ),
            (
                "https://a.example:8443",
                "Work",
                &good,
                SignInRefusal::NotTheProfiles,
            ),
            (
                "https://a.example/login",
                "Work",
                &good,
                SignInRefusal::BadOrigin,
            ),
            ("http://a.example", "Work", &good, SignInRefusal::BadOrigin),
            ("https://a.example", " ", &good, SignInRefusal::BadLabel),
            ("https://a.example", "a\nb", &good, SignInRefusal::BadLabel),
            (
                "https://a.example",
                "Work",
                &sign_in("ada", ""),
                SignInRefusal::NoPassword,
            ),
            (
                "https://a.example",
                &"l".repeat(MAX_LABEL_CHARS + 1),
                &good,
                SignInRefusal::TooLong,
            ),
            (
                "https://a.example",
                "Work",
                &sign_in(&"u".repeat(MAX_USERNAME_BYTES + 1), "pw"),
                SignInRefusal::TooLong,
            ),
            (
                "https://a.example",
                "Work",
                &sign_in("ada", &"p".repeat(MAX_PASSWORD_BYTES + 1)),
                SignInRefusal::TooLong,
            ),
        ] {
            assert_eq!(
                store(origin, label, sign),
                Err(reason),
                "{origin} {label:?}"
            );
        }
        assert_eq!(credentials.list(), Ok(Vec::new()), "nothing kept");
        // At the bounds, kept: the sign-in site is the profile's too.
        let edge = sign_in(
            &"u".repeat(MAX_USERNAME_BYTES),
            &"p".repeat(MAX_PASSWORD_BYTES),
        );
        let label = "l".repeat(MAX_LABEL_CHARS);
        assert!(store("https://idp.example/", &label, &edge).is_ok());
        assert_eq!(credentials.list().map(|listed| listed.len()), Ok(1));
    }

    #[test]
    fn a_kept_sign_in_is_listed_by_label_read_whole_and_deleted() {
        let (config, _scratch, key, profile) = kept("kept");
        let credentials = Credentials::of(&config, &key).unwrap();
        let kept = credentials
            .store(
                &profile,
                "https://a.example",
                " Work ",
                &sign_in("zoe-user", "hunter2-secret"),
                7,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                kept.profile_id.as_str(),
                kept.origin.as_str(),
                kept.label.as_str(),
                kept.created_at
            ),
            (profile.id.as_str(), "https://a.example", "Work", 7)
        );
        assert!(kept.id.starts_with("cred-"), "{}", kept.id);
        assert_eq!(credentials.list(), Ok(vec![kept.clone()]));
        assert_eq!(
            credentials.sign_in(&kept.id),
            Ok(sign_in("zoe-user", "hunter2-secret"))
        );
        assert!(credentials.sign_in("cred-none").is_err());
        // Listed, the secrets are nowhere: not in the index, nor what the app is told.
        let listed = format!("{:?} {:?}", credentials.list(), shown(&kept));
        assert!(
            !listed.contains("hunter2") && !listed.contains("zoe-user"),
            "{listed}"
        );
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
        assert!(
            credentials.sign_in(&kept.id).is_err(),
            "its secrets gone too"
        );
        assert!(
            matches!(
                credentials
                    .vault()
                    .unwrap()
                    .fetch(&Credentials::entry(&kept.id).unwrap()),
                Err(hatchling_vault::Error::NotFound { .. })
            ),
            "its sealed entry removed, not left on disk"
        );
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
                    .store(
                        &profile,
                        "https://a.example",
                        &label,
                        &sign_in("ada", "pw"),
                        1
                    )
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
            credentials.store(
                &profile,
                "https://a.example",
                "One more",
                &sign_in("ada", "pw"),
                1
            ),
            Ok(Err(SignInRefusal::Full))
        );
        assert_eq!(
            credentials.delete_profiles(&profile.id),
            Ok(MAX_CREDENTIALS - 1)
        );
        assert_eq!(credentials.delete_profiles(&profile.id), Ok(0));
        let entries = std::fs::read_dir(config.web.credentials.as_path())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("cred-"))
            })
            .count();
        assert_eq!(
            entries, 1,
            "the other profile's sign-in alone is left on disk"
        );
        let left = credentials.list().unwrap();
        assert_eq!(
            left.iter()
                .map(|credential| credential.profile_id.as_str())
                .collect::<Vec<_>>(),
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
        let SignInResult::Stored {
            sign_in: credential,
        } = stored
        else {
            unreachable!("{stored:?}");
        };
        assert_eq!(credential.origin, "https://a.example");
        assert_eq!(
            handle_credential_op(&config, &key, &SignInOp::List),
            SignInResult::Listed {
                sign_ins: vec![credential.clone()]
            }
        );
        for (op, outcome) in [
            (
                store_op("profile-none", "https://a.example", "Work", "pw"),
                SignInResult::Refused {
                    reason: SignInRefusal::NoProfile,
                },
            ),
            (
                store_op(&profile.id, "https://b.example", "Work", "pw"),
                SignInResult::Refused {
                    reason: SignInRefusal::NotTheProfiles,
                },
            ),
            (
                SignInOp::Delete {
                    sign_in_id: credential.sign_in_id.clone(),
                },
                SignInResult::Deleted,
            ),
            (
                SignInOp::Delete {
                    sign_in_id: credential.sign_in_id,
                },
                SignInResult::NotFound,
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
        assert!(matches!(again, SignInResult::Stored { .. }), "{again:?}");
        let delete = ProfileOp::Delete {
            profile_id: profile.id,
        };
        assert_eq!(
            crate::browser_profiles::handle_profile_op(&config, &key, &delete),
            hatchling_core::ui::ProfileResult::Deleted
        );
        assert_eq!(
            handle_credential_op(&config, &key, &SignInOp::List),
            SignInResult::Listed {
                sign_ins: Vec::new()
            }
        );
        // Another key: failed. No profiles, or no store: off.
        let other = MasterKey::generate().unwrap();
        assert_eq!(
            handle_credential_op(&config, &other, &SignInOp::List),
            SignInResult::Failed
        );
        let shown = format!("{:?}", Credentials::of(&config, &key).unwrap());
        assert!(shown.contains("Credentials"), "{shown}");
        let mut config = config;
        config.web.profiles = std::path::PathBuf::new();
        assert_eq!(
            handle_credential_op(
                &config,
                &key,
                &store_op("profile-1", "https://a.example", "Work", "pw")
            ),
            SignInResult::Off
        );
        config.web.credentials = std::path::PathBuf::new();
        assert_eq!(
            handle_credential_op(&config, &key, &SignInOp::List),
            SignInResult::Off
        );
    }

    #[test]
    fn the_index_and_its_entries_are_read_written_and_dropped_as_named() {
        let (config, _scratch, key, profile) = kept("index");
        let credentials = Credentials::of(&config, &key).unwrap();
        assert!(Credentials::entry("cred-1").is_ok());
        assert!(Credentials::entry("not a name/").is_err());
        let vault = credentials.vault().unwrap();
        assert_eq!(Credentials::read_index(&vault), Ok(Vec::new()), "none yet");
        let listed = |id: &str| Credential {
            id: id.to_owned(),
            profile_id: profile.id.clone(),
            origin: String::from("https://a.example"),
            label: String::from("Work"),
            created_at: 1,
        };
        let both = vec![listed("cred-a"), listed("cred-b")];
        assert_eq!(Credentials::write_index(&vault, &both), Ok(()));
        assert_eq!(Credentials::read_index(&vault), Ok(both));
        assert_eq!(
            Credentials::drop_where(&vault, |credential| credential.id == "cred-a"),
            Ok(1),
            "an entry already gone is no failure"
        );
        assert_eq!(Credentials::read_index(&vault), Ok(vec![listed("cred-b")]));
        assert_eq!(Credentials::drop_where(&vault, |_| false), Ok(0));
        let corrupt = Secret::new(b"not json".to_vec()).unwrap();
        vault
            .store(&Credentials::entry(INDEX).unwrap(), &corrupt)
            .unwrap();
        assert!(Credentials::read_index(&vault).is_err());
    }
}
