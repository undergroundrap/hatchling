//! Values of any size, sealed in a store a whole version at a time
//! (WO-047).
//!
//! A sealed store keeps entries of at most [`MAX_SECRET_BYTES`] under
//! names of at most 128 bytes. A value here is kept as numbered chunks of
//! one generation, and a head names the generation in force and its chunk
//! count:
//!
//! - `h<key>`: the head, `{"generation": n, "chunks": c}`;
//! - `d<key><generation, 16 hex><chunk, 2 digits>`: its chunks.
//!
//! **A write is whole or not at all.** The new generation's chunks are
//! written first, then the head, in one atomic write, then the old
//! generation's chunks are removed. A crash before the head leaves the old
//! value in force and new chunks nothing names; after it, old chunks
//! nothing names. **A removal takes the head first**, so a value is gone
//! the moment its head is.
//!
//! A key is the first 40 hex digits of the SHA-256 of what it's for
//! ([`key_of`]), so no name on disk says which scope or item it is. Each
//! entry's envelope binds it to its store and name, so a chunk swapped
//! with another, or moved, doesn't open.

use std::fmt::Write as _;

use hatchling_vault::{MAX_SECRET_BYTES, SealedVault, Secret, SecretName};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The most chunks a value has: 99 of 32 KiB, a little over 3 MiB.
pub const MAX_CHUNKS: usize = 99;

/// Why a value couldn't be read or kept.
#[derive(Debug)]
pub enum Failure {
    /// The sealed store refused or failed.
    Vault(hatchling_vault::Error),
    /// What's kept isn't a whole value: a head that doesn't parse, or a
    /// chunk missing or of the wrong size.
    Corrupt,
    /// The value needs more than [`MAX_CHUNKS`] chunks.
    TooLarge {
        /// The value's length.
        len: usize,
    },
}

impl From<hatchling_vault::Error> for Failure {
    fn from(error: hatchling_vault::Error) -> Self {
        Self::Vault(error)
    }
}

/// A value's head: the generation in force and its chunk count.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
struct Head {
    generation: u64,
    chunks: u16,
}

/// The key of a value for `parts`: the first 40 hex digits of the
/// SHA-256 of each part, length-prefixed, so no two lists of parts share
/// one.
#[must_use]
pub fn key_of(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(part);
    }
    let mut key = String::with_capacity(64);
    for byte in hash.finalize() {
        let _ = write!(key, "{byte:02x}");
    }
    key.truncate(40);
    key
}

fn name(text: &str) -> Result<SecretName, Failure> {
    text.parse().map_err(|_| Failure::Corrupt)
}

fn head_name(key: &str) -> Result<SecretName, Failure> {
    name(&format!("h{key}"))
}

fn chunk_name(key: &str, generation: u64, chunk: u16) -> Result<SecretName, Failure> {
    name(&format!("d{key}{generation:016x}{chunk:02}"))
}

/// The head of `key`, or `None` when nothing is kept.
fn head(vault: &SealedVault<'_>, key: &str) -> Result<Option<Head>, Failure> {
    match vault.fetch(&head_name(key)?) {
        Ok(secret) => serde_json::from_slice(secret.as_bytes())
            .map(Some)
            .map_err(|_| Failure::Corrupt),
        Err(hatchling_vault::Error::NotFound { .. }) => Ok(None),
        Err(error) => Err(Failure::Vault(error)),
    }
}

/// The value kept for `key`, or `None` when there's none.
///
/// # Errors
///
/// [`Failure::Corrupt`] when a chunk the head names is missing, or every
/// chunk but the last isn't full; [`Failure::Vault`] when the store fails.
pub fn read(vault: &SealedVault<'_>, key: &str) -> Result<Option<Vec<u8>>, Failure> {
    let Some(head) = head(vault, key)? else {
        return Ok(None);
    };
    let mut value = Vec::new();
    for chunk in 0..head.chunks {
        let secret = match vault.fetch(&chunk_name(key, head.generation, chunk)?) {
            Ok(secret) => secret,
            Err(hatchling_vault::Error::NotFound { .. }) => return Err(Failure::Corrupt),
            Err(error) => return Err(Failure::Vault(error)),
        };
        let last = chunk + 1 == head.chunks;
        if !last && secret.as_bytes().len() != MAX_SECRET_BYTES {
            return Err(Failure::Corrupt);
        }
        value.extend_from_slice(secret.as_bytes());
    }
    Ok(Some(value))
}

/// Keeps `value` for `key`, whole or not at all: see the module.
///
/// # Errors
///
/// [`Failure::TooLarge`] past [`MAX_CHUNKS`] chunks, before anything is
/// written; [`Failure::Vault`] when the store fails, the old value then
/// still in force.
pub fn write(vault: &SealedVault<'_>, key: &str, value: &[u8]) -> Result<(), Failure> {
    let pieces: Vec<&[u8]> = if value.is_empty() {
        vec![value]
    } else {
        value.chunks(MAX_SECRET_BYTES).collect()
    };
    if pieces.len() > MAX_CHUNKS {
        return Err(Failure::TooLarge { len: value.len() });
    }
    let chunks = u16::try_from(pieces.len()).map_err(|_| Failure::TooLarge { len: value.len() })?;
    let old = head(vault, key)?;
    let generation = old.map_or(1, |old| old.generation.wrapping_add(1));
    for (chunk, piece) in (0..chunks).zip(&pieces) {
        let secret = Secret::new(piece.to_vec())?;
        vault.store_quietly(&chunk_name(key, generation, chunk)?, &secret)?;
    }
    let head = serde_json::to_vec(&Head { generation, chunks }).map_err(|_| Failure::Corrupt)?;
    vault.store_quietly(&head_name(key)?, &Secret::new(head)?)?;
    if let Some(old) = old {
        forget_chunks(vault, key, old);
    }
    Ok(())
}

/// Removes the value for `key`: `Ok(false)` when there was none.
///
/// # Errors
///
/// [`Failure::Vault`] when the head can't be removed.
pub fn remove(vault: &SealedVault<'_>, key: &str) -> Result<bool, Failure> {
    let Some(old) = head(vault, key)? else {
        return Ok(false);
    };
    vault.remove_quietly(&head_name(key)?)?;
    forget_chunks(vault, key, old);
    Ok(true)
}

/// Removes the chunks of an old generation, best-effort: one left behind
/// is named by no head, and never read.
fn forget_chunks(vault: &SealedVault<'_>, key: &str, old: Head) {
    for chunk in 0..old.chunks {
        if let Ok(name) = chunk_name(key, old.generation, chunk) {
            let _ = vault.remove_quietly(&name);
        }
    }
}

#[cfg(test)]
mod tests;
