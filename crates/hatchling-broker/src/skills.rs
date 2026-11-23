//! Skill loading: from disk to registry.
//!
//! At broker startup, the broker reads each skill subdirectory under
//! the configured skills dir, verifies the signed manifest against the
//! trust root, checks every file the manifest lists against its signed
//! size and SHA-256, pins the program's hash (the registry), and
//! registers the skill. The skills dir and each skill's own directory
//! must be root's or the broker's and closed to other writers, as the
//! trust-roots file is (#613). Any failure — bad signature, over-privileged
//! manifest, unreadable code — refuses that skill loudly and continues
//! with the rest. A broken skill never stops the broker; it just never
//! runs.
//!
//! The result is [`Skills`]: broker-owned, borrowed by every session, and
//! rebuilt whole when the trust roots rotate (see [`crate::trust_roots`]).
//! Cells never hold it; they reach skills only through the session
//! protocol.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use hatchling_core::ui::{SkillFile, SkillListing, SkillState};
use hatchling_skill::{Admission, MANIFEST_FILE, SkillId, SkillRegistry, verify_admission};

use crate::skill_run::{SkillRunError, SkillRuntime, run_skill};
use crate::trust_roots::{TrustRoots, read_trust_roots_file};
use crate::{Error, SkillConfig};

/// A skill as loaded from disk: the verified admission plus the code
/// bytes that were hashed at registration.
pub struct LoadedSkill {
    /// The skill's directory name (for logging).
    pub dir_name: String,
    /// The verified admission.
    pub admission: Admission,
    /// The raw code bytes (pinned by hash in the registry).
    pub code: Vec<u8>,
}

/// A skill the loader refused, and why.
///
/// Every refusal is logged at error level as it happens; the list makes
/// the same refusals checkable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The skill's directory name, or the skills dir itself when it
    /// cannot be read at all.
    pub dir_name: String,
    /// Why it was refused.
    pub reason: String,
}

/// What the owner enabled (WO-046): each skill's name, and the digest
/// they enabled. A skill runs only when its digest now is that one.
pub type Enabled = HashMap<String, String>;

/// A skill that verifies, enabled or not: what the owner is shown.
pub struct Found {
    /// The skill's directory name.
    pub dir_name: String,
    /// The verified admission (its capabilities those it declares).
    pub admission: Admission,
    /// Its digest: what enabling is bound to.
    pub digest: String,
    /// Where it stands against what the owner enabled.
    pub state: SkillState,
}

/// What [`load_skills`] found.
pub struct LoadOutcome {
    /// The admitted skills, code hashes pinned.
    pub registry: SkillRegistry,
    /// The admitted skills as loaded.
    pub loaded: Vec<LoadedSkill>,
    /// Every skill that verifies, enabled or not, in the order met.
    pub found: Vec<Found>,
    /// Every refusal, in the order met.
    pub refused: Vec<Refusal>,
}

impl LoadOutcome {
    /// Takes a skill that verifies: found either way, and registered when
    /// `enabled` holds it at `digest`.
    fn take(
        &mut self,
        dir_name: String,
        admission: Admission,
        code: Vec<u8>,
        digest: String,
        enabled: &Enabled,
    ) {
        let state = match enabled.get(&admission.manifest().name) {
            Some(on) if *on == digest => SkillState::Enabled,
            Some(_) => SkillState::Changed,
            None => SkillState::Waiting,
        };
        self.found.push(Found {
            dir_name: dir_name.clone(),
            admission: admission.clone(),
            digest,
            state,
        });
        if state != SkillState::Enabled {
            tracing::info!(dir = ?dir_name, ?state, "skill found, waiting for the owner");
            return;
        }
        match self.registry.register(admission.clone(), &code) {
            Ok(id) => {
                tracing::info!(skill = %id, dir = ?dir_name, "skill admitted and registered");
                self.loaded.push(LoadedSkill {
                    dir_name,
                    admission,
                    code,
                });
            }
            Err(err) => {
                let err = Error::SkillLoad {
                    skill: dir_name.clone(),
                    reason: format!("registration refused: {err}"),
                };
                tracing::error!(dir = ?dir_name, %err, "skill refused");
                self.refused.push(Refusal {
                    dir_name,
                    reason: err.to_string(),
                });
            }
        }
    }
}

/// Load all skills from the configured directory; register those the
/// owner enabled as they are (`enabled`).
///
/// Each subdirectory must contain `manifest.json` (the signed manifest,
/// strict JSON, format 2) and every file it lists. The manifest is
/// verified via [`verify_admission`] against the trust root, the grant
/// the capabilities it declares; every listed file is read and checked against its signed size
/// and SHA-256, and the program's hash pinned at registration.
///
/// Every signature is verified under `roots`, primary first; a skill
/// admits under whichever root signed it. A skill that verifies is
/// [`Found`] either way, and runs only when `enabled` holds its name with
/// its digest now: never enabled it waits, enabled at another digest it
/// is changed, and both wait for the owner (WO-046). An enabled skill is
/// granted exactly the capabilities its program declares: the owner saw
/// those when they enabled it. A skill that fails to load is
/// logged loudly and refused, not retried. The broker continues with the
/// remaining skills. A disabled config (no skills dir) loads nothing and
/// touches no filesystem.
pub fn load_skills(config: &SkillConfig, roots: &TrustRoots, enabled: &Enabled) -> LoadOutcome {
    let mut outcome = LoadOutcome {
        registry: SkillRegistry::new(),
        loaded: Vec::new(),
        found: Vec::new(),
        refused: Vec::new(),
    };
    if !config.is_enabled() {
        tracing::info!("no skills dir configured: skills disabled");
        return outcome;
    }

    let entries = match std::fs::read_dir(&config.skills_dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::error!(
                dir = %config.skills_dir.display(),
                %err,
                "skills dir unreadable: all skills refused"
            );
            outcome.refused.push(Refusal {
                dir_name: config.skills_dir.display().to_string(),
                reason: format!("skills dir unreadable: {err}"),
            });
            return outcome;
        }
    };

    if let Err(reason) = closed_to_others(&config.skills_dir) {
        tracing::error!(
            dir = %config.skills_dir.display(),
            %reason,
            "skills dir refused: all skills refused"
        );
        outcome.refused.push(Refusal {
            dir_name: config.skills_dir.display().to_string(),
            reason: format!("skills dir refused: {reason}"),
        });
        return outcome;
    }

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(%err, "skipping unreadable skills dir entry");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        match load_one(&path, &dir_name, roots) {
            Ok((admission, code, digest)) => {
                outcome.take(dir_name, admission, code, digest, enabled);
            }
            Err(err) => {
                tracing::error!(dir = ?dir_name, %err, "skill refused");
                outcome.refused.push(Refusal {
                    dir_name,
                    reason: err.to_string(),
                });
            }
        }
    }

    tracing::info!(
        loaded = outcome.loaded.len(),
        refused = outcome.refused.len(),
        "skill loading complete"
    );
    outcome
}

