//! The keys memory and goals are opened with, for as long as the broker
//! serves (WO-047).
//!
//! Every connection opens memory and goals afresh, long after the
//! keeper's handoff was read, so the keys of those two stores are kept
//! here, published once at the broker's start ([`start`]), after their
//! plaintext from 0.9 and before was moved in. They are those two
//! stores' keys alone, at their roots: nothing here opens any other.

use std::path::PathBuf;
use std::sync::OnceLock;

use hatchling_vault::{OpensStores, Store};

/// The published keys.
static KEYS: OnceLock<Box<dyn OpensStores>> = OnceLock::new();

/// Makes `keys` what memory and goals are opened with, for as long as the
/// process serves. The first published stays: a broker publishes once.
pub fn publish(keys: Box<dyn OpensStores>) {
    let _ = KEYS.set(keys);
}

/// The published keys. A process that hasn't published has none, and
/// opens no memory or goals.
#[cfg(not(test))]
pub(crate) fn published() -> Option<&'static dyn OpensStores> {
    KEYS.get().map(AsRef::as_ref)
}

/// The published keys. A test's process publishes a master key of its own
/// the first time one is asked for.
#[cfg(test)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the build's signature, where there can be none"
)]
pub(crate) fn published() -> Option<&'static dyn OpensStores> {
    let keys = KEYS.get_or_init(|| {
        Box::new(
            hatchling_vault::MasterKey::generate()
                .unwrap_or_else(|_| unreachable!("the test's key")),
        )
    });
    Some(keys.as_ref())
}

/// The sealed stores of memory and goals this configuration keeps, each
/// at its root.
#[must_use]
pub fn stores(config: &crate::Config) -> Vec<(Store, PathBuf)> {
    let mut stores = Vec::new();
    if !config.memory.root.as_os_str().is_empty() {
        stores.push((
            Store::MEMORY,
            crate::memory_sealed::sealed_root(&config.memory.root),
        ));
    }
    if !config.goals.root.as_os_str().is_empty() {
        stores.push((
            Store::GOALS,
            crate::goals_sealed::sealed_root(&config.goals.root),
        ));
    }
    stores
}

/// At the broker's start, as its own user: memory and goals sealed.
///
/// Their plaintext is moved into their sealed stores
/// ([`crate::memory_sealed::migrate`], [`crate::goals_sealed::migrate`]),
/// then those stores' keys published for every read ([`publish`]). With
/// neither kept, nothing is.
///
/// # Errors
///
/// When the keeper didn't hand over a store's key, or a migration fails:
/// the broker then doesn't serve, and nothing is lost.
pub fn start(config: &crate::Config, keys: &dyn OpensStores) -> Result<(), crate::Error> {
    let stores = stores(config);
    if stores.is_empty() {
        return Ok(());
    }
    let roots: Vec<(Store, &std::path::Path)> = stores
        .iter()
        .map(|(store, root)| (*store, root.as_path()))
        .collect();
    let held = keys
        .keys_of(&roots)
        .map_err(|source| crate::Error::Vault { source })?;
    if !config.memory.root.as_os_str().is_empty() {
        crate::memory_sealed::migrate(&config.memory.root, &held)
            .map_err(|source| crate::Error::Memory { source })?;
    }
    if !config.goals.root.as_os_str().is_empty() {
        crate::goals_sealed::migrate(&config.goals.root, &held)
            .map_err(|source| crate::Error::Goals { source })?;
    }
    publish(Box::new(held));
    Ok(())
}
