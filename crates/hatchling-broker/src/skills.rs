//! Skill loading: from disk to registry.
//!
//! At broker startup, the broker reads each skill subdirectory under
//! the configured skills dir, verifies the signed manifest against the
//! trust root, pins the code hash ( registry), and
//! registers the skill. Any failure — bad signature, over-privileged
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

use hatchling_skill::{Admission, CodeHash, SkillId, SkillRegistry, verify_admission};

use crate::skill_run::{SkillRunError, SkillRuntime, run_skill};
use crate::trust_roots::{TrustRoots, read_trust_roots_file};
use crate::{Error, SkillConfig};

/// The manifest filename within each skill directory.
const MANIFEST_FILE: &str = "manifest.json";

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

/// What [`load_skills`] found.
pub struct LoadOutcome {
    /// The admitted skills, code hashes pinned.
    pub registry: SkillRegistry,
    /// The admitted skills as loaded.
    pub loaded: Vec<LoadedSkill>,
    /// Every refusal, in the order met.
    pub refused: Vec<Refusal>,
}

/// Load all skills from the configured directory into the registry.
///
/// Each subdirectory must contain `manifest.json` (the signed manifest,
/// strict JSON) and the skill code file referenced by the manifest's
/// `entry_point`. The manifest is verified via [`verify_admission`]
/// against the trust root and user grants; the code is read and its
/// hash pinned at registration.
///
/// Every signature is verified under `roots`, primary first; a skill
/// admits under whichever root signed it. A skill that fails to load is
/// logged loudly and refused, not retried. The broker continues with the
/// remaining skills. A disabled config (no skills dir) loads nothing and
/// touches no filesystem.
pub fn load_skills(config: &SkillConfig, roots: &TrustRoots) -> LoadOutcome {
    let mut outcome = LoadOutcome {
        registry: SkillRegistry::new(),
        loaded: Vec::new(),
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
        match admit(&mut outcome.registry, &path, &dir_name, config, roots) {
            Ok((id, admission, code)) => {
                tracing::info!(
                    skill = %id,
                    dir = ?dir_name,
                    "skill admitted and registered"
                );
                outcome.loaded.push(LoadedSkill {
                    dir_name,
                    admission,
                    code,
                });
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

/// Load, verify, and register one skill directory.
fn admit(
    registry: &mut SkillRegistry,
    dir: &Path,
    dir_name: &str,
    config: &SkillConfig,
    roots: &TrustRoots,
) -> Result<(SkillId, Admission, Vec<u8>), Error> {
    let (admission, code) = load_one(dir, dir_name, config, roots)?;
    let id = registry
        .register(admission.clone(), &code)
        .map_err(|err| Error::SkillLoad {
            skill: dir_name.to_owned(),
            reason: format!("registration refused: {err}"),
        })?;
    Ok((id, admission, code))
}

/// One immutable snapshot of the admitted skills: what a session checks a
/// call against and runs it from. A trust-root reload replaces it whole;
/// it is never edited in place.
#[derive(Debug, Default)]
pub struct SkillSet {
    registry: SkillRegistry,
    packages: HashMap<SkillId, PathBuf>,
}

impl SkillSet {
    /// Loads every skill under the configured dir against `roots`.
    fn build(config: &SkillConfig, roots: &TrustRoots) -> Self {
        let outcome = load_skills(config, roots);
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
        }
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
        /// Skills refused because their code no longer matches the hash
        /// pinned when they were first admitted (also in `dropped` when
        /// the old set held them).
        refused_drift: Vec<SkillId>,
    },
}

/// What the watcher last acted on.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Applied(Vec<u8>),
    Failed(String),
}

/// Where the trust roots come from, what the watcher last saw there, and
/// the code hash each skill id was first admitted with. The pins last
/// until a restart: a reload never re-pins an id.
#[derive(Debug)]
struct RootsSource {
    config: SkillConfig,
    seen: Mutex<Seen>,
    pins: Mutex<HashMap<SkillId, CodeHash>>,
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
        let mut set = SkillSet::build(config, &roots);
        // First sight of every id: this pins them all and refuses none.
        let mut pins = HashMap::new();
        refuse_drift(&mut pins, &mut set);
        Ok(Self {
            current: RwLock::new(Arc::new(set)),
            runtime: None,
            source: Some(RootsSource {
                config: config.clone(),
                seen: Mutex::new(Seen::Applied(bytes)),
                pins: Mutex::new(pins),
            }),
            harnesses: crate::harness::Harnesses::default(),
        })
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
        run_skill(runtime, skill, package, input)
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
        let mut new = SkillSet::build(&source.config, &roots);
        let refused_drift = refuse_drift(&mut lock(&source.pins), &mut new);
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
        let replacement = Arc::new(new);
        match self.current.write() {
            Ok(mut guard) => *guard = replacement,
            Err(poisoned) => *poisoned.into_inner() = replacement,
        }
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
        Reload::Applied {
            added,
            dropped,
            refused_drift,
        }
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

/// Pins each id in `new` to its code hash the first time it is seen, and
/// removes from `new` every id whose code differs from its pin: the same
/// `name@version` with different bytes is drift, not an update (an update
/// is a new version, which pins normally). The pin is the first admitted
/// hash, not whatever the last set held, so a drifted skill cannot come
/// back a reload later. Refused until a restart; each refusal is logged.
fn refuse_drift(pins: &mut HashMap<SkillId, CodeHash>, new: &mut SkillSet) -> Vec<SkillId> {
    let drifted: Vec<SkillId> = new
        .ids()
        .into_iter()
        .filter(|id| {
            new.registry.get(id).is_some_and(|skill| {
                *pins.entry(id.clone()).or_insert_with(|| *skill.code_hash()) != *skill.code_hash()
            })
        })
        .collect();
    for id in &drifted {
        new.registry.unregister(id);
        new.packages.remove(id);
        tracing::error!(
            skill = %id,
            "skill code changed under its pinned id: refused until the broker restarts \
             (ship a new version to update a skill)"
        );
    }
    drifted
}

/// Load and verify a single skill directory.
fn load_one(
    dir: &Path,
    dir_name: &str,
    config: &SkillConfig,
    roots: &TrustRoots,
) -> Result<(Admission, Vec<u8>), Error> {
    // Refuse symlinked skill directories: a symlink at the root would
    // let the loader read outside the skills dir.
    refuse_symlink(dir, dir_name, "skill directory")?;

    // Read the signed manifest (refusing symlinks).
    let manifest_path = dir.join(MANIFEST_FILE);
    refuse_symlink(&manifest_path, dir_name, "manifest")?;
    let manifest_bytes = std::fs::read(&manifest_path).map_err(|err| Error::SkillLoad {
        skill: dir_name.to_owned(),
        reason: format!("cannot read {}: {err}", manifest_path.display()),
    })?;

    // Parse the signed manifest (strict: unknown/duplicate fields refused).
    let signed: hatchling_skill::SignedManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|err| Error::SkillLoad {
            skill: dir_name.to_owned(),
            reason: format!("manifest JSON invalid: {err}"),
        })?;

    // The grants for THIS skill only, keyed by the manifest's name. The
    // name is read before verification, but `verify_admission` checks
    // the signature over it, so a forged name cannot pick up another
    // skill's grants. (Pooling every skill's grants into one set, as an
    // earlier draft did, admitted a skill with capabilities the user
    // granted only to a different skill.)
    let user_grants = config
        .grants
        .get(&signed.manifest.name)
        .cloned()
        .unwrap_or_default();
    let admission = admit_under_roots(&signed, roots, &user_grants, dir_name)?;

    // Read the code file (entry_point, validated as a safe relative path
    // by Manifest::validate during verify_admission). Refuse symlinks:
    // a symlinked entry point would let the loader read outside the
    // skill directory.
    let code = read_skill_file(
        dir,
        &admission.manifest().entry_point,
        dir_name,
        "code file",
    )?;

    Ok((admission, code))
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
    use std::collections::HashMap;
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

    fn manifest(name: &str, caps: &[&str]) -> Manifest {
        Manifest {
            name: name.to_owned(),
            version: String::from("1.0.0"),
            capabilities: caps.iter().map(|cap| cap.parse().unwrap()).collect(),
            entry_point: String::from("main.py"),
        }
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

    fn config(root: &Path, grants: &[(&str, &[&str])]) -> SkillConfig {
        SkillConfig {
            skills_dir: root.to_path_buf(),
            trust_roots_file: root.with_extension("roots"),
            grants: grants
                .iter()
                .map(|(skill, caps)| {
                    (
                        (*skill).to_owned(),
                        caps.iter().map(|cap| cap.parse().unwrap()).collect(),
                    )
                })
                .collect::<HashMap<_, _>>(),
        }
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

    /// The loader under the test key alone.
    fn load(config: &SkillConfig) -> LoadOutcome {
        load_skills(config, &roots_of(&[public(&SECRET)]))
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
        let mut linked = manifest("linked", &["fs.read"]);
        linked.entry_point = String::from("sub/main.py");
        write_skill(&root, "linked", &sign(linked), None);
        std::os::unix::fs::symlink(&outside, root.join("linked").join("sub")).unwrap();
        let mut nested = manifest("nested", &["fs.read"]);
        nested.entry_point = String::from("sub/main.py");
        write_skill(&root, "nested", &sign(nested), None);
        std::fs::create_dir_all(root.join("nested").join("sub")).unwrap();
        std::fs::write(root.join("nested").join("sub").join("main.py"), b"inside").unwrap();
        let LoadOutcome { loaded, .. } = load(&config(
            &root,
            &[("linked", &["fs.read"]), ("nested", &["fs.read"])],
        ));
        assert_eq!(loaded_names(&loaded), vec![String::from("nested")]);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn grants_are_per_skill_not_pooled() {
        // `notes` was granted only fs.write, yet declares net.egress too,
        // which the user granted only to `web`. Pooled grants admitted it.
        let root = skills_dir("per-skill");
        write_skill(
            &root,
            "notes",
            &sign(manifest("notes", &["fs.write", "net.egress"])),
            Some("n"),
        );
        write_skill(
            &root,
            "web",
            &sign(manifest("web", &["net.egress"])),
            Some("w"),
        );
        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(
            &root,
            &[("notes", &["fs.write"]), ("web", &["net.egress"])],
        ));
        assert_eq!(loaded_names(&loaded), vec![String::from("web")]);
        assert_eq!(registry.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_valid_skill_loads_with_its_code() {
        let root = skills_dir("valid");
        write_skill(
            &root,
            "web-dir",
            &sign(manifest("web", &["net.egress"])),
            Some("print('web')"),
        );
        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(&root, &[("web", &["net.egress"])]));
        assert_eq!(loaded.len(), 1);
        let skill = loaded.first().unwrap();
        assert_eq!(skill.dir_name, "web-dir");
        assert_eq!(skill.code, b"print('web')");
        let id = SkillId::new("web", "1.0.0");
        assert!(registry.verify_code(&id, &skill.code).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_skills_are_refused_and_the_rest_load() {
        let root = skills_dir("broken");
        let grants: &[(&str, &[&str])] = &[
            ("unsigned", &["fs.read"]),
            ("codeless", &["fs.read"]),
            ("good", &["fs.read"]),
        ];
        let mut unsigned = sign(manifest("unsigned", &["fs.read"]));
        unsigned.signature = None;
        write_skill(&root, "a", &unsigned, Some("u"));
        write_skill(&root, "b", &sign(manifest("codeless", &["fs.read"])), None);
        write_skill(&root, "c", &sign(manifest("good", &["fs.read"])), Some("g"));
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("d").join(MANIFEST_FILE), b"{not json").unwrap();
        // A plain file beside the skill dirs is not a skill.
        std::fs::write(root.join("README"), b"not a skill").unwrap();
        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(&root, grants));
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
        write_skill(&root, "tampered", &tampered, Some("t"));
        // Unreadable code: the entry point is a directory.
        write_skill(
            &root,
            "unreadable",
            &sign(manifest("unreadable", &["fs.read"])),
            None,
        );
        std::fs::create_dir_all(root.join("unreadable").join("main.py")).unwrap();
        // Over-privileged: declares more than it was granted.
        write_skill(
            &root,
            "greedy",
            &sign(manifest("greedy", &["fs.read", "net.egress"])),
            Some("g"),
        );
        // A malformed capability name never parses.
        std::fs::create_dir_all(root.join("malformed")).unwrap();
        std::fs::write(
            root.join("malformed").join(MANIFEST_FILE),
            br#"{"name":"malformed","version":"1.0.0","capabilities":["fs read"],"entry_point":"main.py","signature":null}"#,
        )
        .unwrap();
        write_skill(
            &root,
            "good",
            &sign(manifest("good", &["fs.read"])),
            Some("ok"),
        );
        let grants: &[(&str, &[&str])] = &[
            ("tampered", &["fs.read"]),
            ("unreadable", &["fs.read"]),
            ("greedy", &["fs.read"]),
            ("malformed", &["fs.read"]),
            ("good", &["fs.read"]),
        ];
        let outcome = load(&config(&root, grants));
        assert_eq!(loaded_names(&outcome.loaded), vec![String::from("good")]);
        assert_eq!(outcome.registry.len(), 1);
        let mut refused = outcome.refused;
        refused.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
        let expected = [
            ("greedy", "beyond user grants: net.egress"),
            ("malformed", "capability name"),
            ("tampered", "signature verification failed"),
            ("unreadable", "cannot read code file"),
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
        let outcome = load(&config(&missing, &[]));
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
            Some("w"),
        );
        let cfg = config(&root, &[("web", &["net.egress"])]);
        std::fs::write(&cfg.trust_roots_file, roots_text(&[public(&SECRET)])).unwrap();
        std::fs::set_permissions(
            &cfg.trust_roots_file,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
        )
        .unwrap();
        let skills = Skills::load(&cfg).unwrap();
        let set = skills.snapshot();
        assert_eq!(set.registry().len(), 1);
        assert!(set.registry().get(&SkillId::new("web", "1.0.0")).is_some());
        assert_eq!(
            set.packages.get(&SkillId::new("web", "1.0.0")),
            Some(&root.join("web"))
        );
        assert!(Skills::default().snapshot().registry().is_empty());
        let _ = std::fs::remove_file(&cfg.trust_roots_file);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_skill_without_grants_is_refused() {
        let root = skills_dir("no-grants");
        write_skill(
            &root,
            "web",
            &sign(manifest("web", &["net.egress"])),
            Some("w"),
        );
        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(&root, &[]));
        assert!(loaded.is_empty());
        assert!(registry.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_skill_with_the_same_id_and_other_code_is_refused() {
        let root = skills_dir("drift");
        let signed = sign(manifest("dup", &["fs.read"]));
        write_skill(&root, "one", &signed, Some("first"));
        write_skill(&root, "two", &signed, Some("second"));
        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(&root, &[("dup", &["fs.read"])]));
        assert_eq!(loaded.len(), 1);
        assert_eq!(registry.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unreadable_dir_or_no_trust_roots_loads_nothing() {
        let missing = skills_dir("missing");
        let _ = std::fs::remove_dir_all(&missing);
        let LoadOutcome {
            registry, loaded, ..
        } = load(&config(&missing, &[("web", &["net.egress"])]));
        assert!(loaded.is_empty() && registry.is_empty());

        let root = skills_dir("no-roots");
        write_skill(
            &root,
            "web",
            &sign(manifest("web", &["net.egress"])),
            Some("w"),
        );
        let outcome = load_skills(&config(&root, &[("web", &["net.egress"])]), &roots_of(&[]));
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
            Some("w"),
        );
        let cfg = config(&root, &[("web", &["net.egress"])]);
        let other = public(&[0x3c; 32]);
        for roots in [
            roots_of(&[public(&SECRET)]),
            roots_of(&[other, public(&SECRET)]),
        ] {
            assert_eq!(
                loaded_names(&load_skills(&cfg, &roots).loaded),
                vec![String::from("web")]
            );
        }
        let outcome = load_skills(&cfg, &roots_of(&[other]));
        assert!(outcome.loaded.is_empty());
        let reason = &outcome.refused.first().unwrap().reason;
        assert!(reason.contains("under every trust root"), "{reason}");
        // A refusal that is not a signature failure is final: an
        // over-privileged manifest is refused even though the primary
        // root would verify it.
        let greedy = config(&root, &[("web", &[])]);
        let outcome = load_skills(&greedy, &roots_of(&[public(&SECRET), other]));
        let reason = &outcome.refused.first().unwrap().reason;
        assert!(reason.contains("beyond user grants"), "{reason}");
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
        } = load(&config(&root, &[("link-skill", &["fs.read"])]));
        assert!(loaded.is_empty(), "symlinked skills must be refused");
        assert!(registry.is_empty());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
