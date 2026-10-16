//! One-shot vault operator actions: master-key loading, plaintext
//! migration, and key rotation.
//!
//! These are not session ops — they run before the broker serves (or
//! via the `migrate-vault` / `rekey-vault` subcommands) and never touch
//! the protocol. They are the only code paths that enumerate the store
//! directory, and they transform files in place without returning
//! secret material to any caller.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use hatchling_vault::{
    FileKey, KeySource, MasterKey, MigrateReport, RekeyReport, SealedVault, Store,
};

use crate::{Config, Error, RedactionConfig, VaultConfig};

/// Loads the broker's master key from the configured key file,
/// provisioning it with fresh `OsRng` bytes on first run.
///
/// Fail-closed on any key-file problem: the broker never serves with
/// a missing-but-uncreatable, unreadable, mispermissioned, or corrupt
/// key. See [`FileKey`] for the threat model and the operational
/// hazard of deleting the key file.
///
/// # Errors
///
/// Returns [`Error::Vault`] if the key cannot be loaded or created.
pub fn load_master_key(vault_config: &VaultConfig) -> Result<MasterKey, Error> {
    // With a keyring the key file no longer holds the key:
    // the keyring module unlocks it through the vault's tier.
    crate::vault_keyring::unlock(
        vault_config,
        &crate::vault_keyring::KeyringConfig::from_env()?,
    )
}

/// Validates that the sealed vault opens under `key`.
///
/// Checks the root's `0700`/owner discipline plus a readable store.
/// Called once at startup so a misconfigured vault stops the broker
/// before it binds, not on the first agent connection.
///
/// # Errors
///
/// Returns [`Error::Vault`] if the vault root cannot be opened.
pub fn validate_sealed_vault(vault_config: &VaultConfig, key: &MasterKey) -> Result<(), Error> {
    SealedVault::open(&vault_config.root, key)
        .map(|_| ())
        .map_err(|source| Error::Vault { source })
}

/// Every sealed store the broker keeps, as configured: its label and root.
///
/// The secrets, artifacts and spaces always; the schedules when
/// scheduling is on; the raw text of redaction records when there is an
/// output gate (`redaction`).
#[must_use]
pub fn sealed_stores(
    config: &Config,
    redaction: Option<&RedactionConfig>,
) -> Vec<(Store, PathBuf)> {
    let mut stores = vec![(Store::SECRETS, config.vault.root.clone())];
    if config.scheduler.enabled() {
        stores.push((Store::SCHEDULES, config.scheduler.root.clone()));
    }
    stores.push((Store::ARTIFACTS, config.artifacts.root.clone()));
    stores.push((Store::SPACES, config.spaces.root.clone()));
    if let Some(redaction) = redaction {
        stores.push((
            Store::REDACTIONS,
            redaction.dir.join(crate::output_gate::RAW_DIR),
        ));
    }
    stores
}

/// Refuses to serve while any of `stores` holds stale sealed files.
///
/// Old envelopes, an unknown version or plaintext are named, with their
/// store. A store whose root doesn't exist yet holds nothing, and is left
/// as it is.
///
/// # Errors
///
/// Returns [`Error::Vault`] with the first such store's names.
pub fn check_sealed_stores(stores: &[(Store, PathBuf)]) -> Result<(), Error> {
    for (store, root) in stores {
        if root.is_dir() {
            hatchling_vault::check(root, *store).map_err(|source| Error::Vault { source })?;
        }
    }
    Ok(())
}

/// One-shot migration of every existing store in `stores` to current
/// sealed envelopes: plaintext sealed, old envelopes re-sealed, each
/// bound to its store and name. Returns each store's report.
///
/// Operator action, run when adopting the sealed vault and after an
/// upgrade. See [`hatchling_vault::migrate`] for the exact semantics
/// (current envelopes are verified, not assumed; one file at a time, so
/// an interruption leaves every file readable and re-running resumes).
///
/// # Errors
///
/// Returns [`Error::Vault`] if a store cannot be migrated.
pub fn migrate_vault(
    stores: &[(Store, PathBuf)],
    key: &MasterKey,
) -> Result<Vec<(&'static str, MigrateReport)>, Error> {
    stores
        .iter()
        .filter(|(_, root)| root.is_dir())
        .map(|(store, root)| {
            hatchling_vault::migrate(root, key, *store)
                .map(|report| (store.label(), report))
                .map_err(|source| Error::Vault { source })
        })
        .collect()
}

