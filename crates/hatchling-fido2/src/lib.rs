//! The FIDO2 transport: CTAP2 over USB HID, as the vault's
//! `Authenticator`.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! A security key is reached in layers, each strict about what it accepts
//! from the device:
//!
//! - [`ctaphid`]: messages framed in 64-byte HID reports, on a channel of
//!   their own, with a hard deadline;
//! - [`cbor`]: CTAP2's canonical CBOR subset, bounded;
//! - [`pin`]: the PIN/UV auth protocols 1 and 2 (key agreement, the
//!   encrypted `hmac-secret` salt and output, PIN tokens);
//! - [`ctap2`] and [`device`]: `makeCredential` and `getAssertion` with
//!   `hmac-secret`, every reply checked for whom it answers, presence and,
//!   for credentials made in the session, its signature;
//! - [`linux`]: keys found and opened through `hidraw` ([`Bus`]).
//!
//! The transport is not a second trust root: it moves bytes, and the
//! vault decides. A key that lies, goes silent or replays yields an error
//! or an output that opens nothing; the vault's state changes only when a
//! whole operation succeeds.

pub mod cbor;
pub mod ctap2;
pub mod ctaphid;
pub mod device;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod pin;
#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "the software key serves this crate's tests only"
)]
pub(crate) mod soft;

pub use device::{Device, PinPrompt};

/// Where security keys are found: the keys plugged in now, each opened
/// and ready.
///
/// Linux reaches them through `hidraw` ([`linux::Hidraw`]); another
/// platform implements this for its own HID stack.
pub trait Bus {
    /// One key, as the vault drives it.
    type Key: hatchling_vault::Authenticator;

    /// Every key plugged in now.
    ///
    /// # Errors
    ///
    /// Returns a message naming what could not be reached: a key that
    /// cannot be opened is reported, never silently left out.
    fn keys(&mut self) -> Result<Vec<Self::Key>, String>;
}
