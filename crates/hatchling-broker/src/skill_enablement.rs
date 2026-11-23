//! The skills the owner enabled (WO-046): which of the skills the broker
//! found may run, sealed in a store of their own
//! (`HATCHLING_SKILL_ENABLEMENTS_DIR`), changed by the owner alone.
//!
//! - **Enabling binds to a digest.** The owner enables a skill as listed,
//!   naming the digest they saw: the SHA-256 of its signed manifest, which
//!   lists every file's hash and the capabilities its program declares. A
//!   skill whose digest is another by the time the owner enables it is
//!   refused as changed, and one that changes after it is enabled stops
//!   running and waits for the owner again: a signer can't widen what a
//!   skill does, or swap its playbook, under the owner's old yes.
//! - **Enabling is the grant.** An enabled skill runs with exactly the
//!   capabilities its program declares, which the owner is shown before
//!   they enable it; there are no grants by setting any more.
//! - **Only the owner changes it,** through the app's API; no session op
//!   reaches this store, so a cell can't enable, list or disable a skill.
//! - **Bounded:** at most [`MAX_ENABLED`]; past that a new one is refused,
//!   and nothing enabled is dropped to make room.
//! - **Disabling one** stops it at once: the broker's set is rebuilt
//!   without it before the owner is answered.
//! - **A store that can't be read** enables nothing: the broker starts
//!   with every skill waiting, and says why in its log.

use std::fmt;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::SecretName;
use hatchling_core::ui::{SkillListing, SkillsOp, SkillsResult};
use hatchling_vault::{OpensStores, SealedVault, Secret, Store};
use serde::{Deserialize, Serialize};

use crate::Config;
use crate::skills::{Enabled, Skills};

/// The most skills the owner may keep enabled.
pub const MAX_ENABLED: usize = 64;

/// The longest skill name a record may hold, in bytes.
const MAX_NAME_BYTES: usize = 128;

/// The store's one entry: every skill enabled, in the order enabled.
const ENTRY: &str = "enabled";

/// Serializes the store's writers within a broker, and each write with the
/// rebuild it causes.
static STORE: Mutex<()> = Mutex::new(());

/// One skill the owner enabled, as kept: what they enabled it as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    /// Its name.
    #[serde(rename = "n")]
    name: String,
    /// The digest the owner enabled.
    #[serde(rename = "d")]
    digest: String,
    /// Its version then.
    #[serde(rename = "v")]
    version: String,
    /// Its signer's fingerprint then.
    #[serde(rename = "s")]
    signer: String,
    /// The capabilities it declared then: what the owner granted.
    #[serde(rename = "c")]
    capabilities: Vec<String>,
    /// When the owner enabled it, in Unix seconds.
    #[serde(rename = "t")]
    enabled_at: i64,
}

impl Record {
    /// Whether it holds what a record can: a bounded name and a digest in
    /// its one form, lowercase hex SHA-256.
    fn sound(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= MAX_NAME_BYTES
            && is_digest(&self.digest)
            && self.enabled_at >= 0
    }
}

/// Whether `digest` is lowercase hex SHA-256.
fn is_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// What the store's entry holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    #[serde(rename = "e")]
    enabled: Vec<Record>,
}

impl Kept {
    /// What it enables: each name, with its digest.
    fn map(&self) -> Enabled {
        self.enabled
            .iter()
            .map(|record| (record.name.clone(), record.digest.clone()))
            .collect()
    }
}

/// The owner's enablements, opened with the broker's keys.
pub struct Enablements<'k> {
    root: &'k Path,
    keys: &'k dyn OpensStores,
}

