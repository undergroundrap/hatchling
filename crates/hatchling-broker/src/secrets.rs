//! The broker's one way into the secrets store (WO-017, step 3).
//!
//! The secrets store holds the operator's credentials. The broker's own
//! reads (an egress credential, a harness vendor's key, a channel
//! account's credential, the gates' scan of held secrets) go through
//! [`Secrets::broker_read`], which names what each read is for and
//! records it in the vault's access log before releasing anything: a read
//! that can't be recorded releases nothing. A cell session's vault ops
//! come here too ([`handle_vault_op`]), checked against the cell's grants
//! and recorded the same way.
//!
//! This is the seam the keeper takes over. Today it runs in the broker,
//! over the keys it was handed; the keeper will hold the secrets store's
//! key in a process of its own and answer these calls there.

use std::path::Path;

use hatchling_core::{CellId, SecretB64, SecretName, VaultOp, VaultResult, decode_secret};
use hatchling_vault::{OpensStores, SealedVault, Secret};

use crate::vault_access::{Caller, Outcome};
use crate::{Error, VaultConfig};

/// The secrets store, opened with the broker's keys.
#[derive(Debug, Clone, Copy)]
pub struct Secrets<'k> {
    root: &'k Path,
    keys: &'k dyn OpensStores,
}

impl<'k> Secrets<'k> {
    /// The secrets store at `root` (the vault's), opened with `keys`.
    #[must_use]
    pub const fn new(root: &'k Path, keys: &'k dyn OpensStores) -> Self {
        Self { root, keys }
    }

    /// The broker's own read of `names`, for `purpose`: each name's
    /// secret, or `None` where nothing is stored under it.
    ///
    /// The read is recorded in the vault's access log as the broker's, as
    /// one access, before anything is returned. It is recorded as found
    /// when any secret was, not found when none was, and failed when the
    /// store couldn't be read. A read that can't be recorded returns
    /// nothing, and every secret it read is wiped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vault`] when the store can't be opened or a
    /// secret can't be read, and [`Error::VaultAccessLog`] when the read
    /// can't be recorded.
    pub fn broker_read(
        &self,
        purpose: &str,
        names: &[SecretName],
    ) -> Result<Vec<Option<Secret>>, Error> {
        // A broker that started a keeper asks it: the secrets store's key
        // is the keeper's alone.
        if let Some(keeper) = crate::keeper::keeper() {
            return keeper.read(purpose, names);
        }
        self.recorded_read(
            Caller::Broker {
                purpose: purpose.to_owned(),
            },
            names,
        )
    }

    /// Reads `names` for `caller`, recorded before anything is returned:
    /// as [`Self::broker_read`] without a keeper, for whoever `caller` is.
    /// The keeper reads with it for the broker and the egress authority.
    ///
    /// # Errors
    ///
    /// As [`Self::broker_read`].
    pub(crate) fn recorded_read(
        &self,
        caller: Caller,
        names: &[SecretName],
    ) -> Result<Vec<Option<Secret>>, Error> {
        let read = self.read(names);
        let outcome = match &read {
            Ok(found) if found.iter().any(Option::is_some) || found.is_empty() => Outcome::Allowed,
            Ok(_) => Outcome::NotFound,
            Err(_) => Outcome::Failed,
        };
        crate::vault_access::record(
            self.root,
            caller,
            "read",
            names.iter().map(|name| name.as_str().to_owned()).collect(),
            outcome,
        )?;
        read
    }

    /// Each of `names`, read from the store.
    fn read(&self, names: &[SecretName]) -> Result<Vec<Option<Secret>>, Error> {
        let vault =
            SealedVault::open(self.root, self.keys).map_err(|source| Error::Vault { source })?;
        names
            .iter()
            .map(|name| match vault.fetch(name) {
                Ok(secret) => Ok(Some(secret)),
                Err(hatchling_vault::Error::NotFound { .. }) => Ok(None),
                Err(source) => Err(Error::Vault { source }),
            })
            .collect()
    }
}

