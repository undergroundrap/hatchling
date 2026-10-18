//! Per-skill cells: dedicated isolation for each admitted skill.
//!
//! Every skill runs in its own broker-spawned cell, never mixed with
//! the agent's cell or another skill's cell. The skill cell has:
//!
//! - **Minimal capability profile**: only the declared capabilities
//!   from the manifest, enforced at call time by the broker.
//! - **Verified, read-only code**: the cell mounts a private copy of
//!   exactly the bytes that were hashed ([`VerifiedCode`]), read-only
//!   at `/skill`, and runs the copied entry point as its program (the
//!   runtime model: a skill's entry point is an executable). Not the live skill directory: a read-only bind blocks
//!   writes through the mount, but a host-side write after the hash is
//!   visible in the cell.
//! - **Explicit credential injection**: the skill receives only the
//!   vault credentials its manifest declares, via the broker-mediated
//!   path. No ambient vault access.
//!
//! The cell is built on the same [`crate::cell`] infrastructure as the
//! agent cell: user namespaces, mount plan, seccomp. The difference is
//! the mount plan (skill code instead of agent binary) and the
//! capability profile.

use std::path::{Path, PathBuf};

use hatchling_core::LogLevel;
use hatchling_skill::RegisteredSkill;

use crate::{CellConfig, CellHandle, Error};

/// Configuration for a skill's dedicated cell.
#[derive(Debug, Clone)]
pub struct SkillCellConfig {
    /// The registered skill (admission + pinned code hash).
    pub skill: RegisteredSkill,
    /// Host path to the skill's code directory. Bind-mounted read-only
    /// at `/skill` in the cell.
    pub code_dir: PathBuf,
    /// The base [`CellConfig`] fields: uid mapping, socket dir, etc.
    /// The skill cell uses the same isolation primitives as the agent
    /// cell, but with a skill-specific mount plan.
    pub cell: CellConfig,
}

impl SkillCellConfig {
    /// Build a skill cell config from a registered skill and the
    /// broker's cell parameters.
    ///
    /// The `code_dir` is the skill's directory on the host (containing
    /// the code file). It is validated to be absolute. It is never
    /// mounted itself: [`spawn_skill_cell`] reads and verifies the entry
    /// point from it and mounts a private copy (see [`VerifiedCode`]).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] if `code_dir` is not absolute.
    pub fn new(skill: RegisteredSkill, code_dir: PathBuf, cell: CellConfig) -> Result<Self, Error> {
        if !code_dir.is_absolute() {
            return Err(Error::InvalidConfig {
                key: "skill code dir",
                reason: format!(
                    "skill code dir must be an absolute path: {}",
                    code_dir.display()
                ),
            });
        }
        Ok(Self {
            skill,
            code_dir,
            cell,
        })
    }

    /// The skill's registry id.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn skill_id(&self) -> &hatchling_skill::SkillId {
        self.skill.id()
    }
}

/// A private, read-only copy of a skill's verified entry point: the
/// directory a skill cell mounts at `/skill`.
///
/// Mounting the live skill directory would not bind what was hashed. A
/// read-only bind mount blocks writes *through* the mount, but a write to
/// the host path after the hash is visible in the cell (measured on
/// the cell read the swapped file). The copy holds exactly the
/// hashed bytes, in a fresh directory this broker created (an existing
/// path at that name is refused, never reused), and is removed on drop,
/// which [`SkillCellHandle`] orders after the cell has exited.
#[derive(Debug)]
pub struct VerifiedCode {
    dir: PathBuf,
}