/// One immutable snapshot of the admitted skills: what a session checks a
/// call against and runs it from. A trust-root reload replaces it whole;
/// it is never edited in place.
#[derive(Debug, Default)]
pub struct SkillSet {
    registry: SkillRegistry,
    packages: HashMap<SkillId, PathBuf>,
    listing: Vec<SkillListing>,
}

impl SkillSet {
    /// Loads every skill under the configured dir against `roots`, the
    /// owner's `enabled` deciding which run.
    fn build(config: &SkillConfig, roots: &TrustRoots, enabled: &Enabled) -> Self {
        let outcome = load_skills(config, roots, enabled);
        let mut listing: Vec<SkillListing> = outcome.found.iter().map(listed).collect();
        listing.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.version.cmp(&b.version)));
        let packages = outcome
            .loaded
            .iter()
            .map(|skill| {
                let manifest = skill.admission.manifest();
                (
                    SkillId::new(&manifest.name, &manifest.version),
                    config.skills_dir.join(&skill.dir_name),
                )
            })
            .collect();
        Self {
            registry: outcome.registry,
            packages,
            listing,
        }
    }

    /// Every skill found that verifies, enabled or not, by name: what the
    /// owner is shown (WO-046).
    #[must_use]
    pub fn listing(&self) -> &[SkillListing] {
        &self.listing
    }

    /// The directory `id` was loaded from, when it is in this set.
    #[must_use]
    pub fn package(&self, id: &SkillId) -> Option<&Path> {
        self.packages.get(id).map(PathBuf::as_path)
    }

    /// The admitted skills, for call-time checks.
    #[must_use]
    pub const fn registry(&self) -> &SkillRegistry {
        &self.registry
    }

    /// The ids in this set, sorted.
    fn ids(&self) -> Vec<SkillId> {
        let mut ids = self.registry.list();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids
    }
}

/// A found skill as the owner is shown it.
fn listed(found: &Found) -> SkillListing {
    let manifest = found.admission.manifest();
    SkillListing {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        signer: found.admission.signer_fingerprint(),
        digest: found.digest.clone(),
        capabilities: manifest
            .capabilities()
            .iter()
            .map(|capability| capability.as_str().to_owned())
            .collect(),
        instructions: manifest.instructions.clone(),
        program: manifest.entry_point().map(str::to_owned),
        files: manifest
            .files
            .iter()
            .map(|entry| SkillFile {
                path: entry.path.clone(),
                bytes: entry.bytes,
            })
            .collect(),
        state: found.state,
    }
}

/// The broker's skills, once published at its start: what every run's
/// session reaches (WO-046).
static LIVE: std::sync::OnceLock<&'static Skills> = std::sync::OnceLock::new();

/// Makes `skills` the broker's for as long as it serves, and what the
/// runs it launches reach ([`live`]). The first published stays: a
/// broker publishes once.
pub fn publish(skills: Skills) -> &'static Skills {
    let skills: &'static Skills = Box::leak(Box::new(skills));
    LIVE.get_or_init(|| skills)
}

/// The broker's skills, when it has published them: none in a process
/// that serves no broker, a test's included.
#[must_use]
pub fn live() -> Option<&'static Skills> {
    LIVE.get().copied()
}

/// What one trust-roots poll did (see [`crate::trust_roots`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reload {
    /// Skills are disabled: there is no trust-roots file to watch.
    Disabled,
    /// The file, or its failure, is exactly what the last poll acted on.
    Unchanged,
    /// The file could not be read or parsed. The current set is kept and
    /// nothing new is trusted.
    Rejected {
        /// Why the file was refused.
        reason: String,
    },
    /// A new set was built against the new roots and swapped in.
    Applied {
        /// Skills in the new set that the old one lacked.
        added: Vec<SkillId>,
        /// Skills in the old set that the new one lacks.
        dropped: Vec<SkillId>,
    },
}

/// What the watcher last acted on.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Applied(Vec<u8>),
    Failed(String),
}

/// Where the trust roots come from, what the watcher last saw there, and
/// the last file it applied.
///
/// No skill's code is pinned across reloads: the owner enabled a digest,
/// which fixes every file's bytes, so the same `name@version` with other
/// code is another digest, listed changed, and never runs (WO-046).
#[derive(Debug)]
struct RootsSource {
    config: SkillConfig,
    seen: Mutex<Seen>,
    /// The trust-roots file last applied: a rebuild for the owner uses it,
    /// whatever the file says now.
    applied: Mutex<Vec<u8>>,
    /// Held across each rebuild, so the last one built is the last one
    /// swapped in: a reload can't put back a skill the owner just disabled.
    rebuild: Mutex<()>,
}

/// The broker's skills: owned by the broker, borrowed by every session.
///
/// Holds the current [`SkillSet`] behind a lock that is only ever held
/// for a pointer read or a pointer swap. A session takes one
/// [`snapshot`](Self::snapshot) per op; [`reload`](Self::reload) builds a
/// whole new set with no lock held and swaps it in.
///
/// `Default` is the empty set, under which every skill op names an
/// unknown skill. Without a [`SkillRuntime`] a call that passes every
/// check still fails closed.
#[derive(Debug, Default)]
pub struct Skills {
    current: RwLock<Arc<SkillSet>>,
    runtime: Option<SkillRuntime>,
    source: Option<RootsSource>,
    harnesses: crate::harness::Harnesses,
    enabled: RwLock<Enabled>,
}

impl Skills {
    /// Startup: reads the trust-roots file and loads every skill under the
    /// configured dir against it (see [`load_skills`]). Broken skills are
    /// refused loudly and the rest load: a bad skill never stops the
    /// broker. A disabled config loads nothing and reads no file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] (`HATCHLING_SKILL_TRUST_ROOTS`) if
    /// skills are enabled and the trust-roots file is missing, unreadable,
    /// untrusted, or malformed: the broker refuses to start rather than
    /// run skills without knowing which keys it trusts.
    pub fn load(config: &SkillConfig) -> Result<Self, Error> {
        if !config.is_enabled() {
            tracing::info!("no skills dir configured: skills disabled");
            return Ok(Self::default());
        }
        let invalid = |reason: String| Error::InvalidConfig {
            key: "HATCHLING_SKILL_TRUST_ROOTS",
            reason,
        };
        let bytes = read_trust_roots_file(&config.trust_roots_file).map_err(invalid)?;
        let roots = TrustRoots::parse(&bytes).map_err(|reason| {
            invalid(format!("{}: {reason}", config.trust_roots_file.display()))
        })?;
        // Nothing is enabled until the owner's enablements are handed over
        // (`set_enabled`, from the sealed store, at the broker's start).
        let set = SkillSet::build(config, &roots, &Enabled::new());
        Ok(Self {
            current: RwLock::new(Arc::new(set)),
            runtime: None,
            source: Some(RootsSource {
                config: config.clone(),
                seen: Mutex::new(Seen::Applied(bytes.clone())),
                applied: Mutex::new(bytes),
                rebuild: Mutex::new(()),
            }),
            harnesses: crate::harness::Harnesses::default(),
            enabled: RwLock::new(Enabled::new()),
        })
    }

