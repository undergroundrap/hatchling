//! `hatchling-space`: spaces, and their sync, devices, and media
//! .
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! A space is a named, persistent scope with members: a project room, a
//! device pair, a shared context. Everything about who may use one is
//! the broker's to say:
//!
//! - [`record`]: the space's identity is **broker-minted**; the creator
//!   gives only a display name. Membership is checked against the
//!   session's **broker-asserted principal**, never anything the cell
//!   claims. A space's tasks hold only grants the space holds, which hold
//!   only grants the creator holds.
//! - [`admission`]: joining a space mints a ticket, and every op on the
//!   space re-checks the ticket **factor by factor**: possession,
//!   liveness of the session, the pinned peer, freshness, and that the
//!   space named is exactly the one admitted.
//! - [`sync`]: a pull or push moves **only the named space's items**, to
//!   or from a member; conflicts resolve last-writer-wins, and every
//!   overwritten write goes to the space's **conflict log**.
//! - [`device`]: devices pair **explicitly**, with single-use, short-lived
//!   codes, and **both ends authenticate** with their keys before any
//!   state moves.
//! - [`media`]: media is bounded and **typed by the broker** from its
//!   bytes, and served with headers the broker sets.
//!
//! The broker owns the store, the session table, and the device
//! endpoint; this crate holds the rules they follow. Nothing here lists or
//! exports the store.

pub mod admission;
pub mod device;
pub mod media;
pub mod record;
pub mod sync;

pub use admission::{
    ADMISSION_TTL_SECS, Admission, Admissions, MAX_ADMISSIONS_PER_SESSION, PeerPin, SessionPin,
};
pub use device::{
    DEVICE_ID_PREFIX, DeviceRecord, MAX_PAIRINGS, PAIRING_TTL_SECS, PairingRefusal, Pairings,
    RefusedPairing, auth_transcript, broker_transcript, device_id, is_nonce, pair_transcript,
    parse_key, verifies,
};
pub use media::{MAX_MEDIA_BYTES, SERVED_HEADERS, admit, served_headers, sniff};
pub use record::{
    ItemMeta, MAX_CONFLICTS, MAX_ITEMS, MAX_KEY_BYTES, MAX_MEDIA, MAX_MEMBERS, MAX_NAME_CHARS,
    MAX_TASKS, MAX_VALUE_BYTES, Principal, SpaceRecord, SpaceTask, check_key, check_name,
    check_value,
};
pub use sync::{MAX_CHANGES, PAGE_BYTES, check_push, overwritten, page};
