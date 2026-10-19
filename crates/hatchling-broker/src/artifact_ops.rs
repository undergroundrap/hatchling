//! The broker's artifact ops: builds through the skill
//! gate, deliveries through the share-review gate, and ideas built
//! through the same pipeline.
//!
//! **A build** pins its request, refuses a kind with no registered
//! profile, checks the builder against the skill gate (the
//! builder must be a registered skill declaring
//! [`ARTIFACT_BUILD_CAPABILITY`]), records it as pending, runs the
//! builder in its own skill cell on exactly the pinned request, and
//! records the outcome. A completed build's provenance is the broker's
//! own: the pinned inputs, the builder's signing identity from its
//! admission, the hash of the bytes the broker received, and the
//! inspector's report on those bytes against the kind's profile.
//!
//! **A delivery** goes through [`hatchling_artifact::deliver`], the one
//! delivery function, whatever the target: the local channel or the
//! operator's workspace. The gate there judges the final bytes against
//! every secret the broker holds ([`held_secrets`]) and the configured
//! review policy, and fails closed. The inspector's report plays no part
//! in it: it is surfaced with the delivery, never consulted by it. Each
//! attempt, delivered or refused, is kept in the build's record with its
//! verdict.
//!
//! **A description** is a build as a UI shows it. Its attribution says
//! "verified builder" only when the builder the provenance names is
//! registered now, under the same signing key ([`attribution`]).
//!
//! **An idea** is captured (with where it came from: a feed item is
//! re-read here, and its provenance label kept), listed, and developed
//! (refined, noted). It becomes a build only through a confirmed
//! activation: a proposal pins the exact request, the user's
//! confirmation is minted by the broker alone
//! ([`crate::activation`]), and activating builds only the pinned
//! request, as an ordinary build whose provenance names the revision.
//! The 025b direct build from an idea is refused: there is no path from
//! an idea to an artifact that skips the confirmation.
//!
//! **The store is sealed and broker-private,** like the schedule store:
//! a [`SealedVault`] at `HATCHLING_ARTIFACT_DIR`, one entry per build
//! (`build-<id>`) and per idea (`idea-<id>`), each with an index, at most
//! [`MAX_BUILDS`] and [`MAX_IDEAS`]. A run's own session has artifacts
//! off.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hatchling_artifact::{
    ARTIFACT_BUILD_CAPABILITY, BuildRecord, BuildStatus, BuilderClaim, BuilderIdentity, IdeaOrigin,
    IdeaRecord, PinnedRequest, SecretSet, ShareReview, Sink, attribution, inspect,
};
use hatchling_channel::{ChannelId, LocalHub, Message, SenderId};
use hatchling_core::{
    ArtifactInput, ArtifactOp, ArtifactResult, ArtifactView, BuildFailure, BuildState,
    CapabilityName, DeliveryRefusal, DeliveryTarget, IdeaRef, IdeaRefusal, IdeaSource,
    MAX_SECRET_BYTES, SecretName,
};
use hatchling_goals::{FeedQuery, GoalId, StatusFilter, TimeRange};
use hatchling_memory::{MemoryPrincipal, Tag};
use hatchling_skill::SkillId;
use hatchling_vault::{OpensStores, SealedVault, Secret};
use zeroize::Zeroizing;

use crate::skills::SkillSet;
use crate::{Config, Error, SkillDenial, Skills, check_skill_call};

/// Most builds the store keeps.
pub const MAX_BUILDS: usize = 64;

/// Most ideas the store keeps.
pub const MAX_IDEAS: usize = 64;

/// Every build id starts with this.
pub const BUILD_ID_PREFIX: &str = "art-";

/// Every idea id starts with this.
pub const IDEA_ID_PREFIX: &str = "idea-";

/// Every activation id starts with this.
pub const ACTIVATION_ID_PREFIX: &str = "act-";

/// Most ideas one list page carries.
pub const IDEA_PAGE: usize = 16;

/// The sender stamp on delivered artifacts: the broker's pipeline, never
/// a peer and never the builder.
pub const ARTIFACT_SENDER: &str = "broker:artifact";

/// Longest workspace file name, in bytes.
pub const MAX_WORKSPACE_NAME_BYTES: usize = 128;

/// One kind of store entry: its index, its entry-name prefix, and how
/// many it keeps.
struct Shelf {
    index: &'static str,
    entry: &'static str,
    max: usize,
}

const BUILDS: Shelf = Shelf {
    index: "build-index",
    entry: "build",
    max: MAX_BUILDS,
};

const IDEAS: Shelf = Shelf {
    index: "idea-index",
    entry: "idea",
    max: MAX_IDEAS,
};

/// Serializes every store change in the process.
static STORE: Mutex<()> = Mutex::new(());

/// The sealed artifact store at `root`: builds and ideas.
#[derive(Debug)]
pub struct SealedArtifactStore<'k> {
    root: &'k Path,
    key: &'k dyn OpensStores,
}

impl<'k> SealedArtifactStore<'k> {
    /// The store at `root`, sealed under `key`. Nothing is opened until an
    /// operation needs it.
    #[must_use]
    pub const fn new(root: &'k Path, key: &'k dyn OpensStores) -> Self {
        Self { root, key }
    }