    /// What the owner has enabled, as last handed over.
    #[must_use]
    pub fn enabled(&self) -> Enabled {
        match self.enabled.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Hands over what the owner has enabled, and rebuilds the set under
    /// the trust roots last applied: a skill enabled runs from now, a
    /// skill disabled stops (WO-046), even while the roots file is bad.
    /// With skills off, nothing is built.
    pub fn set_enabled(&self, enabled: Enabled) {
        self.rebuild(Some(enabled));
    }

    /// Rebuilds the set from the skills dir as it is now, under what the
    /// owner enabled: a skill added since is found, one changed since is
    /// listed changed and stops. The owner's list and enable do this first,
    /// so they see, and enable, each skill as it is.
    pub fn refresh(&self) {
        self.rebuild(None);
    }

    /// Rebuilds under `enabled`, handed over now, or what was last.
    fn rebuild(&self, enabled: Option<Enabled>) {
        let Some(source) = &self.source else {
            if let Some(enabled) = enabled {
                *write(&self.enabled) = enabled;
            }
            return;
        };
        let _one = lock(&source.rebuild);
        let enabled = enabled.map_or_else(
            || self.enabled(),
            |enabled| {
                write(&self.enabled).clone_from(&enabled);
                enabled
            },
        );
        let bytes = lock(&source.applied).clone();
        let new = match TrustRoots::parse(&bytes) {
            Ok(roots) => SkillSet::build(&source.config, &roots, &enabled),
            // It parsed when it was applied; should it ever not, nothing
            // runs rather than what the owner may have just disabled.
            Err(reason) => {
                tracing::error!(%reason, "the trust roots applied don't parse: no skill runs");
                SkillSet::default()
            }
        };
        self.swap(new);
    }

    /// Puts `new` in place of the current set: one pointer store.
    fn swap(&self, new: SkillSet) {
        *write(&self.current) = Arc::new(new);
    }

    /// The same skills, run by `runtime`.
    #[must_use]
    pub fn with_runtime(mut self, runtime: SkillRuntime) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// The same skills, with `harnesses` as the external harness skills
    /// . Without them, no harness skill exists.
    #[must_use]
    pub fn with_harnesses(mut self, harnesses: crate::harness::Harnesses) -> Self {
        self.harnesses = harnesses;
        self
    }

    /// Handles `op` when it names a harness skill; `None` for any other.
    /// `keys` reads a real CLI's vendor key.
    #[must_use]
    pub fn handle_harness(
        &self,
        op: &hatchling_core::SkillOp,
        keys: crate::harness::HarnessKeys<'_>,
    ) -> Option<hatchling_core::SkillResult> {
        self.harnesses.handle(op, self.runtime.as_ref(), keys)
    }

    /// The current set. Hold it for the whole op: a reload that swaps in
    /// a new set leaves this one intact until its last holder is done.
    #[must_use]
    pub fn snapshot(&self) -> Arc<SkillSet> {
        match self.current.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Runs the skill `id` from `set` on `input` in a fresh skill cell.
    /// The caller has already checked the call against the same `set`
    /// ([`check_skill_call`](crate::check_skill_call)).
    ///
    /// # Errors
    ///
    /// Returns [`SkillRunError::Unavailable`] without a runtime or for an
    /// id that `set` does not hold, and every error of [`run_skill`].
    pub fn run(&self, set: &SkillSet, id: &SkillId, input: &str) -> Result<String, SkillRunError> {
        self.run_in(set, id, input, None)
    }

    /// [`Self::run`], for an agent's run: `tree` kills the skill's cell on
    /// a hit, as it does the rest of the run's.
    ///
    /// # Errors
    ///
    /// As [`Self::run`].
    pub(crate) fn run_in(
        &self,
        set: &SkillSet,
        id: &SkillId,
        input: &str,
        tree: Option<&crate::held_check::HeldHit>,
    ) -> Result<String, SkillRunError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(SkillRunError::Unavailable("no skill runtime"))?;
        let skill = set
            .registry
            .get(id)
            .ok_or(SkillRunError::Unavailable("skill not registered"))?;
        let package = set
            .packages
            .get(id)
            .ok_or(SkillRunError::Unavailable("no package for the skill"))?;
        run_skill(&runtime.clone().in_tree(tree), skill, package, input)
    }

    /// One poll of the trust-roots file: the state machine in
    /// [`crate::trust_roots`]. Reads and rebuilds holding no lock; the swap
    /// is one pointer store. Logs only on a change of state, so a file that
    /// stays bad is reported once.
    pub fn reload(&self) -> Reload {
        let Some(source) = &self.source else {
            return Reload::Disabled;
        };
        let file = &source.config.trust_roots_file;
        let read = read_trust_roots_file(file)
            .and_then(|bytes| TrustRoots::parse(&bytes).map(|roots| (bytes, roots)));
        let observed = match &read {
            Ok((bytes, _)) => Seen::Applied(bytes.clone()),
            Err(reason) => Seen::Failed(reason.clone()),
        };
        let _one = lock(&source.rebuild);
        if *seen(&source.seen) == observed {
            return Reload::Unchanged;
        }
        let (bytes, roots) = match read {
            Ok(parsed) => parsed,
            Err(reason) => {
                tracing::error!(
                    file = %file.display(),
                    %reason,
                    "trust-roots file rejected: keeping the current skills"
                );
                *seen(&source.seen) = observed;
                return Reload::Rejected { reason };
            }
        };
        let old = self.snapshot();
        let new = SkillSet::build(&source.config, &roots, &self.enabled());
        let (old_ids, new_ids) = (old.ids(), new.ids());
        let added: Vec<SkillId> = new_ids
            .iter()
            .filter(|id| !old_ids.contains(id))
            .cloned()
            .collect();
        let dropped: Vec<SkillId> = old_ids
            .iter()
            .filter(|id| !new_ids.contains(id))
            .cloned()
            .collect();
        self.swap(new);
        lock(&source.applied).clone_from(&bytes);
        *seen(&source.seen) = Seen::Applied(bytes);
        for id in &dropped {
            tracing::warn!(skill = %id, "skill dropped at trust-roots reload");
        }
        tracing::info!(
            file = %file.display(),
            roots = roots.keys().len(),
            added = ?added.iter().map(SkillId::as_str).collect::<Vec<_>>(),
            dropped = ?dropped.iter().map(SkillId::as_str).collect::<Vec<_>>(),
            "trust roots reloaded"
        );
        Reload::Applied { added, dropped }
    }

    /// The trust-roots watcher: [`reload`](Self::reload) every `interval`,
    /// forever. Returns at once when skills are disabled.
    pub fn watch(&self, interval: Duration) {
        if self.source.is_none() {
            return;
        }
        loop {
            std::thread::sleep(interval);
            self.reload();
        }
    }
}

/// The watcher's last-seen state, whatever a panicked holder left.
fn seen(state: &Mutex<Seen>) -> MutexGuard<'_, Seen> {
    lock(state)
}

