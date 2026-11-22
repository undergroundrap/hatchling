//! A real skill cell, spawned from its verified private code copy.
//!
//! The unit tests pin what `prepare_skill_cell` builds; this pins the
//! rest of the path on a real kernel: `spawn_skill_cell` mounts the copy
//! and runs the cell, the copy exists for the cell's lifetime, and
//! `SkillCellHandle::wait` reports the real exit and then removes it.
//! The skill's code is the adversarial probe binary, run from the
//! verified copy: its `tcp-socket` attack dies with SIGSYS (exit 159)
//! deterministically, which proves the copy is what ran. Cells need user
//! namespaces: without them this skips, and `HATCHLING_REQUIRE_CELL=1`
//! makes the skip a failure.

#[allow(
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use common::{broker_config, guard_cell_available, mapped_agent_uid, probe_binary, test_dir};
use hatchling_adversarial::Attack;
use hatchling_broker::skill_cell::{SkillCellConfig, spawn_skill_cell};
use hatchling_broker::{CellConfig, bind_listener};
use hatchling_core::LogLevel;

fn registered_skill(code: &[u8]) -> Result<hatchling_skill::RegisteredSkill, String> {
    use ed25519_dalek::{Signer, SigningKey};
    let signing = SigningKey::from_bytes(&[0x09; 32]);
    let manifest = hatchling_skill::Manifest::for_program(
        "spawn-skill",
        "1.0.0",
        "A test skill.",
        "probe",
        code,
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
        .register(admission, code)
        .map_err(|e| e.to_string())?;
    registry
        .get(&id)
        .cloned()
        .ok_or_else(|| String::from("registered skill missing"))
}

#[test]
fn a_skill_cell_runs_from_its_verified_copy_and_cleans_it_up() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("skill-cell-spawn");
    let code_dir = dir.join("code");
    let socket_dir = dir.join("sock");
    std::fs::create_dir_all(&code_dir).map_err(|e| format!("code dir: {e}"))?;
    std::fs::create_dir_all(&socket_dir).map_err(|e| format!("socket dir: {e}"))?;
    // The skill's code is the probe binary itself: the cell runs the
    // verified copy of it, not the path the base config names.
    let probe = std::fs::read(probe_binary()?).map_err(|e| format!("probe: {e}"))?;
    std::fs::write(code_dir.join("probe"), &probe).map_err(|e| format!("code: {e}"))?;
    let agent_uid = mapped_agent_uid();
    let broker = broker_config(&socket_dir.join("broker.sock"), agent_uid)?;
    let _listener = bind_listener(&broker).map_err(|e| format!("bind broker socket: {e}"))?;
    let cell = CellConfig::parse_test(
        &probe_binary()?,
        &Attack::TcpSocket.cell_id(),
        &socket_dir,
        agent_uid,
    )
    .map_err(|e| format!("cell config: {e}"))?;
    let config = SkillCellConfig::new(registered_skill(&probe)?, code_dir, cell)
        .map_err(|e| format!("skill cell config: {e}"))?;
    let handle = spawn_skill_cell(&config, LogLevel::Error).map_err(|e| format!("spawn: {e}"))?;
    let copy = handle.code().dir().to_path_buf();
    let copied = std::fs::read(copy.join("probe")).map_err(|e| format!("copy: {e}"))?;
    let status = handle.wait().map_err(|e| format!("wait: {e}"))?;
    let _ = std::fs::remove_dir_all(&dir);
    if copied != probe {
        return Err(format!(
            "the cell's copy holds {copied:?}, not the verified bytes"
        ));
    }
    let code = hatchling_sys::child_exit_code(status);
    if code != 159 {
        return Err(format!("probe cell exited {code}, expected 159 (SIGSYS)"));
    }
    if copy.exists() {
        return Err(format!(
            "verified copy {} outlived the cell",
            copy.display()
        ));
    }
    Ok(())
}