impl VerifiedCode {
    /// Write `code` at `entry_point` (already validated as a plain
    /// relative path) inside a fresh directory. A harness uses it
    /// too: its pinned binary is run only from such a copy.
    pub(crate) fn write(skill: &str, entry_point: &str, code: &[u8]) -> Result<Self, Error> {
        use std::io::Write as _;
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
        let fail = |what: &str, path: &Path, err: std::io::Error| Error::SkillLoad {
            skill: skill.to_owned(),
            reason: format!("cannot {what} {}: {err}", path.display()),
        };
        // A name no one can guess, made exclusively: another local user
        // can't pre-create it to block the skill.
        let name =
            crate::approval::random_hex(&format!("hatchling-skill-code-{}-", std::process::id()))
                .map_err(|err| fail("name the verified code dir", &std::env::temp_dir(), err))?;
        let dir = std::env::temp_dir().join(name);
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&dir)
            .map_err(|err| fail("create verified code dir", &dir, err))?;
        // From here on, dropping `copy` removes the directory.
        let copy = Self { dir };
        let path = copy.dir.join(entry_point);
        if let Some(parent) = path.parent() {
            std::fs::DirBuilder::new()
                .mode(0o755)
                .recursive(true)
                .create(parent)
                .map_err(|err| fail("create verified code subdir", parent, err))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o555)
            .open(&path)
            .map_err(|err| fail("create verified code file", &path, err))?;
        file.write_all(code)
            .map_err(|err| fail("write verified code file", &path, err))?;
        // The entry point is read and execute only: the cell runs it and
        // nothing can write it.
        // The creation modes above are masked by the process umask. Set
        // them explicitly: a root broker maps the skill cell to an
        // unprivileged uid, which must traverse the copy and read the
        // code whatever umask the broker runs under.
        let set_mode = |target: &Path, mode: u32| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode))
                .map_err(|err| fail("set the mode of", target, err))
        };
        let mut current = copy.dir.clone();
        set_mode(&current, 0o755)?;
        for component in Path::new(entry_point)
            .parent()
            .into_iter()
            .flat_map(Path::components)
        {
            current.push(component);
            set_mode(&current, 0o755)?;
        }
        set_mode(&path, 0o555)?;
        Ok(copy)
    }

    /// The directory holding the verified copy (mounted at `/skill`).
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for VerifiedCode {
    fn drop(&mut self) {
        // Through `remove_tree` (WO-024): nothing outside the copy goes.
        let _ = hatchling_sys::remove_dir_tree(&self.dir);
    }
}

/// A running skill cell and the verified code copy it mounts.
///
/// Field order matters: the cell is killed and reaped (or waited for)
/// before the copy it mounts is removed.
#[derive(Debug)]
pub struct SkillCellHandle {
    cell: CellHandle,
    code: VerifiedCode,
}

impl SkillCellHandle {
    /// The verified code copy mounted at `/skill`.
    #[must_use]
    pub const fn code(&self) -> &VerifiedCode {
        &self.code
    }

    /// Wait for the skill cell to exit (see [`CellHandle::wait`]), then
    /// remove its code copy.
    ///
    /// # Errors
    ///
    /// Propagates [`CellHandle::wait`]'s errors.
    pub fn wait(self) -> Result<i32, Error> {
        let Self { cell, code } = self;
        let status = cell.wait();
        drop(code);
        status
    }
}

/// Verify a skill's code and build the cell config that mounts a
/// private copy of it: every check of [`spawn_skill_cell`], without the
/// spawn.
///
/// The entry point is read from `code_dir` without following a symlink
/// in any path component, hashed against the registry's pinned hash,
/// and copied into a fresh [`VerifiedCode`] directory; the returned
/// config mounts that copy, never the live directory, and runs the
/// copied entry point as the cell's program.
///
/// # Errors
///
/// Returns [`Error::SkillLoad`] if the code cannot be read without
/// following a symlink, does not match the pinned hash, or cannot be
/// copied.
pub fn prepare_skill_cell(config: &SkillCellConfig) -> Result<(CellConfig, VerifiedCode), Error> {
    let skill = config.skill_id().to_string();
    let entry_point = config.skill.admission().manifest().entry_point.clone();
    let code =
        crate::skills::read_skill_file(&config.code_dir, &entry_point, &skill, "skill code")?;
    let found = hatchling_skill::CodeHash::of(&code);
    if *config.skill.code_hash() != found {
        return Err(Error::SkillLoad {
            skill,
            reason: format!(
                "code drift detected before spawn: expected {}, found {}",
                config.skill.code_hash(),
                found
            ),
        });
    }
    let copy = VerifiedCode::write(&skill, &entry_point, &code)?;
    let mut cell = config.cell.clone();
    cell.skill_code = Some(copy.dir().to_path_buf());
    // The program the cell runs is the verified copy of the entry point,
    // never anything the base config named.
    cell.agent_bin = copy.dir().join(&entry_point);
    Ok((cell, copy))
}