/// A lock, whatever a panicked holder left (nothing here panics while
/// holding one; this only keeps a poisoned lock from wedging the broker).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A write lock, whatever a panicked holder left, as [`lock`].
fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Load and verify a single skill directory.
fn load_one(
    dir: &Path,
    dir_name: &str,
    roots: &TrustRoots,
) -> Result<(Admission, Vec<u8>, String), Error> {
    // Refuse symlinked skill directories: a symlink at the root would
    // let the loader read outside the skills dir. And refuse one anyone
    // but root or the broker could write, as the trust-roots file is.
    refuse_symlink(dir, dir_name, "skill directory")?;
    closed_to_others(dir).map_err(|reason| Error::SkillLoad {
        skill: dir_name.to_owned(),
        reason,
    })?;

    // Read the signed manifest (refusing symlinks).
    let manifest_path = dir.join(MANIFEST_FILE);
    refuse_symlink(&manifest_path, dir_name, "manifest")?;
    let manifest_bytes = std::fs::read(&manifest_path).map_err(|err| Error::SkillLoad {
        skill: dir_name.to_owned(),
        reason: format!("cannot read {}: {err}", manifest_path.display()),
    })?;

    // Parse the signed manifest (strict: unknown/duplicate fields refused).
    // (A retired format v1 manifest is refused here, saying how to fix it.)
    let signed = hatchling_skill::SignedManifest::from_json(&manifest_bytes).map_err(|err| {
        Error::SkillLoad {
            skill: dir_name.to_owned(),
            reason: err.to_string(),
        }
    })?;

    // Verified with the capabilities its program declares as the grant:
    // the signature covers them, and they are what the owner is shown and
    // enables (WO-046). Enabling binds to the digest, so a skill that
    // later declares more is changed and runs no more until re-enabled.
    let declared = signed.manifest.capabilities().iter().cloned().collect();
    let admission = admit_under_roots(&signed, roots, &declared, dir_name)?;

    // Read every listed file (each a safe relative path, by
    // Manifest::validate during verify_admission), refusing symlinks
    // anywhere in its path, and check it against its signed size and
    // SHA-256: the signature covers every byte the skill can put in front
    // of the agent or run (#613). The program's bytes are kept for the
    // registry's pin; a playbook without one pins nothing to run.
    let manifest = admission.manifest();
    let mut code = Vec::new();
    for entry in &manifest.files {
        let bytes = read_skill_file(dir, &entry.path, dir_name, "listed file")?;
        entry
            .check(&manifest.name, &bytes)
            .map_err(|err| Error::SkillLoad {
                skill: dir_name.to_owned(),
                reason: err.to_string(),
            })?;
        if manifest.entry_point() == Some(entry.path.as_str()) {
            code = bytes;
        }
    }

    let digest = manifest.digest().map_err(|err| Error::SkillLoad {
        skill: dir_name.to_owned(),
        reason: err.to_string(),
    })?;
    Ok((admission, code, digest))
}

/// Verifies `signed` under each root in turn, primary first: a skill admits
/// under whichever root signed it (the rotation grace window). A signature
/// that fails under one root moves on to the next; any other refusal
/// (unsigned, invalid, over-privileged) is final, whatever the root.
fn admit_under_roots(
    signed: &hatchling_skill::SignedManifest,
    roots: &TrustRoots,
    grants: &std::collections::HashSet<hatchling_core::CapabilityName>,
    dir_name: &str,
) -> Result<Admission, Error> {
    let refused = |reason: String| Error::SkillLoad {
        skill: dir_name.to_owned(),
        reason,
    };
    if roots.is_empty() {
        return Err(refused(String::from(
            "admission refused: no trust roots are configured",
        )));
    }
    for (role, root) in roots.keys() {
        let result = verify_admission(signed, &root, grants);
        if matches!(result, Err(hatchling_skill::Error::BadSignature { .. })) {
            continue;
        }
        if result.is_ok() {
            tracing::info!(skill = dir_name, root = role, "skill signature verified");
        }
        return result.map_err(|err| refused(format!("admission refused: {err}")));
    }
    Err(refused(String::from(
        "admission refused: signature verification failed under every trust root",
    )))
}

/// Checks that `dir` is a real directory owned by root or the broker's
/// uid and writable by no one else: the trust-roots file's rule, held for
/// the skills dir and each skill's own directory (#613). Anyone who could
/// write one could put files beside a signed skill's, or swap the skill.
///
/// # Errors
///
/// Returns why it can't be trusted.
fn closed_to_others(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let shown = dir.display();
    let meta =
        std::fs::symlink_metadata(dir).map_err(|err| format!("cannot stat {shown}: {err}"))?;
    if !meta.file_type().is_dir() {
        return Err(format!("{shown} is not a directory (symlinks are refused)"));
    }
    let euid = hatchling_sys::euid();
    if !owned_by_root_or(meta.uid(), euid) {
        return Err(format!(
            "{shown} must be owned by root or the broker's uid {euid}, not uid {}",
            meta.uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{shown} must not be group- or world-writable (mode {:o})",
            meta.mode() & 0o777
        ));
    }
    Ok(())
}

/// Whether a directory `owner` may hold skills for a broker running as
/// `euid`: root's, or the broker's own. A test can't make a directory of
/// another uid without root, so the rule is a function its tests reach.
const fn owned_by_root_or(owner: u32, euid: u32) -> bool {
    owner == 0 || owner == euid
}