    fn vault(&self) -> Result<SealedVault<'k>, Error> {
        SealedVault::open_in(self.root, self.key, hatchling_vault::Store::ARTIFACTS)
            .map_err(|source| Error::Vault { source })
    }

    fn insert_on<T: serde::Serialize>(
        &self,
        on: &Shelf,
        id: &str,
        value: &T,
    ) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut index: Vec<String> = read_json(&vault, on.index)?.unwrap_or_default();
        if index.len() >= on.max {
            return Ok(false);
        }
        write_json(&vault, &entry_name(on, id), value)?;
        index.push(id.to_owned());
        write_json(&vault, on.index, &index)?;
        Ok(true)
    }

    fn put_on<T: serde::Serialize>(&self, on: &Shelf, id: &str, value: &T) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, on.index)?.unwrap_or_default();
        if !index.iter().any(|stored| stored == id) {
            return Ok(false);
        }
        write_json(&vault, &entry_name(on, id), value)?;
        Ok(true)
    }

    fn get_on<T: serde::de::DeserializeOwned>(
        &self,
        on: &Shelf,
        id: &str,
    ) -> Result<Option<T>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, on.index)?.unwrap_or_default();
        if !index.iter().any(|stored| stored == id) {
            return Ok(None);
        }
        read_json(&vault, &entry_name(on, id))
    }

    /// Stores a new build, unless the store is full.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure; `Ok(false)` means full.
    pub fn insert(&self, record: &BuildRecord) -> Result<bool, Error> {
        self.insert_on(&BUILDS, &record.build, record)
    }

    /// Replaces a stored build, if it exists. Returns whether it did.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn put(&self, record: &BuildRecord) -> Result<bool, Error> {
        self.put_on(&BUILDS, &record.build, record)
    }

    /// The build `id`, if it exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn get(&self, id: &str) -> Result<Option<BuildRecord>, Error> {
        self.get_on(&BUILDS, id)
    }

    /// Stores a new idea, unless the store is full.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure; `Ok(false)` means full.
    pub fn insert_idea(&self, idea: &IdeaRecord) -> Result<bool, Error> {
        self.insert_on(&IDEAS, &idea.idea, idea)
    }

    /// Replaces a stored idea, if it exists. Returns whether it did.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn put_idea(&self, idea: &IdeaRecord) -> Result<bool, Error> {
        self.put_on(&IDEAS, &idea.idea, idea)
    }

    /// Every idea, in the order they were captured.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the store cannot be read.
    pub fn ideas(&self) -> Result<Vec<IdeaRecord>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, IDEAS.index)?.unwrap_or_default();
        let mut ideas = Vec::with_capacity(index.len());
        for id in &index {
            if let Some(idea) = read_json(&vault, &entry_name(&IDEAS, id))? {
                ideas.push(idea);
            }
        }
        Ok(ideas)
    }

    /// The idea `id`, if it exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn get_idea(&self, id: &str) -> Result<Option<IdeaRecord>, Error> {
        self.get_on(&IDEAS, id)
    }
}

fn entry_name(shelf: &Shelf, id: &str) -> String {
    format!("{}-{id}", shelf.entry)
}

fn secret_name(name: &str) -> Result<SecretName, Error> {
    name.parse().map_err(|_| Error::ArtifactStore {
        reason: format!("not a store entry name: {name:?}"),
    })
}

fn read_json<T: serde::de::DeserializeOwned>(
    vault: &SealedVault<'_>,
    name: &str,
) -> Result<Option<T>, Error> {
    match vault.fetch(&secret_name(name)?) {
        Ok(secret) => serde_json::from_slice(secret.as_bytes())
            .map(Some)
            .map_err(|e| Error::ArtifactStore {
                reason: format!("entry {name} is corrupt: {e}"),
            }),
        Err(hatchling_vault::Error::NotFound { .. }) => Ok(None),
        Err(source) => Err(Error::Vault { source }),
    }
}

fn write_json<T: serde::Serialize>(
    vault: &SealedVault<'_>,
    name: &str,
    value: &T,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|e| Error::ArtifactStore {
        reason: format!("entry {name} does not encode: {e}"),
    })?;
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(Error::ArtifactStore {
            reason: format!("entry {name} is {} bytes, over the bound", bytes.len()),
        });
    }
    let secret = Secret::new(bytes).map_err(|source| Error::Vault { source })?;
    vault
        .store(&secret_name(name)?, &secret)
        .map_err(|source| Error::Vault { source })
}

/// Every secret the broker holds for the session, as the gate looks for
/// them: the vault secrets its grants name, the channel accounts'
/// secrets, and each granted egress destination's credential.
///
/// The vault has no list op, by design, so this is the union of every
/// secret the configuration names. A name with nothing stored under it
/// holds nothing. Any other failure is an error, and the gate blocks on
/// it.
///
/// # Errors
///
/// Returns why the secrets could not all be read.
pub fn held_secrets(config: &Config, key: &dyn OpensStores) -> Result<SecretSet, String> {
    held_secret_values(config, key, "artifact-gate").map(SecretSet::new)
}