/// Spawn a dedicated cell for a skill.
///
/// This uses the same [`crate::spawn_cell`] infrastructure as the agent
/// cell, with the config [`prepare_skill_cell`] builds: the verified
/// entry point is mounted read-only at `/skill` from a private copy of
/// exactly the hashed bytes. A skill whose code drifted, or whose entry
/// point runs through a symlink, is never spawned.
///
/// # Errors
///
/// Returns [`Error::SkillLoad`] from [`prepare_skill_cell`], and
/// propagates [`Error`] from cell spawning.
pub fn spawn_skill_cell(
    config: &SkillCellConfig,
    log_level: LogLevel,
) -> Result<SkillCellHandle, Error> {
    let (cell, code) = prepare_skill_cell(config)?;
    tracing::info!(
        skill = %config.skill_id(),
        code_dir = %config.code_dir.display(),
        verified_copy = %code.dir().display(),
        "spawning skill cell"
    );
    let cell = crate::spawn_cell(&cell, log_level)?;
    Ok(SkillCellHandle { cell, code })
}

/// Build the mount plan entries for a skill cell.
///
/// The skill's code directory is bind-mounted read-only at `/skill`
/// in the cell. This is in addition to the standard cell mounts
/// (socket dir, etc.) from the base [`CellConfig`].
///
/// This function returns the extra bind mounts; the caller merges them
/// into the cell's mount plan.
#[must_use]
pub fn skill_code_mount(code_dir: &Path) -> (PathBuf, String) {
    (code_dir.to_path_buf(), String::from("skill"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registered skill whose pinned code is `b"v1"` at `main.py`.
    fn registered_skill() -> RegisteredSkill {
        registered_skill_at("main.py")
    }

    /// A registered skill whose pinned code is `b"v1"` at `entry_point`.
    fn registered_skill_at(entry_point: &str) -> RegisteredSkill {
        use ed25519_dalek::{Signer, SigningKey};
        let signing = SigningKey::from_bytes(&[0x05; 32]);
        let manifest = hatchling_skill::Manifest {
            name: String::from("cell-skill"),
            version: String::from("1.0.0"),
            capabilities: vec!["fs.read".parse().unwrap()],
            entry_point: entry_point.to_owned(),
        };
        let sig = signing.sign(&manifest.canonical_bytes().unwrap());
        let signed = hatchling_skill::SignedManifest {
            manifest,
            signature: Some(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                sig.to_bytes(),
            )),
        };
        let grants: std::collections::HashSet<hatchling_core::CapabilityName> =
            std::iter::once("fs.read".parse().unwrap()).collect();
        let admission = hatchling_skill::verify_admission(
            &signed,
            &signing.verifying_key().to_bytes(),
            &grants,
        )
        .unwrap();
        let mut registry = hatchling_skill::SkillRegistry::new();
        let id = registry.register(admission, b"v1").unwrap();
        registry.get(&id).unwrap().clone()
    }

    /// A cell config for `/bin/true`. The drift check must refuse before
    /// any spawn; a check that let it through would spawn this harmless
    /// sandboxed cell and return `Ok`, failing the test.
    fn cell_config() -> CellConfig {
        use hatchling_isolation::IsolationBackend as _;
        let base = hatchling_isolation::platform()
            .identity(None, 1)
            .unwrap()
            .principal;
        CellConfig::parse_test(
            Path::new("/bin/true"),
            "skill-test",
            Path::new("/tmp"),
            base,
        )
        .unwrap()
    }

    #[test]
    fn skill_cell_config_requires_an_absolute_code_dir() {
        assert!(matches!(
            SkillCellConfig::new(
                registered_skill(),
                PathBuf::from("relative/path"),
                cell_config()
            ),
            Err(Error::InvalidConfig { .. })
        ));
        assert!(
            SkillCellConfig::new(
                registered_skill(),
                PathBuf::from("/skills/cell-skill"),
                cell_config()
            )
            .is_ok()
        );
    }

    #[test]
    fn drifted_code_is_refused_before_any_spawn() {
        // The skill was registered with `b"v1"`; write different bytes
        // to the code dir. The spawn must read the mounted tree (not
        // caller-passed bytes) and refuse the drift.
        let dir =
            std::env::temp_dir().join(format!("hatchling-skill-cell-{}-drift", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.py"), b"tampered").unwrap();
        let config = SkillCellConfig::new(registered_skill(), dir.clone(), cell_config()).unwrap();
        let err = spawn_skill_cell(&config, LogLevel::Error).unwrap_err();
        assert!(
            matches!(&err, Error::SkillLoad { reason, .. } if reason.contains("drift")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn symlinked_code_is_refused_before_any_spawn() {
        // A symlink at the entry point is refused, not followed.
        let dir = std::env::temp_dir().join(format!(
            "hatchling-skill-cell-{}-symlink",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("real.py");
        std::fs::write(&target, b"v1").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, dir.join("main.py")).unwrap();
        let config = SkillCellConfig::new(registered_skill(), dir.clone(), cell_config()).unwrap();
        let err = spawn_skill_cell(&config, LogLevel::Error).unwrap_err();
        assert!(
            matches!(&err, Error::SkillLoad { reason, .. } if reason.contains("symlink")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skill_code_mount_target_is_skill() {
        let dir = PathBuf::from("/skills/test-skill");
        let (source, target) = skill_code_mount(&dir);
        assert_eq!(source, dir);
        assert_eq!(target, "skill");
    }

    /// A fresh code dir unique to this test process and test.
    fn code_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-skill-cell-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn prepare_mounts_a_private_copy_of_the_verified_bytes() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = code_dir("copy");
        std::fs::write(dir.join("main.py"), b"v1").unwrap();
        let config = SkillCellConfig::new(registered_skill(), dir.clone(), cell_config()).unwrap();
        let (cell, copy) = prepare_skill_cell(&config).unwrap();
        assert_eq!(cell.skill_code.as_deref(), Some(copy.dir()));
        assert_ne!(copy.dir(), dir.as_path());
        let copied = copy.dir().join("main.py");
        assert_eq!(std::fs::read(&copied).unwrap(), b"v1");
        assert_eq!(
            std::fs::metadata(&copied).unwrap().permissions().mode() & 0o777,
            0o555
        );
        assert_eq!(cell.agent_bin, copied);
        // The race the live-directory mount had: a host-side swap after
        // the hash. The cell mounts the copy, which keeps the hashed bytes.
        std::fs::write(dir.join("main.py"), b"v2 swapped after the hash").unwrap();
        assert_eq!(std::fs::read(&copied).unwrap(), b"v1");
        let copy_dir = copy.dir().to_path_buf();
        drop(copy);
        assert!(!copy_dir.exists(), "the copy must be removed on drop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_copies_a_nested_entry_point() {
        let dir = code_dir("nested");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("main.py"), b"v1").unwrap();
        let config = SkillCellConfig::new(
            registered_skill_at("sub/main.py"),
            dir.clone(),
            cell_config(),
        )
        .unwrap();
        let (_cell, copy) = prepare_skill_cell(&config).unwrap();
        assert_eq!(
            std::fs::read(copy.dir().join("sub").join("main.py")).unwrap(),
            b"v1"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_directory_in_the_entry_point_is_refused() {
        // `sub` links out of the package to a file with the pinned bytes:
        // the final component is a regular file, so a final-component
        // check (the hash matches, too) would have followed the link.
        let dir = code_dir("nested-link");
        let outside = code_dir("nested-link-target");
        std::fs::write(outside.join("main.py"), b"v1").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("sub")).unwrap();
        let config = SkillCellConfig::new(
            registered_skill_at("sub/main.py"),
            dir.clone(),
            cell_config(),
        )
        .unwrap();
        let err = prepare_skill_cell(&config).unwrap_err();
        assert!(
            matches!(&err, Error::SkillLoad { reason, .. } if reason.contains("symlink")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn the_config_alone_never_mounts_the_live_directory() {
        let dir = code_dir("no-live-mount");
        let config = SkillCellConfig::new(registered_skill(), dir.clone(), cell_config()).unwrap();
        assert!(config.cell.skill_code.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verified_code_is_a_read_only_copy_that_goes_with_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;
        let code = VerifiedCode::write("copy", "bin/entry", b"the code").unwrap();
        let dir = code.dir().to_path_buf();
        let entry = dir.join("bin/entry");
        assert_eq!(std::fs::read(&entry).unwrap(), b"the code");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            (mode(&entry), mode(&dir.join("bin")), mode(&dir)),
            (0o555, 0o755, 0o755)
        );
        let other = VerifiedCode::write("copy", "bin/entry", b"the code").unwrap();
        assert_ne!(other.dir(), dir.as_path(), "each copy is fresh");
        drop(code);
        assert!(!dir.exists(), "the copy goes when it is dropped");
        // Through the bounded walk: a tree deeper than it is only partly
        // removed, where remove_dir_all would take it all.
        let deep = VerifiedCode::write("copy", "bin/entry", b"the code").unwrap();
        let top = deep.dir().to_path_buf();
        std::fs::create_dir_all(
            (0..hatchling_sys::tree::MAX_DEPTH + 2).fold(top.clone(), |path, _| path.join("d")),
        )
        .unwrap();
        drop(deep);
        assert!(top.exists(), "removed by the walk, not remove_dir_all");
        let _ = std::fs::remove_dir_all(&top);
    }
}