impl fmt::Debug for Enablements<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Enablements")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl<'k> Enablements<'k> {
    /// The store, when skills are on: `None` when the operator gave the
    /// broker none.
    #[must_use]
    pub fn of(config: &'k Config, keys: &'k dyn OpensStores) -> Option<Self> {
        let root = config.skills.enablements.as_path();
        (config.skills.is_enabled() && !root.as_os_str().is_empty()).then_some(Self { root, keys })
    }

    fn vault(&self) -> Result<SealedVault<'_>, String> {
        SealedVault::open_in(self.root, self.keys, Store::SKILLS)
            .map_err(|error| format!("the skills enabled can't be opened: {error}"))
    }

    fn name() -> Result<SecretName, String> {
        ENTRY
            .parse()
            .map_err(|_| format!("the enablements' entry name {ENTRY:?} is invalid"))
    }

    fn read(vault: &SealedVault<'_>) -> Result<Kept, String> {
        let kept: Kept = match vault.fetch(&Self::name()?) {
            Ok(secret) => serde_json::from_slice(secret.as_bytes())
                .map_err(|error| format!("the skills enabled are corrupt: {error}"))?,
            Err(hatchling_vault::Error::NotFound { .. }) => Kept::default(),
            Err(error) => return Err(format!("the skills enabled can't be read: {error}")),
        };
        let mut names: Vec<&str> = kept.enabled.iter().map(|r| r.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let sound = kept.enabled.len() <= MAX_ENABLED
            && names.len() == kept.enabled.len()
            && kept.enabled.iter().all(Record::sound);
        if sound {
            Ok(kept)
        } else {
            Err(String::from(
                "the skills enabled hold what enablements can't",
            ))
        }
    }

    fn write(vault: &SealedVault<'_>, kept: &Kept) -> Result<(), String> {
        let bytes = serde_json::to_vec(kept)
            .map_err(|error| format!("the skills enabled don't encode: {error}"))?;
        let secret =
            Secret::new(bytes).map_err(|error| format!("the skills enabled don't fit: {error}"))?;
        vault
            .store(&Self::name()?, &secret)
            .map_err(|error| format!("the skills enabled can't be written: {error}"))
    }

    /// Runs `then` on the store as kept, one writer at a time.
    fn opened<T>(
        &self,
        then: impl FnOnce(&SealedVault<'_>, Kept) -> Result<T, String>,
    ) -> Result<T, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let kept = Self::read(&vault)?;
        then(&vault, kept)
    }

    /// What the owner has enabled: each name, with its digest.
    ///
    /// # Errors
    ///
    /// Why the store can't be read.
    pub fn enabled(&self) -> Result<Enabled, String> {
        self.opened(|_, kept| Ok(kept.map()))
    }

    /// Enables `skill` as listed, at `now`, and hands what is enabled to
    /// `skills`: it runs from now. Enabling one already enabled at another
    /// digest replaces it.
    ///
    /// # Errors
    ///
    /// Why it can't be kept: the store, or [`MAX_ENABLED`] reached.
    pub fn enable(&self, skill: &SkillListing, skills: &Skills, now: i64) -> Result<(), String> {
        self.opened(|vault, mut kept| {
            kept.enabled.retain(|record| record.name != skill.name);
            if kept.enabled.len() >= MAX_ENABLED {
                return Err(format!(
                    "{MAX_ENABLED} skills are enabled already: disable one first"
                ));
            }
            let record = Record {
                name: skill.name.clone(),
                digest: skill.digest.clone(),
                version: skill.version.clone(),
                signer: skill.signer.clone(),
                capabilities: skill.capabilities.clone(),
                enabled_at: now.max(0),
            };
            if !record.sound() {
                return Err(String::from("the skill isn't one a record can hold"));
            }
            kept.enabled.push(record);
            Self::write(vault, &kept)?;
            skills.set_enabled(kept.map());
            Ok(())
        })
    }

    /// Disables the skill named `name`, and hands what is still enabled to
    /// `skills`: it stops now. Whether it was enabled.
    ///
    /// # Errors
    ///
    /// Why the store can't be changed.
    pub fn disable(&self, name: &str, skills: &Skills) -> Result<bool, String> {
        self.opened(|vault, mut kept| {
            let before = kept.enabled.len();
            kept.enabled.retain(|record| record.name != name);
            if kept.enabled.len() == before {
                return Ok(false);
            }
            Self::write(vault, &kept)?;
            skills.set_enabled(kept.map());
            Ok(true)
        })
    }
}

/// Hands `skills` what the owner enabled, at the broker's start, once the
/// keys are in hand. A store that can't be read enables nothing: every
/// skill waits, and the log says why.
pub fn load_enabled(config: &Config, keys: &dyn OpensStores, skills: &Skills) {
    let Some(store) = Enablements::of(config, keys) else {
        return;
    };
    match store.enabled() {
        Ok(enabled) => {
            tracing::info!(enabled = enabled.len(), "the owner's skills handed over");
            skills.set_enabled(enabled);
        }
        Err(error) => {
            tracing::error!(%error, "the skills enabled can't be read: every skill waits");
        }
    }
}

/// The owner's `op` on the skills, under `config`, with `keys`, at `now`.
#[must_use]
pub fn handle_skills_op(
    config: &Config,
    keys: &dyn OpensStores,
    skills: &Skills,
    op: &SkillsOp,
    now: i64,
) -> SkillsResult {
    let Some(store) = Enablements::of(config, keys) else {
        return SkillsResult::Off;
    };
    // The skills as they are now: what the owner lists, and enables.
    if !matches!(op, SkillsOp::Disable { .. }) {
        skills.refresh();
    }
    let set = skills.snapshot();
    let named = |name: &str| set.listing().iter().any(|listing| listing.name == name);
    let answer = match op {
        SkillsOp::List => Ok(SkillsResult::Listed {
            skills: set.listing().to_vec(),
        }),
        SkillsOp::Enable { name, digest } => {
            let seen = set
                .listing()
                .iter()
                .find(|listing| listing.name == *name && listing.digest == *digest);
            match seen {
                Some(listing) => store
                    .enable(listing, skills, now)
                    .map(|()| SkillsResult::Enabled),
                None if named(name) => Ok(SkillsResult::Changed),
                None => Ok(SkillsResult::NotFound),
            }
        }
        SkillsOp::Disable { name } => store.disable(name, skills).map(|was| {
            if was || named(name) {
                SkillsResult::Disabled
            } else {
                SkillsResult::NotFound
            }
        }),
    };
    answer.unwrap_or_else(|error| {
        tracing::error!(%error, "a skills operation failed");
        SkillsResult::Failed
    })
}

#[cfg(test)]
#[path = "skill_enablement/tests.rs"]
mod tests;
