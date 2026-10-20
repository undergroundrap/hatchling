//! `hatchling-vault`: the broker-owned credential store.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! One file per secret under a `0700` root directory. Names are validated
//! [`SecretName`](hatchling_core::SecretName)s, so path traversal is
//! unrepresentable; values are zeroized on drop ([`Secret`]) and never
//! logged; writes are atomic (temp file + rename). There is deliberately
//! no `list` operation — the store cannot enumerate, so a compromised
//! reader cannot bulk-read.
//!
//! The [`seal`] module adds encryption at rest: [`SealedVault`] stores
//! every secret as a versioned XChaCha20-Poly1305 envelope under a
//! 256-bit [`MasterKey`](seal::MasterKey), loaded through a
//! [`KeySource`](seal::KeySource). Sealing sits *below* the store —
//! the atomic-write pipeline, the `0700` root validation, and the
//! no-`list` invariant are inherited, not reimplemented.
//!
//! The [`keyring`] module makes that master key a DEK, wrapped
//! once per way of unlocking it: a KEK file ([`Tier::File`]), an
//! authenticator app's codes ([`Tier::Totp`], weaker by construction:
//! [`totp`] says why), or a FIDO2 security key's `hmac-secret`
//! ([`Tier::HardwareKey`], [`fido2`]), with [`backup`] codes to recover.
//! A tier changes only with its own unlock; a backup code restores, never
//! lowers. [`KeyLease`] scopes how long the DEK stays in memory.
//!
//! The vault is a library, not a daemon. Only the broker drives it, over
//! the peer-authorized session (the agent never touches these files;
//! filesystem permissions are the second barrier).

pub mod backup;
pub mod error;
pub mod fault;
pub mod fido2;
pub mod handoff;
pub mod keyring;
pub mod lease;
pub mod manage;
pub mod rotate;
pub mod seal;
pub mod secret;
pub mod totp;
pub mod vault;

// The name alphabet and the size bound are wire-contract rules: both
// ends of the protocol validate the same values, so they live in
// `hatchling-core` and are re-exported here for the broker's use.
pub use backup::BackupCodes;
pub use error::Error;
pub use fido2::{Authenticator, AuthenticatorError, Credential, KeyCheck, SingleKeyRisk};
pub use hatchling_core::{MAX_SECRET_BYTES, SecretName};
pub use keyring::{Factor, Kek, Keyring, Tier, Unlocked, Wrap};
pub use lease::KeyLease;
pub use manage::{KeyInfo, Label, MAX_LABEL_CHARS, Protection, Warning};
pub use rotate::{Rotated, Rotation};
pub use seal::{
    FileKey, KeySource, MasterKey, MigrateReport, OpenedKey, OpensStores, RekeyReport,
    STORE_KEY_FILE, SealedVault, Store, StoreKeys, check, holds_envelopes, migrate, rekey,
};
pub use secret::Secret;
pub use totp::{PendingTotp, totp_code};
pub use vault::Vault;

#[cfg(test)]
pub mod testutil;
#[cfg(any(test, feature = "virtual-authenticator"))]
pub mod virtual_key;
