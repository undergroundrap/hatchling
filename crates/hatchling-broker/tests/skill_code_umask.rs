//! The verified skill-code copy is readable whatever the broker's umask.
//!
//! A root broker maps the skill cell to an unprivileged uid, which must
//! traverse the copy's directories and read its code. Modes passed at
//! creation are masked by the process umask, so under a strict umask
//! (`077`) they would come out `0700`/`0400`, root-owned: unreadable in
//! the cell. The umask is process-wide, so this target holds exactly one
//! test and restores the umask before asserting.

use std::os::unix::fs::PermissionsExt as _;

use hatchling_broker::CellConfig;
use hatchling_broker::skill_cell::{SkillCellConfig, prepare_skill_cell};

fn registered_skill(entry_point: &str) -> Result<hatchling_skill::RegisteredSkill, String> {
    use ed25519_dalek::{Signer, SigningKey};
    let signing = SigningKey::from_bytes(&[0x0b; 32]);
    let manifest = hatchling_skill::Manifest::for_program(
        "umask-skill",
        "1.0.0",
        "A test skill.",
        entry_point,
        b"code",
        vec!["fs.read".parse().map_err(|e| format!("{e}"))?],
    );
    let payload = manifest.canonical_bytes().map_err(|e| e.to_string())?;
    let signed = hatchling_skill::SignedManifest {
        manifest,
        signature: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signing.sign(&payload).to_bytes(),
        )),
    };
    let grants: std::collections::HashSet<hatchling_core::CapabilityName> =
        std::iter::once("fs.read".parse().map_err(|e| format!("{e}"))?).collect();
    let admission =
        hatchling_skill::verify_admission(&signed, &signing.verifying_key().to_bytes(), &grants)
            .map_err(|e| e.to_string())?;
    let mut registry = hatchling_skill::SkillRegistry::new();
    let id = registry
        .register(admission, b"v1")
        .map_err(|e| e.to_string())?;
    registry
        .get(&id)
        .cloned()
        .ok_or_else(|| String::from("registered skill missing"))
}

fn mode_of(path: &std::path::Path) -> Result<u32, String> {
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .map_err(|e| format!("stat {}: {e}", path.display()))
}

/// The cell config's uid must be the default id-map base.
fn cell_uid() -> u32 {
    let euid = rustix::process::geteuid().as_raw();
    if euid == 0 { 65534 } else { euid }
}

#[test]
fn the_verified_copy_is_readable_whatever_the_umask() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("hatchling-skill-umask-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib")).map_err(|e| format!("code dir: {e}"))?;
    std::fs::write(dir.join("lib").join("main.py"), b"v1").map_err(|e| format!("code: {e}"))?;
    let cell = CellConfig::parse_test(
        std::path::Path::new("/bin/true"),
        "skill-umask",
        std::path::Path::new("/tmp"),
        cell_uid(),
    )
    .map_err(|e| format!("cell config: {e}"))?;
    let config = SkillCellConfig::new(registered_skill("lib/main.py")?, dir.clone(), cell)
        .map_err(|e| format!("skill cell config: {e}"))?;

    let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let prepared = prepare_skill_cell(&config);
    rustix::process::umask(old);

    let (_cell, copy) = prepared.map_err(|e| format!("prepare: {e}"))?;
    let modes = (
        mode_of(copy.dir())?,
        mode_of(&copy.dir().join("lib"))?,
        mode_of(&copy.dir().join("lib").join("main.py"))?,
    );
    drop(copy);
    let _ = std::fs::remove_dir_all(&dir);
    if modes != (0o755, 0o755, 0o555) {
        return Err(format!(
            "verified copy modes under umask 077: dir {:o}, subdir {:o}, file {:o} \
             (want 755, 755, 555)",
            modes.0, modes.1, modes.2
        ));
    }
    Ok(())
}