/// The raw values [`held_secrets`] looks for: what the output gate's
/// rules also normalize and look for. The read is recorded in the
/// vault's access log as the broker's, for `purpose`.
///
/// Each value is wiped when dropped.
///
/// # Errors
///
/// As [`held_secrets`].
pub fn held_secret_values(
    config: &Config,
    key: &dyn OpensStores,
    purpose: &str,
) -> Result<Vec<Zeroizing<Vec<u8>>>, String> {
    let names = held_secret_names(config)
        .into_iter()
        .map(|name| {
            name.parse::<SecretName>()
                .map_err(|_| format!("{name:?} is not a secret name"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let read = crate::secrets::Secrets::new(&config.vault.root, key)
        .broker_read(purpose, &names)
        .map_err(|e| format!("the held secrets could not be read: {e}"))?;
    Ok(read
        .into_iter()
        .flatten()
        .map(|secret| Zeroizing::new(secret.as_bytes().to_vec()))
        .collect())
}

/// Every secret name the configuration names.
fn held_secret_names(config: &Config) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = config
        .vault
        .grants
        .iter()
        .map(|grant| grant.name.as_str().to_owned())
        .collect();
    names.extend(
        config
            .channel_accounts
            .iter()
            .map(|account| account.secret.as_str().to_owned()),
    );
    names.extend(
        config
            .egress
            .grants
            .iter()
            .map(|destination| crate::serve::destination_to_secret_name(destination)),
    );
    names
}

/// Handles one [`ArtifactOp`] at `now` (Unix seconds).
///
/// `Denied` when artifacts are off, which they always are in a run's own
/// session. Builds run through `skills`; channel deliveries go to `hub`,
/// the session's local channel.
pub fn handle_artifact_op(
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
    principal: &MemoryPrincipal,
    op: &ArtifactOp,
    now: i64,
) -> ArtifactResult {
    if !config.artifacts.enabled() {
        return ArtifactResult::Denied;
    }
    let pipeline = Pipeline {
        config,
        key,
        skills,
        hub,
        scope: crate::output_gate::session_scope(&principal.session_id),
        store: SealedArtifactStore::new(&config.artifacts.root, key),
        now,
    };
    match op {
        ArtifactOp::Build {
            build_kind,
            builder,
            inputs,
        } => pipeline.build(build_kind, builder, inputs, None),
        ArtifactOp::Deliver { build, to } => pipeline.deliver(build, to),
        ArtifactOp::Describe { build } => pipeline.describe(build),
        ArtifactOp::CaptureIdea { text, from } => origin(config, principal, from.as_ref())
            .map_or_else(
                || ArtifactResult::IdeaRefused {
                    reason: IdeaRefusal::UnreadableSource,
                },
                |origin| pipeline.capture_idea(text, origin),
            ),
        ArtifactOp::RefineIdea { idea, text } => pipeline.refine_idea(idea, text),
        ArtifactOp::ListIdeas { after } => pipeline.list_ideas(after.as_deref()),
        ArtifactOp::NoteIdea { idea, note } => pipeline.note_idea(idea, note),
        // an idea becomes a build only through a confirmed
        // activation.
        ArtifactOp::BuildIdea { .. } => ArtifactResult::IdeaRefused {
            reason: IdeaRefusal::ConfirmationRequired,
        },
        ArtifactOp::ProposeActivation {
            idea,
            build_kind,
            builder,
            description,
        } => pipeline.propose_activation(idea, build_kind, builder, description),
        ArtifactOp::Activate {
            activation,
            confirmation,
        } => pipeline.activate(activation, confirmation),
    }
}

/// Where an idea is captured from, as the broker records it: a feed item
/// is re-read, with the session's own read grants, and its provenance
/// label kept. `None` if it cannot be read.
fn origin(
    config: &Config,
    principal: &MemoryPrincipal,
    from: Option<&IdeaSource>,
) -> Option<IdeaOrigin> {
    let (store, id, tag) = match from {
        None => return Some(IdeaOrigin::Unstated),
        Some(IdeaSource::Chat) => return Some(IdeaOrigin::Chat),
        Some(IdeaSource::Command) => return Some(IdeaOrigin::Command),
        Some(IdeaSource::Feed { store, id, tag }) => (store, id, tag),
    };
    let query = match store.as_str() {
        "goal" => {
            id.parse::<GoalId>().ok()?;
            FeedQuery::Goals {
                status: StatusFilter::Any,
                updated_since_secs: None,
                after: None,
            }
        }
        "tracking" => FeedQuery::Tracking {
            range: TimeRange::ALL,
            source: None,
            after_seq: id.parse::<u64>().ok()?.checked_sub(1),
        },
        "memory" => FeedQuery::Memory {
            tag: tag.as_deref()?.parse::<Tag>().ok()?,
            after: None,
        },
        _ => return None,
    };
    if !crate::goals_ops::may_read(&config.goals, &config.memory, &query) {
        return None;
    }
    let item = crate::goals_ops::read_feed(&config.goals, &config.memory, principal, &query)?
        .into_iter()
        .find(|item| item.cursor() == *id)?;
    Some(IdeaOrigin::Feed {
        label: item.provenance().label(),
    })
}

/// The builder `id` in `set`, if the skill gate admits it as a builder.
fn admit_builder(set: &SkillSet, id: &SkillId) -> Result<BuilderIdentity, Box<ArtifactResult>> {
    let failed = |reason| Box::new(ArtifactResult::BuildFailed { reason });
    let capability: CapabilityName = ARTIFACT_BUILD_CAPABILITY
        .parse()
        .map_err(|_| Box::new(ArtifactResult::ArtifactError))?;
    match check_skill_call(set.registry(), id, &capability) {
        Ok(()) => {}
        Err(SkillDenial::UnknownSkill { .. }) => return Err(failed(BuildFailure::UnknownBuilder)),
        Err(SkillDenial::UndeclaredCapability { .. }) => {
            tracing::warn!(builder = %id.as_str(), "builder lacks the build capability: refused by the skill gate");
            return Err(failed(BuildFailure::CapabilityDenied));
        }
    }
    let skill = set
        .registry()
        .get(id)
        .ok_or_else(|| failed(BuildFailure::UnknownBuilder))?;
    Ok(BuilderIdentity {
        skill: id.as_str().to_owned(),
        signer: skill.admission().signer_fingerprint(),
    })
}

/// The fingerprint of the key that signed the skill registered as
/// `skill` in `set` now, if one is.
fn current_signer(set: &SkillSet, skill: &str) -> Option<String> {
    let id: SkillId = skill.parse().ok()?;
    set.registry()
        .get(&id)
        .map(|registered| registered.admission().signer_fingerprint())
}

/// One artifact op's view of the session: its config, key, skills,
/// local channel, store, and clock.
struct Pipeline<'a> {
    config: &'a Config,
    key: &'a dyn OpensStores,
    skills: &'a Skills,
    hub: Option<&'a LocalHub>,
    /// The session's output gate scope.
    scope: String,
    store: SealedArtifactStore<'a>,
    now: i64,
}

impl Pipeline<'_> {
    /// Builds `kind` with `builder` from `inputs`; `idea` is the idea
    /// revision they came from, if any.
    fn build(
        &self,
        kind: &str,
        builder: &str,
        inputs: &[ArtifactInput],
        idea: Option<IdeaRef>,
    ) -> ArtifactResult {
        let failed = |reason| ArtifactResult::BuildFailed { reason };
        let request = match PinnedRequest::pin(kind, inputs) {
            Ok(request) => request,
            Err(reason) => return failed(reason),
        };
        let Some(profile) = self.config.artifacts.kinds.get(&request.kind) else {
            return failed(BuildFailure::UnknownKind);
        };
        let Ok(id) = builder.parse::<SkillId>() else {
            return failed(BuildFailure::UnknownBuilder);
        };
        // One snapshot for the check and the run: a reload in between
        // cannot swap the builder.
        let set: Arc<SkillSet> = self.skills.snapshot();
        let identity = match admit_builder(&set, &id) {
            Ok(identity) => identity,
            Err(result) => return *result,
        };
        let Ok(build) = crate::approval::random_hex(BUILD_ID_PREFIX) else {
            tracing::error!("build id entropy unavailable");
            return ArtifactResult::ArtifactError;
        };
        let mut record = BuildRecord::pending(build, request);
        if let Some(idea) = idea {
            record = record.from_idea(idea);
        }
        match self.store.insert(&record) {
            Ok(true) => {}
            Ok(false) => return failed(BuildFailure::Full),
            Err(error) => {
                tracing::error!(%error, "artifact store failed on a build");
                return ArtifactResult::ArtifactError;
            }
        }
        let result = match self.run_builder(&set, &id, &record) {
            Ok(claim) => {
                let inspection = inspect(profile, &record.request, &claim.content);
                let shown = attribution(Some(&identity), Some(&identity.signer));
                record.complete(identity, claim, inspection, self.now);
                match &record.provenance {
                    Some(provenance) => ArtifactResult::Built {
                        build: record.build.clone(),
                        output_hash: provenance.output_hash.clone(),
                        builder: provenance.builder.skill.clone(),
                        signer: provenance.builder.signer.clone(),
                        attribution: shown,
                        inspection: provenance.inspection.clone(),
                    },
                    None => ArtifactResult::ArtifactError,
                }
            }
            Err(reason) => {
                record.fail(format!("{reason:?}"));
                failed(reason)
            }
        };
        if let Err(error) = self.store.put(&record) {
            tracing::error!(%error, "artifact store failed recording a build");
            return ArtifactResult::ArtifactError;
        }
        result
    }

    /// Runs the builder `id` from `set` on `record`'s pinned request, and
    /// parses what it claims.
    fn run_builder(
        &self,
        set: &SkillSet,
        id: &SkillId,
        record: &BuildRecord,
    ) -> Result<BuilderClaim, BuildFailure> {
        record
            .request
            .builder_input()
            .and_then(|input| {
                self.skills.run(set, id, &input).map_err(|error| {
                    tracing::warn!(build = %record.build, %error, "builder failed");
                    BuildFailure::BuilderFailed
                })
            })
            .and_then(|raw| BuilderClaim::parse(&raw))
    }

    fn deliver(&self, build: &str, to: &DeliveryTarget) -> ArtifactResult {
        let mut record = match self.store.get(build) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return ArtifactResult::Refused {
                    reason: DeliveryRefusal::UnknownBuild,
                };
            }
            Err(error) => {
                tracing::error!(%error, "artifact store failed on a delivery");
                return ArtifactResult::ArtifactError;
            }
        };
        let delivery = sink_for(self.config, self.hub, &self.scope, to).map_or_else(
            || hatchling_artifact::DeliveryRecord {
                target: describe_target(to),
                verdict: None,
                refusal: Some(DeliveryRefusal::BadTarget),
                at: self.now,
            },
            |mut sink| {
                let reviewer = ShareReview::new(
                    held_secrets(self.config, self.key),
                    self.config.artifacts.policy.clone(),
                );
                hatchling_artifact::deliver(&record, &reviewer, sink.as_mut(), self.now)
            },
        );
        tracing::info!(
            build = %record.build,
            target = ?delivery.target,
            verdict = ?delivery.verdict,
            refusal = ?delivery.refusal,
            "artifact delivery"
        );
        let refusal = delivery.refusal;
        record.record_delivery(delivery);
        if let Err(error) = self.store.put(&record) {
            tracing::error!(%error, "artifact store failed recording a delivery");
        }
        match (refusal, &record.provenance) {
            (Some(reason), _) => ArtifactResult::Refused { reason },
            (None, Some(provenance)) => ArtifactResult::Delivered {
                inspection: provenance.inspection.clone(),
            },
            (None, None) => ArtifactResult::ArtifactError,
        }
    }

    fn describe(&self, build: &str) -> ArtifactResult {
        let record = match self.store.get(build) {
            Ok(Some(record)) => record,
            Ok(None) => return ArtifactResult::UnknownBuild,
            Err(error) => {
                tracing::error!(%error, "artifact store failed on a description");
                return ArtifactResult::ArtifactError;
            }
        };
        let provenance = record.provenance.as_ref();
        let state = match record.status {
            BuildStatus::Pending => BuildState::Pending,
            BuildStatus::Completed => BuildState::Completed,
            BuildStatus::Failed { .. } => BuildState::Failed,
        };
        let shown = (state == BuildState::Completed).then(|| {
            let set = self.skills.snapshot();
            let identity = provenance.map(|p| &p.builder);
            let current = identity.and_then(|identity| current_signer(&set, &identity.skill));
            attribution(identity, current.as_deref())
        });
        let delivered = record
            .deliveries
            .iter()
            .filter(|delivery| delivery.delivered())
            .count();
        ArtifactResult::Described {
            view: ArtifactView {
                build: record.build.clone(),
                kind: record.request.kind.clone(),
                state,
                attribution: shown,
                inspection: provenance.map(|p| p.inspection.clone()),
                output_hash: provenance.map(|p| p.output_hash.clone()),
                idea: record.idea.clone(),
                delivered: u32::try_from(delivered).unwrap_or(u32::MAX),
            },
        }
    }

    fn capture_idea(&self, text: &str, origin: IdeaOrigin) -> ArtifactResult {
        let Ok(id) = crate::approval::random_hex(IDEA_ID_PREFIX) else {
            tracing::error!("idea id entropy unavailable");
            return ArtifactResult::ArtifactError;
        };
        let idea = match IdeaRecord::capture(id, text, origin, self.now) {
            Ok(idea) => idea,
            Err(reason) => return ArtifactResult::IdeaRefused { reason },
        };
        match self.store.insert_idea(&idea) {
            Ok(true) => ArtifactResult::IdeaSaved {
                idea: idea.idea,
                revision: idea.revision,
            },
            Ok(false) => ArtifactResult::IdeaRefused {
                reason: IdeaRefusal::Full,
            },
            Err(error) => {
                tracing::error!(%error, "artifact store failed capturing an idea");
                ArtifactResult::ArtifactError
            }
        }
    }

    /// The idea `id`, or the reply to give when there is none.
    fn idea(&self, id: &str) -> Result<IdeaRecord, Box<ArtifactResult>> {
        match self.store.get_idea(id) {
            Ok(Some(idea)) => Ok(idea),
            Ok(None) => Err(Box::new(ArtifactResult::IdeaRefused {
                reason: IdeaRefusal::UnknownIdea,
            })),
            Err(error) => {
                tracing::error!(%error, "artifact store failed reading an idea");
                Err(Box::new(ArtifactResult::ArtifactError))
            }
        }
    }

    fn refine_idea(&self, id: &str, text: &str) -> ArtifactResult {
        let mut idea = match self.idea(id) {
            Ok(idea) => idea,
            Err(result) => return *result,
        };
        if let Err(reason) = idea.refine(text, self.now) {
            return ArtifactResult::IdeaRefused { reason };
        }
        match self.store.put_idea(&idea) {
            Ok(true) => ArtifactResult::IdeaSaved {
                idea: idea.idea,
                revision: idea.revision,
            },
            Ok(false) => ArtifactResult::IdeaRefused {
                reason: IdeaRefusal::UnknownIdea,
            },
            Err(error) => {
                tracing::error!(%error, "artifact store failed refining an idea");
                ArtifactResult::ArtifactError
            }
        }
    }

    /// Stores a changed idea, answering as `saved`.
    fn save_idea(&self, idea: &IdeaRecord, saved: ArtifactResult) -> ArtifactResult {
        match self.store.put_idea(idea) {
            Ok(true) => saved,
            Ok(false) => ArtifactResult::IdeaRefused {
                reason: IdeaRefusal::UnknownIdea,
            },
            Err(error) => {
                tracing::error!(%error, "artifact store failed saving an idea");
                ArtifactResult::ArtifactError
            }
        }
    }

    /// One page of ideas, oldest first, after the idea `after`.
    fn list_ideas(&self, after: Option<&str>) -> ArtifactResult {
        let ideas = match self.store.ideas() {
            Ok(ideas) => ideas,
            Err(error) => {
                tracing::error!(%error, "artifact store failed listing ideas");
                return ArtifactResult::ArtifactError;
            }
        };
        let start = after.map_or(0, |after| {
            ideas
                .iter()
                .position(|idea| idea.idea == after)
                .map_or(ideas.len(), |at| at.saturating_add(1))
        });
        let rest = ideas.get(start..).unwrap_or_default();
        let page: Vec<_> = rest
            .iter()
            .take(IDEA_PAGE)
            .map(IdeaRecord::summary)
            .collect();
        let next = (rest.len() > IDEA_PAGE)
            .then(|| page.last().map(|summary| summary.idea.clone()))
            .flatten();
        ArtifactResult::Ideas { ideas: page, next }
    }

    /// Adds a development note to the idea `id`.
    fn note_idea(&self, id: &str, note: &str) -> ArtifactResult {
        let mut idea = match self.idea(id) {
            Ok(idea) => idea,
            Err(result) => return *result,
        };
        if let Err(reason) = idea.note(note, self.now) {
            return ArtifactResult::IdeaRefused { reason };
        }
        let saved = ArtifactResult::IdeaSaved {
            idea: idea.idea.clone(),
            revision: idea.revision,
        };
        self.save_idea(&idea, saved)
    }

    /// Proposes activating the idea `id`: pins the exact request, keeps
    /// it as awaiting confirmation, and answers with it.
    fn propose_activation(
        &self,
        id: &str,
        kind: &str,
        builder: &str,
        description: &str,
    ) -> ArtifactResult {
        let failed = |reason| ArtifactResult::BuildFailed { reason };
        let mut idea = match self.idea(id) {
            Ok(idea) => idea,
            Err(result) => return *result,
        };
        let request = match idea.pin(kind) {
            Ok(request) => request,
            Err(reason) => return failed(reason),
        };
        if self.config.artifacts.kinds.get(&request.kind).is_none() {
            return failed(BuildFailure::UnknownKind);
        }
        if builder.parse::<SkillId>().is_err() {
            return failed(BuildFailure::UnknownBuilder);
        }
        let Ok(activation) = crate::approval::random_hex(ACTIVATION_ID_PREFIX) else {
            tracing::error!("activation id entropy unavailable");
            return ArtifactResult::ArtifactError;
        };
        let proposal = match idea.propose(activation, &request, builder, description) {
            Ok(proposal) => proposal,
            Err(reason) => return ArtifactResult::IdeaRefused { reason },
        };
        let saved = self.save_idea(&idea, ArtifactResult::ActivationProposed { proposal });
        let ArtifactResult::ActivationProposed { proposal } = saved else {
            return saved;
        };
        // Put to the owner (WO-027), or why the owner wasn't asked.
        match crate::approval_board::board(self.config).propose_activation(
            &proposal,
            &self.scope,
            self.now,
        ) {
            Ok(_) => ArtifactResult::ActivationProposed { proposal },
            Err(refusal) => ArtifactResult::ApprovalRefused {
                reason: crate::approval_board::refused_as(&refusal),
                proposal,
            },
        }
    }

    /// Activates `activation` with `confirmation`: spends it, and builds
    /// exactly the request the user confirmed.
    fn activate(&self, activation: &str, confirmation: &str) -> ArtifactResult {
        let refused = |reason| ArtifactResult::IdeaRefused { reason };
        let ideas = match self.store.ideas() {
            Ok(ideas) => ideas,
            Err(error) => {
                tracing::error!(%error, "artifact store failed finding an activation");
                return ArtifactResult::ArtifactError;
            }
        };
        let Some(mut idea) = ideas
            .into_iter()
            .find(|idea| idea.pinned(activation).is_some())
        else {
            return refused(IdeaRefusal::NoProposal);
        };
        let Some((request, proposal)) = idea.pinned(activation) else {
            return refused(IdeaRefusal::NoProposal);
        };
        if !crate::activation::take_confirmation(
            confirmation,
            activation,
            &proposal.request_hash,
            self.now,
        ) {
            tracing::warn!(%activation, "activation refused: not confirmed");
            return refused(IdeaRefusal::NotConfirmed);
        }
        let inputs: Vec<ArtifactInput> = request
            .inputs
            .iter()
            .map(|input| ArtifactInput {
                name: input.name.clone(),
                content: input.content.clone(),
            })
            .collect();
        let (kind, builder, revision) = (
            request.kind,
            proposal.builder.clone(),
            proposal.idea.clone(),
        );
        let result = self.build(&kind, &builder, &inputs, Some(revision));
        if let ArtifactResult::Built { build, .. } = &result {
            idea.activated(build.clone());
            if let Err(error) = self.store.put_idea(&idea) {
                tracing::error!(%error, "artifact store failed recording an activation");
            }
        }
        result
    }
}

