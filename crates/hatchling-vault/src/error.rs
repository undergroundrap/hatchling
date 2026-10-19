//! Typed errors for `hatchling-vault`.
//!
//! Every failure the store can report is a variant here. Secret *values*
//! never appear in any variant: [`NotFound`](Error::NotFound) carries the
//! name (names are not secret), and nothing carries the bytes.

use std::path::PathBuf;

use thiserror::Error;

/// All errors producible by `hatchling-vault`.
#[derive(Debug, Error)]
pub enum Error {
    /// No secret is stored under the name.
    #[error("secret not found: {name}")]
    NotFound {
        /// The name that was looked up.
        name: hatchling_core::SecretName,
    },

    /// A secret exceeded the size limit.
    #[error("secret of {len} bytes exceeds the {max}-byte limit")]
    TooLarge {
        /// The rejected size, in bytes.
        len: usize,
        /// The limit, in bytes.
        max: usize,
    },

    /// A required directory (the vault root, or the key-file parent)
    /// exists but is not a directory. The vault will not follow a
    /// symlink or treat a file as a store.
    #[error("directory {path} exists and is not a directory")]
    NotADirectory {
        /// The offending path.
        path: PathBuf,
    },

    /// A required directory exists but has the wrong mode or owner.
    /// The vault refuses to operate on a store another user could read
    /// or has tampered with.
    #[error(
        "directory {path} has insecure permissions or owner \
         (expected mode 700, owned by uid {expected_uid}); fix with: chmod 700 {path}"
    )]
    InsecurePermissions {
        /// The offending path.
        path: PathBuf,
        /// The uid that must own the root.
        expected_uid: u32,
    },

    /// An underlying filesystem operation failed.
    #[error("vault I/O failed: {source}")]
    Io {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// A sealed-vault file lacks the envelope magic. The sealed vault
    /// never falls back to plaintext: a non-envelope file is a tamper
    /// or misconfiguration signal, and the read fails closed.
    #[error(
        "secret {name} is not a sealed envelope; refusing to read it as plaintext — run hatchling-broker migrate-vault to seal it"
    )]
    NotSealed {
        /// The name that was looked up.
        name: hatchling_core::SecretName,
    },

    /// A file is an older envelope: version 1, which names neither its
    /// secret nor its store, or version 2, sealed under the master key
    /// rather than its store's key. Read only by the migration that
    /// re-seals it.
    #[error(
        "secret {name} is an older sealed envelope (v1 or v2); run hatchling-broker migrate-vault to re-seal it"
    )]
    OldEnvelope {
        /// The name that was looked up.
        name: hatchling_core::SecretName,
    },

    /// A store holds files that aren't current sealed envelopes: old
    /// (v1) envelopes, unknown versions, or plaintext. The broker refuses
    /// to serve until `migrate-vault` has re-sealed them.
    #[error(
        "the {store} store holds files that aren't current sealed envelopes: {names}; run hatchling-broker migrate-vault"
    )]
    UnmigratedStore {
        /// The store's label.
        store: &'static str,
        /// The files' names, comma-separated, sorted.
        names: String,
    },

    /// Sealing a secret failed. Defensive: sealing with a valid key
    /// and a fresh nonce cannot fail; this variant exists so the
    /// failure mode is typed instead of a panic.
    #[error("failed to seal secret")]
    SealFailed {
        /// The underlying AEAD failure.
        source: chacha20poly1305::aead::Error,
    },

    /// A sealed-vault file carries the envelope magic but is too short
    /// to hold a nonce and an authentication tag.
    #[error("secret {name} has a truncated sealed envelope")]
    Malformed {
        /// The name that was looked up.
        name: hatchling_core::SecretName,
    },

    /// A store's key file is missing while the store holds files sealed
    /// under it: they can't be opened (WO-017).
    #[error(
        "the {store} store's key file (.store-key) is missing, so its sealed files can't be opened; restore it from the backup it was made with"
    )]
    StoreKeyMissing {
        /// The store.
        store: &'static str,
    },

    /// A store's key file isn't a wrapped store key.
    #[error("the {store} store's key file (.store-key) is not a wrapped store key")]
    StoreKeyMalformed {
        /// The store.
        store: &'static str,
    },

    /// A process was handed the keys of some stores ([`StoreKeys`]) and
    /// asked to open another, or one at another root.
    ///
    /// [`StoreKeys`]: crate::StoreKeys
    #[error("the {store} store at this path is not one this process was handed")]
    StoreNotHeld {
        /// The store.
        store: &'static str,
    },

    /// A store's key didn't unwrap under the master key. One generic error
    /// covers a wrong master key and a changed file, as
    /// [`DecryptFailed`](Error::DecryptFailed) does for a secret.
    #[error("could not unwrap the {store} store's key: wrong key, or the file was changed")]
    StoreKeyRefused {
        /// The store.
        store: &'static str,
    },

    /// Envelope authentication failed. One generic error covers a wrong
    /// key, tampered ciphertext, and a tampered tag alike: the three
    /// are indistinguishable by design, so no oracle leaks which one
    /// happened.
    #[error("could not decrypt secret {name}: wrong key, or the file was changed")]
    DecryptFailed {
        /// The name that was looked up.
        name: hatchling_core::SecretName,
    },

    /// The master-key file could not be read. Creation failures are
    /// [`Io`](Error::Io): the read leg and the create leg are
    /// deliberately distinct, so a load that must fail closed can
    /// never be confused with a creation attempt.
    #[error("key file {path} I/O failed: {source}")]
    KeyLoad {
        /// The key file path.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The master-key file has the wrong mode, owner, or file type. A
    /// group- or world-readable key file fails closed: it would hand
    /// the whole vault to anyone who can read it.
    #[error(
        "key file {path} has insecure permissions, owner, or file type (expected mode 600, owned by the broker uid, a regular file); fix with: chmod 600 {path}"
    )]
    InsecureKeyFile {
        /// The key file path.
        path: PathBuf,
    },

    /// The key file does not hold exactly 32 bytes of key material.
    /// The bytes are never reported: only their count.
    #[error("key file {path} holds {len} bytes of key material, expected 32")]
    InvalidKey {
        /// The key file path.
        path: PathBuf,
        /// The bytes found.
        len: usize,
    },

    /// The operating system would not provide randomness. Without it
    /// there is no key and no nonce, so the sealed vault cannot start.
    #[error("OS randomness unavailable: {source}")]
    Random {
        /// The underlying RNG failure.
        source: getrandom::Error,
    },

    /// A keyring already exists where a new one would be created. A
    /// keyring is never overwritten: it holds the only wraps of the DEK.
    #[error("a keyring already exists at {path}; refusing to overwrite it")]
    KeyringExists {
        /// The keyring path.
        path: PathBuf,
    },

    /// Another process is changing the keyring: nothing was written.
    #[error(
        "the keyring {path} is being changed by another process; nothing was written: try again"
    )]
    KeyringBusy {
        /// The keyring path.
        path: PathBuf,
    },

    /// The keyring changed since it was read: nothing was written, so no
    /// change rests on a stale copy (a spent code can't come back).
    #[error("the keyring {path} changed since it was read; nothing was written: try again")]
    KeyringChanged {
        /// The keyring path.
        path: PathBuf,
    },

    /// The keyring file could not be read or written.
    #[error("keyring {path} I/O failed: {source}")]
    KeyringLoad {
        /// The keyring path.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The keyring file is not a keyring this build reads.
    #[error("keyring {path} is unreadable: {detail}")]
    KeyringFormat {
        /// The keyring path.
        path: PathBuf,
        /// What is wrong, never what it holds.
        detail: String,
    },

    /// No wrap opened: the factor was wrong, absent, or not this
    /// vault's, or a wrap was changed. One generic error, whichever.
    #[error("the vault did not unlock")]
    UnlockFailed,

    /// The key lease is locked: the vault must be unlocked again.
    #[error("the vault is locked; unlock it again")]
    Locked,

    /// A change to the vault's protection was refused.
    #[error("the vault's protection was not changed: {reason}")]
    TierChange {
        /// Why, in words.
        reason: &'static str,
    },

    /// A security key without the FIDO2 `hmac-secret` extension cannot
    /// protect the vault: it can only answer yes or no, and a yes is no
    /// key. Enrollment stops; nothing falls back to a weaker tier.
    #[error(
        "this security key does not support the FIDO2 hmac-secret extension, so it cannot protect the vault; enroll a key that does"
    )]
    MissingHmacSecret,

    /// Hardware-key enrollment needs two keys, a primary and a backup,
    /// so losing one key does not lose the vault; or one key, knowingly,
    /// with backup codes already generated.
    #[error(
        "enroll two security keys (a primary and a backup), or one key with backup codes generated first; one lost key must not lose the vault"
    )]
    NeedTwoKeys,

    /// Single-key mode needs backup codes generated first.
    #[error(
        "run vault-keyring backup-codes before enrolling a single security key: they are the only way back if it is lost"
    )]
    NeedBackupCodes,

    /// A rotation of the vault's DEK is committed but not finished: the
    /// stores may hold both keys' envelopes. Nothing is served, and no
    /// other rotation starts, until `rekey-vault` finishes it.
    #[error(
        "a rotation of the vault's key is unfinished: run hatchling-broker rekey-vault to finish it"
    )]
    RotationUnfinished,

    /// An enrolled security key did not answer, so the vault's key can't
    /// be wrapped for it again: nothing was changed.
    #[error(
        "the security key {key} did not answer: plug in every enrolled key, or remove a lost one first (vault-keyring remove-key); nothing was changed"
    )]
    KeyAbsent {
        /// The key: its name and wrap id, or the id alone.
        key: String,
    },

    /// The one-time code did not match during enrollment: nothing was
    /// changed. Check the code, and the device's clock.
    #[error("that code did not match; check the authenticator app's code and the device's clock")]
    CodeRejected,

    /// A key derivation could not run.
    #[error("the key derivation failed")]
    Kdf,

    /// A security key was not added, removed or labelled.
    #[error("the vault's keys were not changed: {reason}")]
    KeyChange {
        /// Why, in words.
        reason: &'static str,
    },

    /// No enrolled security key has that id. Keys are named by id, never
    /// by label.
    #[error("no enrolled security key has that id; see vault-keyring status for the enrolled ids")]
    NoSuchKey,

    /// A key's label is not one this vault keeps.
    #[error("that label cannot be used: {reason}")]
    InvalidLabel {
        /// Why, in words.
        reason: &'static str,
    },

    /// The security key failed during enrollment.
    #[error("the security key failed: {reason}")]
    Authenticator {
        /// What the key reported.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// each of these says what to do about it, in the audit's words.
    #[test]
    fn the_polished_messages_say_what_to_do() {
        let name: hatchling_core::SecretName = "token".parse().unwrap();
        for (error, says) in [
            (
                Error::DecryptFailed { name: name.clone() },
                "could not decrypt secret token: wrong key, or the file was changed",
            ),
            (
                Error::NotSealed { name },
                "refusing to read it as plaintext — run hatchling-broker migrate-vault to seal it",
            ),
            (
                Error::NeedBackupCodes,
                "run vault-keyring backup-codes before enrolling a single security key",
            ),
            (
                Error::StoreNotHeld { store: "spaces" },
                "the spaces store at this path is not one this process was handed",
            ),
            (
                Error::StoreKeyRefused { store: "secrets" },
                "could not unwrap the secrets store's key: wrong key, or the file was changed",
            ),
            (
                Error::StoreKeyMissing { store: "spaces" },
                "the spaces store's key file (.store-key) is missing",
            ),
            (
                Error::StoreKeyMalformed { store: "schedules" },
                "the schedules store's key file (.store-key) is not a wrapped store key",
            ),
            (
                Error::NoSuchKey,
                "no enrolled security key has that id; see vault-keyring status for the enrolled ids",
            ),
            (
                Error::InsecurePermissions {
                    path: PathBuf::from("/srv/vault"),
                    expected_uid: 1000,
                },
                "owned by uid 1000); fix with: chmod 700 /srv/vault",
            ),
            (
                Error::InsecureKeyFile {
                    path: PathBuf::from("/srv/vault.key"),
                },
                "a regular file); fix with: chmod 600 /srv/vault.key",
            ),
        ] {
            assert!(error.to_string().contains(says), "{error}");
        }
    }
}