/// One-shot master-key rotation.
///
/// `new_key_file` holds (or will hold) the replacement key: it is
/// created with fresh randomness if absent, via the same `0600`
/// discipline as the broker's own key file. The path is the
/// operator's explicit choice and may be relative to the operator's
/// cwd; the daemon's configured key path stays absolute-only. See
/// [`hatchling_vault::rekey`] for the exact semantics (two-phase,
/// resumable).
///
/// # Errors
///
/// Returns [`Error::Vault`] if the new key cannot be loaded/created
/// or the rotation fails.
pub fn rekey_vault(
    vault_config: &VaultConfig,
    old_key: &MasterKey,
    new_key_file: &Path,
) -> Result<RekeyReport, Error> {
    let new_key = FileKey::new(new_key_file.to_path_buf())
        .load_or_create()
        .map_err(|source| Error::Vault { source })?;
    hatchling_vault::rekey(&vault_config.root, old_key, &new_key, Store::SECRETS)
        .map_err(|source| Error::Vault { source })
}

/// Runs the `migrate-vault` operator subcommand.
///
/// Loads the master key and brings every sealed store the broker keeps
/// to current envelopes, in place. Prints each store's report to stdout
/// (via `io::Write`, not `println!` — the workspace denies
/// `print_stdout`).
///
/// # Errors
///
/// Returns [`Error`] from key loading, the redaction settings, or
/// migration.
pub fn run_migrate_vault(config: &Config) -> Result<(), Error> {
    let key = load_master_key(&config.vault)?;
    let redaction = config
        .channel_ingress
        .as_ref()
        .map(|_| RedactionConfig::from_env(config))
        .transpose()?;
    let reports = migrate_vault(&sealed_stores(config, redaction.as_ref()), &key)?;
    let mut stdout = std::io::stdout().lock();
    for (store, report) in reports {
        let _ = writeln!(
            stdout,
            "migrate-vault: {store}: sealed {}, re-sealed {} old, skipped {} file(s)",
            report.encrypted, report.upgraded, report.skipped
        );
    }
    Ok(())
}

