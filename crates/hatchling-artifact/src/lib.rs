//! `hatchling-artifact`: the artifact pipeline core, its build
//! kinds and inspector, and the ideas path.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! An artifact is what a builder makes from inputs the agent supplies.
//! The pipeline treats a build as the place untrusted output becomes
//! user-facing content, so the pipeline itself is the boundary:
//!
//! - [`request`]: a build request is **pinned** when the broker receives
//!   it. Every input is hashed, and the builder receives exactly those
//!   bytes, so what was built is answerable from the record.
//! - [`kind`]: a build kind is admitted by **registering a profile**:
//!   its content type, size bound, and shape checks. The **inspector**
//!   runs them on every completed build and reports; it is advisory and
//!   never decides a delivery. Media kinds plug in the same way.
//! - [`record`]: the builder's output is only a *claim*. The broker
//!   parses it strictly (a builder cannot write provenance or anything
//!   else the broker owns), hashes the bytes itself, and records
//!   **broker-asserted provenance**: the inputs it pinned, the builder's
//!   signing identity, the output hash it observed, and the inspector's
//!   report. A UI names the builder only through [`attribution`], which
//!   says "verified" only for an identity the broker vouches for now.
//! - [`gate`]: the **share-review gate** judges the final bytes, never
//!   the builder's summary or the inspector's report, against every
//!   secret the broker holds and the configured review policy. Anything
//!   it cannot judge is an error, and an error blocks.
//! - [`deliver`]: the **one delivery function**. Every target goes
//!   through it: it delivers only a completed build, re-hashes the bytes
//!   against the recorded output hash, and requires a pass verdict.
//! - [`idea`]: ideas are **captured** (with where from), **listed**, and
//!   **developed** (refined, noted), and become builds only through a
//!   **confirmed activation**: the exact request is pinned at
//!   proposal, and built only with the broker's confirmation for it.
//!
//! The broker owns the store, the builder runs (through the skill gate,
//! ), and the delivery targets; this crate holds the rules they
//! follow.

pub mod deliver;
pub mod gate;
pub mod idea;
pub mod kind;
pub mod record;
pub mod request;

pub use deliver::{DeliveryRecord, Sink, deliver};
pub use gate::{ReviewPolicy, Reviewer, SecretSet, ShareReview, Verdict};
pub use idea::{
    Activation, IDEA_INPUT, IdeaOrigin, IdeaRecord, MAX_ACTIVATIONS, MAX_DESCRIPTION_BYTES,
    MAX_IDEA_BYTES, MAX_IDEA_HISTORY, MAX_NOTE_BYTES, MAX_NOTES, MAX_TITLE_CHARS, NOTES_INPUT,
    request_hash,
};
pub use kind::{
    DocumentProfile, KindProfile, Kinds, MEDIA_MANIFEST_TYPE, MediaProfile, PageProfile,
    TextProfile, finding, inspect,
};
pub use record::{
    BuildRecord, BuildStatus, BuilderClaim, BuilderIdentity, InputRef, MAX_DELIVERY_RECORDS,
    MAX_SHOWN_ID_BYTES, Provenance, attribution,
};
pub use request::{
    ARTIFACT_BUILD_CAPABILITY, MAX_INPUTS, MAX_NAME_BYTES, PinnedInput, PinnedRequest, is_token,
    sha256_hex,
};