/// Read `relative` inside `root` without following a symlink anywhere.
///
/// Every component of the path is checked, each intermediate directory
/// and the file itself. Checking only the final component let an entry
/// point like `sub/main.py`, with `sub` linked outside the package, read
/// an arbitrary file. (A swap between this check and the read needs
/// write access to the skills dir, which is the broker's own; the skill
/// cell mounts a private copy of the bytes read here either way, see
/// [`crate::skill_cell::VerifiedCode`].)
///
/// # Errors
///
/// Returns [`Error::SkillLoad`] for a symlinked component or an
/// unreadable path.
pub(crate) fn read_skill_file(
    root: &Path,
    relative: &str,
    skill: &str,
    what: &str,
) -> Result<Vec<u8>, Error> {
    refuse_symlink(root, skill, what)?;
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        path.push(component);
        refuse_symlink(&path, skill, what)?;
    }
    std::fs::read(&path).map_err(|err| Error::SkillLoad {
        skill: skill.to_owned(),
        reason: format!("cannot read {what} {}: {err}", path.display()),
    })
}

/// Refuse a path if it is a symlink.
///
/// Uses `symlink_metadata` (does not follow the link) at every level
/// the loader touches. A symlink anywhere in the skill's loaded files
/// is a directory-traversal vector; it is refused loudly, never
/// skipped silently.
fn refuse_symlink(path: &Path, dir_name: &str, what: &str) -> Result<(), Error> {
    let meta = std::fs::symlink_metadata(path).map_err(|err| Error::SkillLoad {
        skill: dir_name.to_owned(),
        reason: format!("cannot stat {what} {}: {err}", path.display()),
    })?;
    if meta.is_symlink() {
        return Err(Error::SkillLoad {
            skill: dir_name.to_owned(),
            reason: format!(
                "{what} {} is a symlink: symlinks are refused in skill directories",
                path.display()
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ed25519_dalek::{Signer, SigningKey};
    use hatchling_skill::{Manifest, SignedManifest};

    use super::*;
    use crate::trust_roots::TRUST_ROOTS_HEADER;

    const SECRET: [u8; 32] = [0x07; 32];

    /// A fresh skills dir unique to this test process and test.
    fn skills_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-skills-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The program every fixture's `main.py` holds, as its manifest lists.
    const CODE: &str = "print(1)";

    fn manifest(name: &str, caps: &[&str]) -> Manifest {
        manifest_with(name, caps, "main.py", CODE.as_bytes())
    }

    fn manifest_with(name: &str, caps: &[&str], path: &str, code: &[u8]) -> Manifest {
        Manifest::for_program(
            name,
            "1.0.0",
            "A test skill.",
            path,
            code,
            caps.iter().map(|cap| cap.parse().unwrap()).collect(),
        )
    }

    fn sign(manifest: Manifest) -> SignedManifest {
        let sig = SigningKey::from_bytes(&SECRET).sign(&manifest.canonical_bytes().unwrap());
        SignedManifest {
            manifest,
            signature: Some(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                sig.to_bytes(),
            )),
        }
    }

    /// Writes `<root>/<dir>/manifest.json` and, when `code` is given,
    /// the entry point `main.py`.
    fn write_skill(root: &Path, dir: &str, signed: &SignedManifest, code: Option<&str>) {
        let skill_dir = root.join(dir);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join(MANIFEST_FILE), signed.to_json().unwrap()).unwrap();
        if let Some(code) = code {
            std::fs::write(skill_dir.join("main.py"), code).unwrap();
        }
    }

    fn config(root: &Path) -> SkillConfig {
        SkillConfig {
            skills_dir: root.to_path_buf(),
            trust_roots_file: root.with_extension("roots"),
            enablements: root.with_extension("enabled"),
        }
    }

    /// Every skill found under `roots`, enabled as it is now.
    fn all_enabled(config: &SkillConfig, roots: &TrustRoots) -> Enabled {
        load_skills(config, roots, &Enabled::new())
            .found
            .into_iter()
            .map(|found| (found.admission.manifest().name.clone(), found.digest))
            .collect()
    }

    /// The loader under `roots`, every skill that verifies enabled.
    fn load_under(config: &SkillConfig, roots: &TrustRoots) -> LoadOutcome {
        load_skills(config, roots, &all_enabled(config, roots))
    }

    /// A v1 trust-roots file for these keys: primary first.
    fn roots_text(keys: &[[u8; 32]]) -> String {
        use std::fmt::Write as _;
        let mut text = format!("{TRUST_ROOTS_HEADER}\n");
        for (role, key) in ["primary", "previous"].iter().zip(keys) {
            let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key);
            writeln!(text, "{role} {key}").unwrap();
        }
        text.push_str("end\n");
        text
    }

    fn public(secret: &[u8; 32]) -> [u8; 32] {
        SigningKey::from_bytes(secret).verifying_key().to_bytes()
    }

    fn roots_of(keys: &[[u8; 32]]) -> TrustRoots {
        TrustRoots::parse(roots_text(keys).as_bytes()).unwrap()
    }

    /// The loader under the test key alone, every skill enabled.
    fn load(config: &SkillConfig) -> LoadOutcome {
        load_under(config, &roots_of(&[public(&SECRET)]))
    }

    fn loaded_names(loaded: &[LoadedSkill]) -> Vec<String> {
        let mut names: Vec<String> = loaded
            .iter()
            .map(|skill| skill.admission.manifest().name.clone())
            .collect();
        names.sort();
        names
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_directory_inside_the_entry_point_is_refused() {
        // `sub/main.py` where `sub` is a symlink out of the package: the
        // final component is a regular file, so a final-component-only
        // check followed the link. A real `sub/` directory still loads.
        let root = skills_dir("nested-link");
        let outside = std::env::temp_dir().join(format!(
            "hatchling-skills-{}-nested-link-target",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("main.py"), b"outside").unwrap();
        let linked = manifest_with("linked", &["fs.read"], "sub/main.py", b"outside");
        write_skill(&root, "linked", &sign(linked), None);
        std::os::unix::fs::symlink(&outside, root.join("linked").join("sub")).unwrap();
        let nested = manifest_with("nested", &["fs.read"], "sub/main.py", b"inside");
        write_skill(&root, "nested", &sign(nested), None);
        std::fs::create_dir_all(root.join("nested").join("sub")).unwrap();
        std::fs::write(root.join("nested").join("sub").join("main.py"), b"inside").unwrap();
        let LoadOutcome { loaded, .. } = load(&config(&root));
        assert_eq!(loaded_names(&loaded), vec![String::from("nested")]);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_skill_runs_only_enabled_at_its_digest_now() {
        let root = skills_dir("enabled");
        for name in ["web", "notes"] {
            write_skill(
                &root,
                name,
                &sign(manifest(name, &["net.egress"])),
                Some(CODE),
            );
        }
        let cfg = config(&root);
        let roots = roots_of(&[public(&SECRET)]);
        let all = all_enabled(&cfg, &roots);
        let web = all.get("web").unwrap().clone();
        let state = |outcome: &LoadOutcome, name: &str| {
            outcome
                .found
                .iter()
                .find(|found| found.admission.manifest().name == name)
                .map(|found| found.state)
        };
        // Nothing enabled: both found, both waiting, none runs.
        let outcome = load_skills(&cfg, &roots, &Enabled::new());
        assert!(outcome.loaded.is_empty() && outcome.registry.is_empty());
        assert!(outcome.refused.is_empty(), "{:?}", outcome.refused);
        assert_eq!(state(&outcome, "web"), Some(SkillState::Waiting));
        assert_eq!(state(&outcome, "notes"), Some(SkillState::Waiting));
        // One enabled at its digest: that one runs, granted what it declares.
        let outcome = load_skills(
            &cfg,
            &roots,
            &Enabled::from([(String::from("web"), web.clone())]),
        );
        assert_eq!(loaded_names(&outcome.loaded), vec![String::from("web")]);
        assert_eq!(state(&outcome, "web"), Some(SkillState::Enabled));
        assert_eq!(state(&outcome, "notes"), Some(SkillState::Waiting));
        let id = SkillId::new("web", "1.0.0");
        assert!(outcome.registry.get(&id).is_some());
        // Enabled at another digest: changed, and it doesn't run.
        let other = Enabled::from([(String::from("web"), "0".repeat(64))]);
        let outcome = load_skills(&cfg, &roots, &other);
        assert!(outcome.loaded.is_empty());
        assert_eq!(state(&outcome, "web"), Some(SkillState::Changed));
        // The skill re-signed with more: its digest moves, so the owner's
        // enabling no longer covers it.
        write_skill(
            &root,
            "web",
            &sign(manifest("web", &["net.egress", "fs.write"])),
            Some(CODE),
        );
        let outcome = load_skills(&cfg, &roots, &Enabled::from([(String::from("web"), web)]));
        assert!(outcome.loaded.is_empty());
        assert_eq!(state(&outcome, "web"), Some(SkillState::Changed));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_listing_shows_what_the_owner_is_enabling_sorted_by_name() {
        let root = skills_dir("listing");
        let mut playbook = manifest("notes", &[]);
        playbook.program = None;
        playbook.instructions = Some(String::from("main.py"));
        write_skill(&root, "z-notes", &sign(playbook), Some(CODE));
        write_skill(
            &root,
            "a-web",
            &sign(manifest("web", &["net.egress"])),
            Some(CODE),
        );
        let cfg = config(&root);
        let roots = roots_of(&[public(&SECRET)]);
        let set = SkillSet::build(&cfg, &roots, &Enabled::new());
        let names: Vec<&str> = set.listing().iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["notes", "web"]);
        let notes = set.listing().first().unwrap();
        assert_eq!(notes.instructions.as_deref(), Some("main.py"));
        assert_eq!(notes.program, None);
        assert!(notes.capabilities.is_empty());
        let web = set.listing().get(1).unwrap();
        assert_eq!(web.version, "1.0.0");
        assert_eq!(web.description, "A test skill.");
        assert_eq!(web.program.as_deref(), Some("main.py"));
        assert_eq!(web.capabilities, vec![String::from("net.egress")]);
        assert_eq!(
            web.files,
            vec![SkillFile {
                path: String::from("main.py"),
                bytes: u64::try_from(CODE.len()).unwrap(),
            }]
        );
        assert_eq!(web.signer.len(), 64);
        assert_eq!(&web.digest, all_enabled(&cfg, &roots).get("web").unwrap());
        assert_eq!(web.state, SkillState::Waiting);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_valid_skill_loads_with_its_code() {
        let root = skills_dir("valid");
        write_skill(
            &root,
            "web-dir",
            &sign(manifest("web", &["net.egress"])),
            Some(CODE),
        );
        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(&root));
        assert_eq!(loaded.len(), 1);
        let skill = loaded.first().unwrap();
        assert_eq!(skill.dir_name, "web-dir");
        assert_eq!(skill.code, CODE.as_bytes());
        let id = SkillId::new("web", "1.0.0");
        assert!(registry.verify_code(&id, &skill.code).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_skills_are_refused_and_the_rest_load() {
        let root = skills_dir("broken");
        let mut unsigned = sign(manifest("unsigned", &["fs.read"]));
        unsigned.signature = None;
        write_skill(&root, "a", &unsigned, Some(CODE));
        write_skill(&root, "b", &sign(manifest("codeless", &["fs.read"])), None);
        write_skill(
            &root,
            "c",
            &sign(manifest("good", &["fs.read"])),
            Some(CODE),
        );
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("d").join(MANIFEST_FILE), b"{not json").unwrap();
        // A plain file beside the skill dirs is not a skill.
        std::fs::write(root.join("README"), b"not a skill").unwrap();
        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(&root));
        assert_eq!(loaded_names(&loaded), vec![String::from("good")]);
        assert_eq!(registry.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn each_broken_skill_is_refused_with_its_reason_and_the_rest_load() {
        let root = skills_dir("refusals");
        // Bad signature: signed, then a signed field changed.
        let mut tampered = sign(manifest("tampered", &["fs.read"]));
        tampered.manifest.version = String::from("9.9.9");
        write_skill(&root, "tampered", &tampered, Some(CODE));
        // Unreadable code: the entry point is a directory.
        write_skill(
            &root,
            "unreadable",
            &sign(manifest("unreadable", &["fs.read"])),
            None,
        );
        std::fs::create_dir_all(root.join("unreadable").join("main.py")).unwrap();
        // A malformed capability name never parses.
        std::fs::create_dir_all(root.join("malformed")).unwrap();
        let malformed =
            String::from_utf8(sign(manifest("malformed", &["fs.read"])).to_json().unwrap())
                .unwrap()
                .replace("\"fs.read\"", "\"fs read\"");
        std::fs::write(root.join("malformed").join(MANIFEST_FILE), malformed).unwrap();
        write_skill(
            &root,
            "good",
            &sign(manifest("good", &["fs.read"])),
            Some(CODE),
        );
        let outcome = load(&config(&root));
        assert_eq!(loaded_names(&outcome.loaded), vec![String::from("good")]);
        assert_eq!(outcome.registry.len(), 1);
        let mut refused = outcome.refused;
        refused.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
        let expected = [
            ("malformed", "capability name"),
            ("tampered", "signature verification failed"),
            ("unreadable", "cannot read listed file"),
        ];
        assert_eq!(refused.len(), expected.len(), "{refused:?}");
        for (refusal, (dir, why)) in refused.iter().zip(expected) {
            assert_eq!(refusal.dir_name, dir);
            assert!(refusal.reason.contains(why), "{refusal:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_disabled_config_refuses_nothing_but_a_missing_dir_is_refused() {
        let outcome = load(&SkillConfig::disabled());
        assert!(outcome.registry.is_empty());
        assert!(outcome.loaded.is_empty());
        assert!(outcome.refused.is_empty(), "{:?}", outcome.refused);
        let missing = skills_dir("missing-dir-refusal");
        let _ = std::fs::remove_dir_all(&missing);
        let outcome = load(&config(&missing));
        assert_eq!(outcome.refused.len(), 1);
        let refusal = outcome.refused.first().unwrap();
        assert_eq!(refusal.dir_name, missing.display().to_string());
        assert!(
            refusal.reason.contains("skills dir unreadable"),
            "{refusal:?}"
        );
    }

    #[test]
    fn skills_load_holds_the_admitted_registry() {
        let root = skills_dir("skills-load");
        write_skill(
            &root,
            "web",
            &sign(manifest("web", &["net.egress"])),
            Some(CODE),
        );
        let cfg = config(&root);
        std::fs::write(&cfg.trust_roots_file, roots_text(&[public(&SECRET)])).unwrap();
        std::fs::set_permissions(
            &cfg.trust_roots_file,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
        )
        .unwrap();
        let skills = Skills::load(&cfg).unwrap();
        // Found, and waiting: nothing runs until the owner enables it.
        let waiting = skills.snapshot();
        assert!(waiting.registry().is_empty());
        let listed = waiting.listing().first().unwrap();
        assert_eq!(
            (listed.name.as_str(), listed.state),
            ("web", SkillState::Waiting)
        );
        assert_eq!(listed.capabilities, vec![String::from("net.egress")]);
        skills.set_enabled(Enabled::from([(
            String::from("web"),
            listed.digest.clone(),
        )]));
        let set = skills.snapshot();
        assert_eq!(set.listing().first().unwrap().state, SkillState::Enabled);
        assert_eq!(set.registry().len(), 1);
        assert!(set.registry().get(&SkillId::new("web", "1.0.0")).is_some());
        assert_eq!(
            set.packages.get(&SkillId::new("web", "1.0.0")),
            Some(&root.join("web"))
        );
        assert!(Skills::default().snapshot().registry().is_empty());
        // Disabled: it stops at once, and is listed waiting again.
        skills.set_enabled(Enabled::new());
        let set = skills.snapshot();
        assert!(set.registry().is_empty());
        assert_eq!(set.listing().first().unwrap().state, SkillState::Waiting);
        // A reload under new roots keeps what the owner enabled.
        let web = || Enabled::from([(String::from("web"), listed.digest.clone())]);
        skills.set_enabled(web());
        std::fs::write(
            &cfg.trust_roots_file,
            roots_text(&[public(&SECRET), public(&[0x3c; 32])]),
        )
        .unwrap();
        assert!(matches!(skills.reload(), Reload::Applied { .. }));
        assert_eq!(skills.snapshot().registry().len(), 1);
        // A bad roots file is kept out, and the owner can still disable.
        std::fs::write(
            &cfg.trust_roots_file,
            b"garbage
",
        )
        .unwrap();
        assert!(matches!(skills.reload(), Reload::Rejected { .. }));
        skills.set_enabled(Enabled::new());
        assert!(skills.snapshot().registry().is_empty());
        skills.set_enabled(web());
        assert_eq!(skills.snapshot().registry().len(), 1);
        let _ = std::fs::remove_file(&cfg.trust_roots_file);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_skill_with_the_same_id_and_other_code_is_refused() {
        // The same signed manifest twice: beside its own program it loads;
        // beside other code it's refused, since the signature lists the
        // program's hash (#613), not a pin taken on first sight.
        let root = skills_dir("drift");
        let signed = sign(manifest("dup", &["fs.read"]));
        write_skill(&root, "one", &signed, Some(CODE));
        write_skill(&root, "two", &signed, Some("print(2)"));
        let LoadOutcome {
            registry,
            loaded,
            refused,
            ..
        } = load(&config(&root));
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.first().unwrap().dir_name, "one");
        assert_eq!(registry.len(), 1);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(
            refused
                .first()
                .unwrap()
                .reason
                .contains("isn't the file its signed manifest lists"),
            "{refused:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_listed_file_is_checked_and_nothing_unlisted_matters() {
        // A playbook with a program: every listed file is checked against
        // its signed size and hash. A changed playbook, or a missing one,
        // is refused; a file the manifest doesn't list is never read.
        let root = skills_dir("files");
        let playbook: &[u8] = b"# Steps\n";
        let both = |name: &str| {
            let mut m = manifest(name, &["fs.read"]);
            m.instructions = Some(String::from("SKILL.md"));
            m.files
                .push(hatchling_skill::FileEntry::of("SKILL.md", playbook));
            sign(m)
        };
        let other: &[u8] = b"# Other steps\n";
        for (dir, content) in [
            ("good", Some(playbook)),
            ("changed", Some(other)),
            ("missing", None),
        ] {
            write_skill(&root, dir, &both(dir), Some(CODE));
            if let Some(content) = content {
                std::fs::write(root.join(dir).join("SKILL.md"), content).unwrap();
            }
        }
        std::fs::write(root.join("good").join("unlisted.sh"), b"rm -rf ~").unwrap();
        let mut outcome = load(&config(&root));
        assert_eq!(loaded_names(&outcome.loaded), vec![String::from("good")]);
        outcome.refused.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
        let reasons: Vec<(&str, &str)> = outcome
            .refused
            .iter()
            .map(|r| (r.dir_name.as_str(), r.reason.as_str()))
            .collect();
        assert!(
            matches!(
                reasons.as_slice(),
                [("changed", changed), ("missing", missing)]
                    if changed.contains("'SKILL.md' isn't the file")
                        && missing.contains("listed file")
                        && missing.contains("SKILL.md")
            ),
            "{reasons:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_playbook_without_a_program_loads_and_pins_nothing_to_run() {
        let root = skills_dir("playbook");
        let mut m = manifest("notes", &[]);
        m.program = None;
        m.instructions = Some(String::from("main.py"));
        write_skill(&root, "notes", &sign(m), Some(CODE));
        let LoadOutcome {
            loaded, refused, ..
        } = load(&config(&root));
        assert!(refused.is_empty(), "{refused:?}");
        let skill = loaded.first().unwrap();
        assert!(skill.code.is_empty());
        assert!(skill.admission.manifest().capabilities().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn a_skill_the_signer_signed_loads_under_its_key_alone() {
        // The author's side and the broker's side agree: what
        // `hatchling skill sign` writes, the loader admits under the
        // signer's key, and under no other.
        let root = skills_dir("signer");
        let skill = root.join("notes");
        std::fs::create_dir_all(skill.join("scripts")).unwrap();
        std::fs::write(skill.join("SKILL.md"), b"# Steps\n").unwrap();
        std::fs::write(skill.join("scripts").join("tidy.py"), b"print(1)\n").unwrap();
        std::fs::write(
            skill.join(MANIFEST_FILE),
            br#"{"name":"notes","version":"1","description":"Use when asked.","instructions":"SKILL.md"}"#,
        )
        .unwrap();
        let keys = std::env::temp_dir().join(format!(
            "hatchling-skills-{}-signer-keys",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&keys);
        std::fs::create_dir_all(&keys).unwrap();
        let public = hatchling_skill::author::new_key(&keys.join("k")).unwrap();
        hatchling_skill::author::sign(&skill, &keys.join("k")).unwrap();
        let signer: [u8; 32] =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &public.base64)
                .unwrap()
                .try_into()
                .unwrap();
        let outcome = load_under(&config(&root), &roots_of(&[signer]));
        assert!(outcome.refused.is_empty(), "{:?}", outcome.refused);
        assert_eq!(loaded_names(&outcome.loaded), vec![String::from("notes")]);
        assert_eq!(
            outcome
                .loaded
                .first()
                .unwrap()
                .admission
                .signer_fingerprint(),
            public.fingerprint
        );
        assert!(load(&config(&root)).loaded.is_empty());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&keys);
    }

    #[test]
    fn a_format_v1_manifest_is_refused_saying_to_sign_it_again() {
        let root = skills_dir("v1");
        std::fs::create_dir_all(root.join("old")).unwrap();
        std::fs::write(
            root.join("old").join(MANIFEST_FILE),
            br#"{"name":"old","version":"1.0.0","capabilities":[],"entry_point":"main.py","signature":"AAAA"}"#,
        )
        .unwrap();
        let outcome = load(&config(&root));
        let reason = &outcome.refused.first().unwrap().reason;
        assert!(
            reason.contains("format v1") && reason.contains("format 2"),
            "{reason}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_skill_dir_is_root_s_or_the_broker_s_and_no_one_else_s() {
        // As the broker's uid, as root, and another uid.
        assert!(owned_by_root_or(1000, 1000));
        assert!(owned_by_root_or(0, 1000));
        assert!(owned_by_root_or(0, 0));
        assert!(!owned_by_root_or(1001, 1000));
        assert!(!owned_by_root_or(1000, 0));
    }

    #[test]
    #[cfg(unix)]
    fn a_skills_dir_or_skill_dir_others_can_write_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let root = skills_dir("writable");
        mode(&root, 0o755);
        for dir in ["open", "shut"] {
            write_skill(&root, dir, &sign(manifest(dir, &["fs.read"])), Some(CODE));
            mode(&root.join(dir), 0o755);
        }
        // A skill's own directory others can write: that skill alone.
        for writable in [0o775, 0o757] {
            mode(&root.join("open"), writable);
            let outcome = load(&config(&root));
            assert_eq!(loaded_names(&outcome.loaded), vec![String::from("shut")]);
            let reason = &outcome.refused.first().unwrap().reason;
            assert!(
                reason.contains("must not be group- or world-writable"),
                "{reason}"
            );
        }
        mode(&root.join("open"), 0o755);
        assert_eq!(load(&config(&root)).loaded.len(), 2);
        // The skills dir itself: every skill.
        for writable in [0o775, 0o757] {
            mode(&root, writable);
            let outcome = load(&config(&root));
            assert!(outcome.loaded.is_empty());
            let reason = &outcome.refused.first().unwrap().reason;
            assert!(reason.contains("skills dir refused"), "{reason}");
        }
        mode(&root, 0o755);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unreadable_dir_or_no_trust_roots_loads_nothing() {
        let missing = skills_dir("missing");
        let _ = std::fs::remove_dir_all(&missing);
        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(&missing));
        assert!(loaded.is_empty() && registry.is_empty());

        let root = skills_dir("no-roots");
        write_skill(
            &root,
            "web",
            &sign(manifest("web", &["net.egress"])),
            Some(CODE),
        );
        let outcome = load_under(&config(&root), &roots_of(&[]));
        assert!(outcome.loaded.is_empty() && outcome.registry.is_empty());
        let reason = &outcome.refused.first().unwrap().reason;
        assert!(reason.contains("no trust roots are configured"), "{reason}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_skill_admits_under_whichever_root_signed_it() {
        // Signed by SECRET. Admitted with SECRET as primary or as previous
        // (the rotation grace window); refused once SECRET is gone.
        let root = skills_dir("grace");
        write_skill(
            &root,
            "web",
            &sign(manifest("web", &["net.egress"])),
            Some(CODE),
        );
        let cfg = config(&root);
        let other = public(&[0x3c; 32]);
        for roots in [
            roots_of(&[public(&SECRET)]),
            roots_of(&[other, public(&SECRET)]),
        ] {
            assert_eq!(
                loaded_names(&load_under(&cfg, &roots).loaded),
                vec![String::from("web")]
            );
        }
        let outcome = load_under(&cfg, &roots_of(&[other]));
        assert!(outcome.loaded.is_empty());
        let reason = &outcome.refused.first().unwrap().reason;
        assert!(reason.contains("under every trust root"), "{reason}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_skill_files_are_refused() {
        // A symlink for the manifest, the code file, or the skill dir
        // itself is refused loudly — never followed. Symlink targets
        // live outside the skills root so they aren't loaded as skills.
        let root = skills_dir("symlink");
        let outside = std::env::temp_dir().join(format!(
            "hatchling-skills-{}-symlink-targets",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();
        let signed = sign(manifest("link-skill", &["fs.read"]));

        // Symlinked manifest.
        let dir1 = root.join("one");
        std::fs::create_dir_all(&dir1).unwrap();
        let real_manifest = outside.join("real-manifest.json");
        std::fs::write(&real_manifest, signed.to_json().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real_manifest, dir1.join(MANIFEST_FILE)).unwrap();
        std::fs::write(dir1.join("main.py"), b"code").unwrap();

        // Symlinked code file.
        let dir2 = root.join("two");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join(MANIFEST_FILE), signed.to_json().unwrap()).unwrap();
        let real_code = outside.join("real-code.py");
        std::fs::write(&real_code, b"code").unwrap();
        std::os::unix::fs::symlink(&real_code, dir2.join("main.py")).unwrap();

        // Symlinked skill dir.
        let real_dir = outside.join("real-dir");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(real_dir.join(MANIFEST_FILE), signed.to_json().unwrap()).unwrap();
        std::fs::write(real_dir.join("main.py"), b"code").unwrap();
        std::os::unix::fs::symlink(&real_dir, root.join("three")).unwrap();

        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(&root));
        assert!(loaded.is_empty(), "symlinked skills must be refused");
        assert!(registry.is_empty());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