/// Activates `activation` with `confirmation`, which the broker minted
/// when the owner approved it (WO-027).
///
/// The broker's own run of the stored action, never a session's. The
/// outcome is delivered, as a session's would be, in the broker's
/// scheduler scope.
#[must_use]
pub fn activate_confirmed(
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
    activation: &str,
    confirmation: &str,
    now: i64,
) -> ArtifactResult {
    if !config.artifacts.enabled() {
        return ArtifactResult::Denied;
    }
    Pipeline {
        config,
        key,
        skills,
        hub,
        scope: String::from(crate::output_gate::SCHEDULER_SCOPE),
        store: SealedArtifactStore::new(&config.artifacts.root, key),
        now,
    }
    .activate(activation, confirmation)
}

/// The local channel's ingress client, as a delivery target, through the
/// output gate in the session's scope.
struct HubSink<'a> {
    hub: &'a LocalHub,
    scope: &'a str,
}

impl Sink for HubSink<'_> {
    fn target(&self) -> String {
        String::from("channel:local")
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), String> {
        let text = String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?;
        self.hub
            .deliver(
                &Message {
                    sender: SenderId(String::from(ARTIFACT_SENDER)),
                    channel: ChannelId::local(),
                    text,
                },
                self.scope,
            )
            .map_err(|error| error.to_string())
    }
}

