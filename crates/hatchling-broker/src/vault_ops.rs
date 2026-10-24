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
    FileKey, KeySource, MasterKey, MigrateReport, RekeyReport, SealedVault, Store, StoreKeys,
};

use crate::vault_keyring::KeyringConfig;
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
    crate::vault_keyring::unlock(vault_config, &KeyringConfig::from_env()?)
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
/// The secrets always; the schedules when scheduling is on; the
/// artifacts and the spaces when each is on (a store that is off has no
/// root); the raw text of redaction records when there is an output gate
/// (`redaction`).
#[must_use]
pub fn sealed_stores(
    config: &Config,
    redaction: Option<&RedactionConfig>,
) -> Vec<(Store, PathBuf)> {
    let mut stores = vec![(Store::SECRETS, config.vault.root.clone())];
    if config.scheduler.enabled() {
        stores.push((Store::SCHEDULES, config.scheduler.root.clone()));
    }
    for (store, root) in [
        (Store::ARTIFACTS, &config.artifacts.root),
        (Store::SPACES, &config.spaces.root),
        (Store::SITES, &config.web.grants),
        (Store::PROFILES, &config.web.profiles),
        (Store::CREDENTIALS, &config.web.credentials),
    ] {
        if !root.as_os_str().is_empty() {
            stores.push((store, root.clone()));
        }
    }
    if let Some(redaction) = redaction {
        stores.push((
            Store::REDACTIONS,
            redaction.dir.join(crate::output_gate::RAW_DIR),
        ));
    }
    stores
}

/// The broker's own sealed stores: [`sealed_stores`] but the vault, which
/// is the keeper's (WO-017).
#[must_use]
pub(crate) fn broker_stores(
    config: &Config,
    redaction: Option<&RedactionConfig>,
) -> Vec<(Store, PathBuf)> {
    let mut stores = sealed_stores(config, redaction);
    stores.retain(|(store, _)| *store != Store::SECRETS);
    stores
}

/// The keys of `stores`, unwrapped under `master`, which is dropped here,
/// zeroized: what the broker serves with (WO-017). Each store's root is
/// made if absent, and a store without a key is given one.
///
/// # Errors
///
/// Returns [`Error::Vault`] for the first store whose key doesn't unwrap
/// or can't be made.
pub fn hand_store_keys(master: MasterKey, stores: &[(Store, PathBuf)]) -> Result<StoreKeys, Error> {
    let stores: Vec<(Store, &Path)> = stores
        .iter()
        .map(|(store, root)| (*store, root.as_path()))
        .collect();
    let keys = StoreKeys::unwrap(&master, &stores);
    drop(master);
    keys.map_err(|source| Error::Vault { source })
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
    stores: &[(Store, PathBuf)],
    old_key: &MasterKey,
    new_key_file: &Path,
) -> Result<Vec<(&'static str, RekeyReport)>, Error> {
    let new_key = FileKey::new(new_key_file.to_path_buf())
        .load_or_create()
        .map_err(|source| Error::Vault { source })?;
    reseal_stores(stores, old_key, &new_key)
}

/// Re-seals every existing store in `stores` from `old_key` to `new_key`,
/// each with [`hatchling_vault::rekey`] (resumable: a file already under
/// `new_key` is skipped). Returns each store's report.
///
/// # Errors
///
/// Returns [`Error::Vault`] if a store cannot be re-sealed.
pub fn reseal_stores(
    stores: &[(Store, PathBuf)],
    old_key: &MasterKey,
    new_key: &MasterKey,
) -> Result<Vec<(&'static str, RekeyReport)>, Error> {
    stores
        .iter()
        .filter(|(_, root)| root.is_dir())
        .map(|(store, root)| {
            hatchling_vault::rekey(root, old_key, new_key, *store)
                .map(|report| (store.label(), report))
                .map_err(|source| Error::Vault { source })
        })
        .collect()
}

