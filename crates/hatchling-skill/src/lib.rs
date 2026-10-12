//! Signed skills: the trust core.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! Every skill is signed, sandboxed, and capability-scoped. An unsigned
//! skill, a tampered skill, or a skill asking for capabilities it wasn't
//! granted does not run — fail-closed, structurally, not by convention.
//!
//! This crate implements the signing + manifest + admission
//! trust core. The broker verifies signatures before admission; the
//! cell never self-attests.

mod error;
mod manifest;
mod registry;
mod verify;

pub use error::Error;
pub use manifest::{Manifest, SIGNING_CONTEXT, SignedManifest};
pub use registry::{CodeHash, RegisteredSkill, SkillId, SkillRegistry};
pub use verify::{Admission, check_trust_root, verify_admission};