/// A new, owner-only file in the operator's workspace, as a delivery
/// target. It never replaces a file.
struct WorkspaceSink {
    path: PathBuf,
    name: String,
}

impl Sink for WorkspaceSink {
    fn target(&self) -> String {
        format!("workspace:{}", self.name)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), String> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut file| file.write_all(bytes))
            .map_err(|e| e.to_string())
    }
}

/// Whether `name` is a plain workspace file name: one path component of
/// ASCII letters, digits, `.`, `_`, `-`, not starting with a dot.
#[must_use]
pub fn is_workspace_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_WORKSPACE_NAME_BYTES
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The sink for `to`, if this session can deliver there.
fn sink_for<'a>(
    config: &Config,
    hub: Option<&'a LocalHub>,
    scope: &'a str,
    to: &DeliveryTarget,
) -> Option<Box<dyn Sink + 'a>> {
    match to {
        DeliveryTarget::Channel { channel } => {
            let local = ChannelId::local().0;
            let hub = hub.filter(|_| *channel == local && config.channels.contains(&local))?;
            Some(Box::new(HubSink { hub, scope }))
        }
        DeliveryTarget::Workspace { name } => {
            let dir = config.artifacts.workspace.as_ref()?;
            if !is_workspace_name(name) {
                return None;
            }
            let sink: Box<dyn Sink + 'a> = Box::new(WorkspaceSink {
                path: dir.join(name),
                name: name.clone(),
            });
            Some(sink)
        }
    }
}