/// Authorizes and executes one vault op, producing the reply.
///
/// The grant check fires before the vault is touched: an op the broker
/// holds no grant for never reaches the filesystem, so ungranted names
/// cannot be probed for existence. Denials and invalid names both answer
/// `Denied` — the agent learns nothing beyond "no".
///
/// The vault itself is opened here, after name validation and grant
/// authorization: opening (which creates the root when absent) is a
/// filesystem side effect, and it happens only for ops the peer is
/// allowed to perform.
///
/// Backend failures (filesystem errors, oversize values) collapse to the
/// opaque `VaultError` reply: the session survives them, and the agent
/// learns that the op failed, not why. Only malformed request bytes
/// (undecodable base64) end the session — those are protocol violations.
///
/// # Errors
///
/// Returns [`Error::SessionProtocol`] if the op's secret payload is not
/// valid base64, or [`Error::Vault`] if the vault root cannot be opened.
pub(crate) fn handle_vault_op(
    vault_config: &VaultConfig,
    key: &dyn OpensStores,
    op: &VaultOp,
    cell_id: &CellId,
    uid: u32,
) -> Result<VaultResult, Error> {
    if let Some(keeper) = crate::keeper::keeper() {
        return keeper.cell_op(op, cell_id, uid);
    }
    let result = vault_op_result(vault_config, key, op, cell_id)?;
    // Every access is recorded. One that went ahead but cannot be
    // recorded fails closed: no secret leaves unlogged.
    let outcome = match result {
        VaultResult::Fetched { .. } | VaultResult::Stored | VaultResult::Removed => {
            Outcome::Allowed
        }
        VaultResult::NotFound => Outcome::NotFound,
        VaultResult::Denied => Outcome::Denied,
        VaultResult::VaultError => Outcome::Failed,
    };
    let name = op.name().parse::<SecretName>().map_or_else(
        |_| String::from("(not a secret name)"),
        |name| name.as_str().to_owned(),
    );
    let caller = Caller::Session {
        uid,
        cell: cell_id.to_string(),
    };
    match crate::vault_access::record(
        &vault_config.root,
        caller,
        &op.action().to_string(),
        vec![name],
        outcome,
    ) {
        Ok(()) => Ok(result),
        Err(error) if outcome == Outcome::Allowed => {
            tracing::error!(%error, "vault op not recorded: answered as failed");
            Ok(VaultResult::VaultError)
        }
        Err(error) => {
            tracing::error!(%error, "vault op not recorded");
            Ok(result)
        }
    }
}