/// Runs the `rekey-vault <new-key-file>` operator subcommand.
///
/// Rotates the master key, creating the new key file with fresh
/// randomness if absent. Prints the rekey report to stdout.
///
/// # Errors
///
/// Returns [`Error`] from key loading/creation or rotation.
pub fn run_rekey_vault(config: &Config, new_key_file: &Path) -> Result<(), Error> {
    let old_key = load_master_key(&config.vault)?;
    let report = rekey_vault(&config.vault, &old_key, new_key_file)?;
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(
        stdout,
        "rekey-vault: re-encrypted {} secret(s) under the new key, skipped {} file(s)",
        report.rekeyed, report.skipped
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use hatchling_vault::{Secret, SecretName};

    use super::*;

    #[test]
    fn the_master_key_loads_the_same_each_start() {
        let dir = std::env::temp_dir().join(format!("hatchling-vault-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = dir.join("vault");
        let vault = VaultConfig {
            key_file: VaultConfig::default_key_file(&root),
            root,
            grants: Vec::new(),
        };
        let first = load_master_key(&vault).unwrap();
        validate_sealed_vault(&vault, &first).unwrap();
        let name: SecretName = "probe".parse().unwrap();
        SealedVault::open(&vault.root, &first)
            .unwrap()
            .store(&name, &Secret::new(b"p".to_vec()).unwrap())
            .unwrap();
        let again = load_master_key(&vault).unwrap();
        assert!(
            SealedVault::open(&vault.root, &again)
                .unwrap()
                .fetch(&name)
                .is_ok(),
            "the same key, again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_sealed_store_is_checked_and_migrated_under_its_own_label() {
        let dir = crate::api_auth::tests::scratch("vault-ops-stores");
        let mut config =
            crate::api_auth::tests::owner_config(Some(&dir.0.join("schedules").to_string_lossy()));
        config.vault.root = dir.0.join("vault");
        config.artifacts.root = dir.0.join("artifacts");
        config.spaces.root = dir.0.join("spaces");
        let redaction = RedactionConfig {
            enabled: true,
            dir: dir.0.join("red"),
            keep_raw: true,
            timeout: std::time::Duration::from_secs(1),
            builtins: crate::output_gate::Builtins::ALL,
        };
        let stores = sealed_stores(&config, Some(&redaction));
        assert_eq!(
            stores
                .iter()
                .map(|(store, root)| (store.label(), root.clone()))
                .collect::<Vec<_>>(),
            [
                ("secrets", dir.0.join("vault")),
                ("schedules", dir.0.join("schedules")),
                ("artifacts", dir.0.join("artifacts")),
                ("spaces", dir.0.join("spaces")),
                ("redactions", dir.0.join("red").join("raw"))
            ]
        );
        // Nothing exists yet: nothing to check, nothing created.
        assert!(check_sealed_stores(&stores).is_ok());
        assert!(
            migrate_vault(&stores, &MasterKey::generate().unwrap())
                .unwrap()
                .is_empty()
        );
        assert!(!dir.0.join("spaces").exists());
        // A plaintext file in the spaces store stops the broker, by name.
        let key = MasterKey::generate().unwrap();
        hatchling_vault::Vault::open(&dir.0.join("spaces")).unwrap();
        let planted = dir.0.join("spaces").join("team-7");
        std::fs::write(&planted, b"members").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o600)).unwrap();
        SealedVault::open(&config.vault.root, &key)
            .unwrap()
            .store(
                &"probe".parse().unwrap(),
                &Secret::new(b"p".to_vec()).unwrap(),
            )
            .unwrap();
        let refused = check_sealed_stores(&stores).unwrap_err();
        let shown = refused.to_string();
        assert!(
            shown.contains("spaces") && shown.contains("team-7"),
            "{shown}"
        );
        // migrate-vault seals it into the spaces store, bound to it.
        let reports = migrate_vault(&stores, &key).unwrap();
        assert_eq!(
            reports
                .iter()
                .map(|(store, report)| (*store, report.encrypted, report.skipped))
                .collect::<Vec<_>>(),
            [("secrets", 0, 1), ("spaces", 1, 0)]
        );
        assert!(check_sealed_stores(&stores).is_ok());
        let spaces = SealedVault::open_in(&dir.0.join("spaces"), &key, Store::SPACES).unwrap();
        assert_eq!(
            spaces.fetch(&"team-7".parse().unwrap()).unwrap().as_bytes(),
            b"members"
        );
        // Under another store's label, it does not open.
        assert!(
            SealedVault::open(&dir.0.join("spaces"), &key)
                .unwrap()
                .fetch(&"team-7".parse().unwrap())
                .is_err()
        );
        // Scheduling off and no output gate: those two aren't kept.
        config.scheduler = crate::SchedulerConfig::disabled();
        let labels: Vec<_> = sealed_stores(&config, None)
            .iter()
            .map(|(store, _)| store.label())
            .collect();
        assert_eq!(labels, ["secrets", "artifacts", "spaces"]);
    }

    #[test]
    fn the_operator_commands_migrate_and_rekey_the_configured_vault() {
        let dir = crate::api_auth::tests::scratch("vault-ops-commands");
        let mut config = crate::api_auth::tests::owner_config(None);
        config.vault.root = dir.0.join("vault");
        config.vault.key_file = VaultConfig::default_key_file(&config.vault.root);
        config.artifacts.root = dir.0.join("artifacts");
        config.spaces.root = dir.0.join("spaces");
        hatchling_vault::Vault::open(&config.vault.root).unwrap();
        let planted = config.vault.root.join("tok");
        std::fs::write(&planted, b"plaintext").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o600)).unwrap();
        let name: SecretName = "tok".parse().unwrap();
        let fetch = |key: &MasterKey| {
            SealedVault::open(&config.vault.root, key)
                .unwrap()
                .fetch(&name)
                .map(|secret| secret.as_bytes().to_vec())
        };
        // migrate-vault seals the configured store under the configured key.
        assert!(check_sealed_stores(&sealed_stores(&config, None)).is_err());
        run_migrate_vault(&config).unwrap();
        assert!(check_sealed_stores(&sealed_stores(&config, None)).is_ok());
        let first = load_master_key(&config.vault).unwrap();
        assert_eq!(fetch(&first).unwrap(), b"plaintext");
        // rekey-vault moves it from the configured key to a new one.
        let keys = dir.0.join("keys");
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700)).unwrap();
        run_rekey_vault(&config, &keys.join("second")).unwrap();
        let second = FileKey::new(keys.join("second")).load().unwrap();
        assert!(fetch(&first).is_err(), "the old key still opens it");
        assert_eq!(fetch(&second).unwrap(), b"plaintext");
        // And on from that key, reporting what it did.
        let report = rekey_vault(&config.vault, &second, &keys.join("third")).unwrap();
        assert_eq!((report.rekeyed, report.skipped), (1, 0));
        let third = FileKey::new(keys.join("third")).load().unwrap();
        assert!(fetch(&second).is_err());
        assert_eq!(fetch(&third).unwrap(), b"plaintext");
    }
}