/// How a target is named in a record when no sink could be made for it.
fn describe_target(to: &DeliveryTarget) -> String {
    match to {
        DeliveryTarget::Channel { channel } => format!("channel:{channel}"),
        DeliveryTarget::Workspace { name } => format!("workspace:{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchling_vault::MasterKey;

    #[test]
    fn workspace_names_are_one_plain_component() {
        for good in [
            "report.md",
            "a",
            "Report_2026-10-01.txt",
            &"x".repeat(MAX_WORKSPACE_NAME_BYTES),
        ] {
            assert!(is_workspace_name(good), "{good}");
        }
        for bad in [
            "",
            ".hidden",
            "..",
            "../escape",
            "dir/file",
            "a b",
            "a\0b",
            "ré.md",
            &"x".repeat(MAX_WORKSPACE_NAME_BYTES + 1),
        ] {
            assert!(!is_workspace_name(bad), "{bad:?}");
        }
        assert_eq!(MAX_WORKSPACE_NAME_BYTES, 128);
    }

    #[test]
    fn an_entry_at_the_vault_bound_is_stored_and_one_byte_over_is_refused() {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-artifact-entry-bound-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let vault = SealedVault::open_in(&root, &key, hatchling_vault::Store::ARTIFACTS).unwrap();
        // A JSON string is its text plus two quotes.
        let at_bound = "x".repeat(MAX_SECRET_BYTES - 2);
        assert!(write_json(&vault, "probe", &at_bound).is_ok());
        let read: Option<String> = read_json(&vault, "probe").unwrap();
        assert_eq!(read.map(|s| s.len()), Some(MAX_SECRET_BYTES - 2));
        let over = "x".repeat(MAX_SECRET_BYTES - 1);
        let error = write_json(&vault, "probe", &over).unwrap_err();
        assert!(
            matches!(&error, Error::ArtifactStore { reason } if reason.contains("over the bound")),
            "{error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn store_entry_names_are_vault_names() {
        assert_eq!(entry_name(&BUILDS, "art-ab12"), "build-art-ab12");
        assert_eq!(entry_name(&IDEAS, "idea-ab12"), "idea-idea-ab12");
        assert!(secret_name(&entry_name(&BUILDS, "art-ab12")).is_ok());
        assert!(secret_name(&entry_name(&IDEAS, "idea-ab12")).is_ok());
        assert!(secret_name(&entry_name(&BUILDS, "../x")).is_err());
        assert_eq!(
            (BUILDS.index, BUILDS.max, IDEAS.index, IDEAS.max),
            ("build-index", 64, "idea-index", 64)
        );
        assert_eq!(
            (
                MAX_BUILDS,
                MAX_IDEAS,
                BUILD_ID_PREFIX,
                IDEA_ID_PREFIX,
                ARTIFACT_SENDER
            ),
            (64, 64, "art-", "idea-", "broker:artifact")
        );
    }

    /// A channel delivery goes to the local hub, through its gate, in the
    /// session's scope; with no hub, or to any other channel, there is no
    /// sink.
    #[test]
    fn an_artifact_for_the_channel_goes_through_the_hub_in_its_session() {
        let config = crate::api_auth::tests::owner_config(None);
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let shown = Arc::new(Mutex::new(Vec::new()));
        let sink_shown = Arc::clone(&shown);
        let _ = hub.attach(Box::new(move |cleared: &hatchling_channel::Cleared| {
            sink_shown.lock().unwrap().push(cleared.message().clone());
            Ok(())
        }));
        let local = DeliveryTarget::Channel {
            channel: String::from("local"),
        };
        let mut sink = sink_for(&config, Some(&hub), "session:a", &local).unwrap();
        assert_eq!(sink.target(), "channel:local");
        sink.put(b"the build").unwrap();
        assert!(sink.put(&[0xff]).is_err(), "not UTF-8");
        let shown = shown.lock().unwrap().clone();
        assert_eq!(
            shown
                .iter()
                .map(|m| (m.sender.0.as_str(), m.text.as_str()))
                .collect::<Vec<_>>(),
            [(ARTIFACT_SENDER, "the build")]
        );
        assert!(sink_for(&config, None, "session:a", &local).is_none());
        let elsewhere = DeliveryTarget::Channel {
            channel: String::from("mail"),
        };
        assert!(sink_for(&config, Some(&hub), "session:a", &elsewhere).is_none());
        let workspace = DeliveryTarget::Workspace {
            name: String::from("report.md"),
        };
        assert!(
            sink_for(&config, Some(&hub), "session:a", &workspace).is_none(),
            "no workspace"
        );
    }

    /// the gates' read of every held secret names what it reads,
    /// and is recorded as the broker's, for its purpose.
    #[test]
    fn the_gates_read_every_held_secret_on_record() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("held-read");
        config.vault.root = dir.0.join("vault");
        config.vault.grants = vec![crate::VaultGrant {
            action: hatchling_core::VaultAction::Fetch,
            name: "token".parse().unwrap(),
        }];
        config.egress.grants = vec![String::from("api.example.com:443")];
        let key = MasterKey::generate().unwrap();
        SealedVault::open(&config.vault.root, &key)
            .unwrap()
            .store(
                &"token".parse().unwrap(),
                &Secret::new(b"v".to_vec()).unwrap(),
            )
            .unwrap();
        let names: Vec<String> = held_secret_names(&config).into_iter().collect();
        assert_eq!(names, ["egress-api-example-com-443", "token"]);
        assert_eq!(
            held_secret_values(&config, &key, "artifact-gate").unwrap(),
            [Zeroizing::new(b"v".to_vec())]
        );
        let (accesses, _) = crate::vault_access::read_log(&config.vault.root).unwrap();
        let [only] = accesses.as_slice() else {
            unreachable!("only the recorded read writes: {accesses:?}");
        };
        assert_eq!(
            only.caller,
            crate::vault_access::Caller::Broker {
                purpose: String::from("artifact-gate")
            }
        );
        assert_eq!(only.names, names);
        assert!(held_secrets(&config, &key).unwrap().exposed_in(b"v"));
        let wrong = MasterKey::generate().unwrap();
        assert!(held_secret_values(&config, &wrong, "output-gate").is_err());
        let (accesses, _) = crate::vault_access::read_log(&config.vault.root).unwrap();
        assert_eq!(
            accesses.last().map(|access| access.outcome),
            Some(crate::vault_access::Outcome::Failed)
        );
    }

    #[test]
    fn the_store_seals_under_its_own_label() {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-artifacts-label-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let name: SecretName = "probe".parse().unwrap();
        let secret = Secret::new(b"p".to_vec()).unwrap();
        SealedArtifactStore::new(&root, &key)
            .vault()
            .unwrap()
            .store(&name, &secret)
            .unwrap();
        let own = SealedVault::open_in(&root, &key, hatchling_vault::Store::ARTIFACTS)
            .unwrap()
            .fetch(&name);
        let secrets = SealedVault::open(&root, &key).and_then(|vault| vault.fetch(&name));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(own.unwrap().as_bytes(), b"p");
        assert!(secrets.is_err(), "opened under the secrets label");
    }
}