/// The vault op's result, before it is recorded.
pub(crate) fn vault_op_result(
    vault_config: &VaultConfig,
    key: &dyn OpensStores,
    op: &VaultOp,
    cell_id: &CellId,
) -> Result<VaultResult, Error> {
    // Defense in depth: the agent's bytes are untrusted even though the
    // agent validates names client-side. A name that fails parsing can
    // never match a grant, so it is denied like any other ungranted op.
    let Ok(name) = op.name().parse::<SecretName>() else {
        tracing::warn!(
            cell_id = %cell_id,
            action = %op.action(),
            "vault op with invalid secret name"
        );
        return Ok(VaultResult::Denied);
    };
    let granted = vault_config
        .grants
        .iter()
        .any(|grant| grant.action == op.action() && grant.name == name);
    if !granted {
        tracing::warn!(
            cell_id = %cell_id,
            action = %op.action(),
            secret_name = name.as_str(),
            "denied vault op: no grant"
        );
        return Ok(VaultResult::Denied);
    }

    // Authorized: only now may the vault be opened (and its root
    // created). The `0700`/owner validation runs on every op, so a
    // store tampered with mid-session fails closed on the next op.
    // The sealed vault borrows the broker's store keys: bytes
    // on disk are ciphertext, and plaintext never touches the store.
    let vault =
        SealedVault::open(&vault_config.root, key).map_err(|source| Error::Vault { source })?;

    let result = match op {
        VaultOp::Store { secret_b64, .. } => {
            // `?` maps undecodable base64 to `SessionProtocol` via the
            // `From` impl: malformed request bytes end the session.
            let bytes = decode_secret(secret_b64.as_str())?;
            let Ok(secret) = Secret::new(bytes) else {
                return Ok(VaultResult::VaultError);
            };
            match vault.store(&name, &secret) {
                Ok(()) => {
                    tracing::info!(
                        cell_id = %cell_id,
                        secret_name = name.as_str(),
                        "stored secret"
                    );
                    VaultResult::Stored
                }
                Err(source) => {
                    tracing::error!(
                        cell_id = %cell_id,
                        secret_name = name.as_str(),
                        %source,
                        "vault store failed"
                    );
                    VaultResult::VaultError
                }
            }
        }
        VaultOp::Fetch { .. } => match vault.fetch(&name) {
            Ok(secret) => VaultResult::Fetched {
                secret_b64: SecretB64::encode(secret.as_bytes()),
            },
            Err(hatchling_vault::Error::NotFound { .. }) => VaultResult::NotFound,
            Err(source) => {
                tracing::error!(
                    cell_id = %cell_id,
                    secret_name = name.as_str(),
                    %source,
                    "vault fetch failed"
                );
                VaultResult::VaultError
            }
        },
        VaultOp::Remove { .. } => match vault.remove(&name) {
            Ok(()) => {
                tracing::info!(
                    cell_id = %cell_id,
                    secret_name = name.as_str(),
                    "removed secret"
                );
                VaultResult::Removed
            }
            Err(hatchling_vault::Error::NotFound { .. }) => VaultResult::NotFound,
            Err(source) => {
                tracing::error!(
                    cell_id = %cell_id,
                    secret_name = name.as_str(),
                    %source,
                    "vault remove failed"
                );
                VaultResult::VaultError
            }
        },
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use hatchling_vault::{MasterKey, StoreKeys};

    use super::*;
    use crate::vault_access::{ACCESS_LOG, Access, read_log};

    fn name(text: &str) -> SecretName {
        text.parse().unwrap()
    }

    /// The access each read left, newest last.
    fn accesses(root: &Path) -> Vec<(Caller, String, Vec<String>, Outcome)> {
        read_log(root)
            .unwrap()
            .0
            .into_iter()
            .map(
                |Access {
                     caller,
                     action,
                     names,
                     outcome,
                     ..
                 }| (caller, action, names, outcome),
            )
            .collect()
    }

    /// A broker read is recorded, with what it was for and how it went,
    /// before anything is released; one that can't be recorded releases
    /// nothing.
    #[test]
    fn a_broker_read_is_recorded_before_anything_is_released() {
        let root =
            std::env::temp_dir().join(format!("hatchling-secrets-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        SealedVault::open(&root, &key)
            .unwrap()
            .store(&name("token"), &Secret::new(b"t".to_vec()).unwrap())
            .unwrap();
        let secrets = Secrets::new(&root, &key);
        let mut read = secrets
            .broker_read("egress", &[name("token"), name("absent")])
            .unwrap()
            .into_iter();
        assert_eq!(read.next().flatten().unwrap().as_bytes(), b"t");
        assert!(read.next().flatten().is_none());
        assert!(read.next().is_none());
        assert!(
            secrets
                .broker_read("harness", &[name("absent")])
                .unwrap()
                .into_iter()
                .all(|secret| secret.is_none())
        );
        let unheld = StoreKeys::unwrap(&key, &[]).unwrap();
        assert!(matches!(
            Secrets::new(&root, &unheld).broker_read("scan", &[name("token")]),
            Err(Error::Vault { .. })
        ));
        let broker = |purpose: &str| Caller::Broker {
            purpose: purpose.to_owned(),
        };
        let read = String::from("read");
        assert_eq!(
            accesses(&root),
            [
                (
                    broker("egress"),
                    read.clone(),
                    vec![String::from("token"), String::from("absent")],
                    Outcome::Allowed
                ),
                (
                    broker("harness"),
                    read.clone(),
                    vec![String::from("absent")],
                    Outcome::NotFound
                ),
                (
                    broker("scan"),
                    read,
                    vec![String::from("token")],
                    Outcome::Failed
                ),
            ]
        );
        // A log that can't be written: the read releases nothing.
        std::fs::remove_file(root.join(ACCESS_LOG)).unwrap();
        std::fs::create_dir(root.join(ACCESS_LOG)).unwrap();
        let unrecorded = secrets.broker_read("egress", &[name("token")]);
        let _ = std::fs::remove_dir_all(&root);
        assert!(matches!(unrecorded, Err(Error::VaultAccessLog { .. })));
    }

    /// `recorded_read` records whoever it's given: the egress authority's
    /// read is recorded as the authority's.
    #[test]
    fn a_recorded_read_is_recorded_as_its_callers() {
        let root = std::env::temp_dir().join(format!(
            "hatchling-secrets-authority-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        SealedVault::open(&root, &key)
            .unwrap()
            .store(&name("search-brave"), &Secret::new(b"k".to_vec()).unwrap())
            .unwrap();
        let secrets = Secrets::new(&root, &key);
        let authority = Caller::Authority {
            purpose: String::from("search"),
        };
        let read = secrets
            .recorded_read(authority.clone(), &[name("search-brave")])
            .unwrap();
        let key = read.first().and_then(Option::as_ref).unwrap();
        assert_eq!(key.as_bytes(), b"k");
        let session = Caller::Session {
            uid: 7,
            cell: String::from("c"),
        };
        let absent = secrets
            .recorded_read(session.clone(), &[name("absent")])
            .unwrap();
        assert!(matches!(absent.as_slice(), [None]));
        let logged = accesses(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            logged,
            [
                (
                    authority,
                    String::from("read"),
                    vec![String::from("search-brave")],
                    Outcome::Allowed
                ),
                (
                    session,
                    String::from("read"),
                    vec![String::from("absent")],
                    Outcome::NotFound
                ),
            ]
        );
    }
}