/// Which way `rekey-vault` rotates: inside the keyring, which takes no
/// key file, or to the plain key file given, which a vault without a
/// keyring needs. Anything else is refused, and nothing is created.
fn rekey_target<'a>(
    keyring: &KeyringConfig,
    new_key_file: Option<&'a Path>,
) -> Result<Option<&'a Path>, Error> {
    match (keyring.keyring.as_deref(), new_key_file) {
        (Some(path), Some(file)) => Err(Error::Usage {
            reason: format!(
                "this vault's key is in the keyring at {} (HATCHLING_VAULT_KEYRING): rekey-vault rotates it there and takes no key file; {} was not created",
                path.display(),
                file.display()
            ),
        }),
        (Some(_), None) => Ok(None),
        (None, Some(file)) => Ok(Some(file)),
        (None, None) => Err(Error::Usage {
            reason: String::from(
                "rekey-vault requires a new key file path without a keyring: hatchling-broker rekey-vault <new-key-file>",
            ),
        }),
    }
}

/// The output gate's settings, when there is an ingress for it to guard.
pub(crate) fn redaction_config(config: &Config) -> Result<Option<RedactionConfig>, Error> {
    config
        .channel_ingress
        .as_ref()
        .map(|_| RedactionConfig::from_env(config))
        .transpose()
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
    let reports = migrate_vault(
        &sealed_stores(config, redaction_config(config)?.as_ref()),
        &key,
    )?;
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
pub fn run_rekey_vault(config: &Config, new_key_file: Option<&Path>) -> Result<(), Error> {
    let keyring = KeyringConfig::from_env()?;
    let target = rekey_target(&keyring, new_key_file)?;
    let stores = sealed_stores(config, redaction_config(config)?.as_ref());
    let (reports, codes) = match target {
        None => crate::vault_keyring::run_rekey(&config.vault, &keyring, &stores)?,
        Some(new_key_file) => {
            let old_key = load_master_key(&config.vault)?;
            // Old envelopes or plaintext stop it before any store is touched.
            check_sealed_stores(&stores)?;
            (rekey_vault(&stores, &old_key, new_key_file)?, None)
        }
    };
    let mut stdout = std::io::stdout().lock();
    for (store, report) in reports {
        let _ = writeln!(
            stdout,
            "rekey-vault: {store}: re-sealed {} under the new key, skipped {} file(s)",
            report.rekeyed, report.skipped
        );
    }
    crate::vault_keyring::new_codes(&mut stdout, codes.as_ref());
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
                .and_then(|vault| vault.fetch(&name))
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
                .and_then(|vault| vault.fetch(&"team-7".parse().unwrap()))
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
                .and_then(|vault| vault.fetch(&name))
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
        run_rekey_vault(&config, Some(&keys.join("second"))).unwrap();
        let second = FileKey::new(keys.join("second")).load().unwrap();
        assert!(fetch(&first).is_err(), "the old key still opens it");
        assert_eq!(fetch(&second).unwrap(), b"plaintext");
        // And on from that key, reporting what it did.
        let reports =
            rekey_vault(&sealed_stores(&config, None), &second, &keys.join("third")).unwrap();
        assert_eq!(
            reports
                .iter()
                .map(|(store, report)| (*store, report.rekeyed, report.skipped))
                .collect::<Vec<_>>(),
            [("secrets", 1, 0)]
        );
        let third = FileKey::new(keys.join("third")).load().unwrap();
        assert!(fetch(&second).is_err());
        assert_eq!(fetch(&third).unwrap(), b"plaintext");
    }

    #[test]
    fn rekey_vault_reseals_every_store_under_the_new_key_and_none_under_the_old() {
        let dir = crate::api_auth::tests::scratch("vault-ops-rekey-all");
        let roots = ["secrets", "schedules", "artifacts", "spaces", "redactions"]
            .map(|name| dir.0.join(name));
        let stores: Vec<(Store, PathBuf)> = [
            Store::SECRETS,
            Store::SCHEDULES,
            Store::ARTIFACTS,
            Store::SPACES,
            Store::REDACTIONS,
        ]
        .into_iter()
        .zip(roots)
        .collect();
        let old = MasterKey::generate().unwrap();
        let name: SecretName = "record".parse().unwrap();
        for (store, root) in &stores {
            SealedVault::open_in(root, &old, *store)
                .unwrap()
                .store(
                    &name,
                    &Secret::new(store.label().as_bytes().to_vec()).unwrap(),
                )
                .unwrap();
        }
        let keys = dir.0.join("keys");
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700)).unwrap();
        let reports = rekey_vault(&stores, &old, &keys.join("new")).unwrap();
        assert_eq!(
            reports
                .iter()
                .map(|(store, report)| (*store, report.rekeyed, report.skipped))
                .collect::<Vec<_>>(),
            [
                ("secrets", 1, 0),
                ("schedules", 1, 0),
                ("artifacts", 1, 0),
                ("spaces", 1, 0),
                ("redactions", 1, 0)
            ]
        );
        let new = FileKey::new(keys.join("new")).load().unwrap();
        for (store, root) in &stores {
            let under = |key: &MasterKey| {
                SealedVault::open_in(root, key, *store).and_then(|vault| vault.fetch(&name))
            };
            assert_eq!(under(&new).unwrap().as_bytes(), store.label().as_bytes());
            assert!(
                matches!(
                    under(&old),
                    Err(hatchling_vault::Error::StoreKeyRefused { .. })
                ),
                "{} still opens under the old key",
                store.label()
            );
        }
    }

    #[test]
    fn rekey_vault_rotates_inside_a_keyring_and_to_a_key_file_without_one() {
        let plain = KeyringConfig::default();
        let keyring = KeyringConfig {
            keyring: Some(PathBuf::from("/srv/hatchling/vault.key/keyring")),
            ..KeyringConfig::default()
        };
        let file = Path::new("/srv/hatchling/new.key");
        assert_eq!(rekey_target(&plain, Some(file)).unwrap(), Some(file));
        assert_eq!(rekey_target(&keyring, None).unwrap(), None);
        let refused = rekey_target(&keyring, Some(file)).unwrap_err().to_string();
        assert!(
            refused.contains("/srv/hatchling/vault.key/keyring")
                && refused.contains("HATCHLING_VAULT_KEYRING")
                && refused.contains("/srv/hatchling/new.key"),
            "{refused}"
        );
        let missing = rekey_target(&plain, None).unwrap_err().to_string();
        assert!(missing.contains("rekey-vault <new-key-file>"), "{missing}");
    }

    #[test]
    fn the_output_gate_settings_follow_the_ingress() {
        let mut config = crate::api_auth::tests::owner_config(None);
        assert!(config.channel_ingress.is_none());
        assert!(redaction_config(&config).unwrap().is_none());
        // With an ingress, the gate's raw records are a store to rekey.
        config.channel_ingress = Some("/run/hatchling-test/ingress.sock".parse().unwrap());
        let redaction = redaction_config(&config).unwrap().unwrap();
        assert_eq!(
            sealed_stores(&config, Some(&redaction))
                .last()
                .map(|(store, _)| store.label()),
            Some("redactions")
        );
    }

    #[test]
    fn rekey_vault_stops_before_touching_anything_when_a_store_is_stale() {
        let dir = crate::api_auth::tests::scratch("vault-ops-rekey-stale");
        let mut config = crate::api_auth::tests::owner_config(None);
        config.vault.root = dir.0.join("vault");
        config.vault.key_file = VaultConfig::default_key_file(&config.vault.root);
        config.artifacts.root = dir.0.join("artifacts");
        config.spaces.root = dir.0.join("spaces");
        let key = load_master_key(&config.vault).unwrap();
        let name: SecretName = "tok".parse().unwrap();
        SealedVault::open(&config.vault.root, &key)
            .unwrap()
            .store(&name, &Secret::new(b"kept".to_vec()).unwrap())
            .unwrap();
        // The spaces store, after the secrets, still holds plaintext.
        hatchling_vault::Vault::open(&config.spaces.root).unwrap();
        let planted = config.spaces.root.join("team-7");
        std::fs::write(&planted, b"members").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o600)).unwrap();
        let new_key = dir.0.join("new.key");
        let refused = run_rekey_vault(&config, Some(&new_key))
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("spaces") && refused.contains("team-7"),
            "{refused}"
        );
        assert!(!new_key.exists(), "no new key was made");
        assert_eq!(
            SealedVault::open(&config.vault.root, &key)
                .and_then(|vault| vault.fetch(&name))
                .unwrap()
                .as_bytes(),
            b"kept",
            "the secrets are still under the old key"
        );
    }

    #[test]
    fn resealing_moves_each_existing_store_and_skips_what_is_moved() {
        let dir = crate::api_auth::tests::scratch("vault-ops-reseal");
        let (old, new) = (
            MasterKey::generate().unwrap(),
            MasterKey::generate().unwrap(),
        );
        let name: SecretName = "record".parse().unwrap();
        SealedVault::open_in(&dir.0.join("spaces"), &old, Store::SPACES)
            .unwrap()
            .store(&name, &Secret::new(b"x".to_vec()).unwrap())
            .unwrap();
        let stores = [
            (Store::SECRETS, dir.0.join("absent")),
            (Store::SPACES, dir.0.join("spaces")),
        ];
        let moved = reseal_stores(&stores, &old, &new).unwrap();
        assert_eq!(
            moved
                .iter()
                .map(|(store, report)| (*store, report.rekeyed, report.skipped))
                .collect::<Vec<_>>(),
            [("spaces", 1, 0)]
        );
        assert!(!dir.0.join("absent").exists());
        let again = reseal_stores(&stores, &old, &new).unwrap();
        assert_eq!(again.first().map(|(_, report)| report.skipped), Some(1));
        assert!(
            SealedVault::open_in(&dir.0.join("spaces"), &new, Store::SPACES)
                .and_then(|vault| vault.fetch(&name))
                .is_ok()
        );
    }

    /// The broker's keys hold its configured stores, and open no other;
    /// a store whose key the master key doesn't unwrap stops the start.
    #[test]
    fn hand_store_keys_holds_the_configured_stores_alone() {
        let scratch =
            std::env::temp_dir().join(format!("hatchling-hand-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir(&scratch).unwrap();
        let (secrets, spaces) = (scratch.join("vault"), scratch.join("spaces"));
        let stores = vec![
            (Store::SECRETS, secrets.clone()),
            (Store::SPACES, spaces.clone()),
        ];
        let master = MasterKey::generate().unwrap();
        SealedVault::open_in(&spaces, &master, Store::SPACES).unwrap();
        let keys = hand_store_keys(master, &stores).unwrap();
        let held: Vec<_> = keys
            .stores()
            .map(|(store, root)| (store, root.to_path_buf()))
            .collect();
        assert_eq!(held, stores);
        assert!(SealedVault::open(&secrets, &keys).is_ok());
        assert!(SealedVault::open_in(&spaces, &keys, Store::SPACES).is_ok());
        let refused = SealedVault::open_in(&scratch.join("artifacts"), &keys, Store::ARTIFACTS);
        let other = hand_store_keys(MasterKey::generate().unwrap(), &[(Store::SPACES, spaces)]);
        let _ = std::fs::remove_dir_all(&scratch);
        assert!(matches!(
            refused,
            Err(hatchling_vault::Error::StoreNotHeld { store: "artifacts" })
        ));
        assert!(matches!(
            other,
            Err(Error::Vault {
                source: hatchling_vault::Error::StoreKeyRefused { store: "spaces" }
            })
        ));
    }

    /// The stores the broker keeps: the secrets always, the others only
    /// when they're on.
    #[test]
    fn sealed_stores_lists_only_the_stores_that_are_on() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let labels = |config: &Config| -> Vec<&'static str> {
            sealed_stores(config, None)
                .into_iter()
                .map(|(store, _)| store.label())
                .collect()
        };
        config.artifacts.root = PathBuf::new();
        config.spaces.root = PathBuf::new();
        assert_eq!(labels(&config), ["secrets"]);
        config.artifacts.root = PathBuf::from("/srv/artifacts");
        config.spaces.root = PathBuf::from("/srv/spaces");
        assert_eq!(labels(&config), ["secrets", "artifacts", "spaces"]);
        config.web.grants = PathBuf::from("/srv/sites");
        assert_eq!(labels(&config), ["secrets", "artifacts", "spaces", "sites"]);
        let brokers: Vec<_> = broker_stores(&config, None)
            .into_iter()
            .map(|(store, _)| store.label())
            .collect();
        assert_eq!(brokers, ["artifacts", "spaces", "sites"]);
    }
}
