//! The audit checks. Each check is a pure function over the workspace
//! root returning its findings; an empty list means the check passed.
//!
//! The fast checks also run as unit tests in this module, so they
//! execute inside the normal per-push gate (`cargo test --workspace`).
//! The slow checks (mutation testing, coverage) are only reachable
//! through the `hatchling-dev` binary.

pub mod arch;
pub mod coverage;
pub mod dep_direction;
pub mod drift;
pub mod driver;
pub mod mutant_cache;
pub mod mutants;
pub mod pins;
pub mod preflight;
pub mod protocol_schema;
pub mod sha256;
pub mod space_surface;
pub mod vault_surface;
pub mod verdict;

use std::path::Path;

/// One violation found by a check. Checks report violations as
/// findings; [`Error`](crate::Error) is reserved for when a check
/// could not run at all.
#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    /// Which check produced this finding.
    pub check: &'static str,
    /// Human-readable description of the violation.
    pub message: String,
}

impl Finding {
    /// Build a finding for the given check.
    #[must_use]
    pub const fn new(check: &'static str, message: String) -> Self {
        Self { check, message }
    }
}

/// Manifest of a workspace member crate.
#[derive(Debug)]
pub struct Member {
    /// Crate name from its `Cargo.toml`.
    pub name: String,
    /// Directory containing its `Cargo.toml`.
    pub dir: std::path::PathBuf,
}

/// Read every workspace member's manifest. Members are the
/// directories directly under `crates/` containing a `Cargo.toml`.
///
/// # Errors
///
/// Returns [`Error::Io`](crate::Error::Io) if the `crates/` directory
/// cannot be read, and [`Error::Parse`](crate::Error::Parse) if a
/// member manifest is not valid TOML or lacks `package.name`.
pub fn members(root: &Path) -> Result<Vec<Member>, crate::Error> {
    let crates_dir = root.join("crates");
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&crates_dir).map_err(crate::Error::Io)?;
    for entry in entries {
        let entry = entry.map_err(crate::Error::Io)?;
        let manifest = entry.path().join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest).map_err(crate::Error::Io)?;
        let table: toml::Table =
            text.parse()
                .map_err(|e: toml::de::Error| crate::Error::Parse {
                    path: manifest.clone(),
                    reason: e.to_string(),
                })?;
        let name = table
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .ok_or_else(|| crate::Error::Parse {
                path: manifest.clone(),
                reason: String::from("missing package.name"),
            })?;
        out.push(Member {
            name: name.to_owned(),
            dir: entry.path(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Read and parse a TOML file.
///
/// # Errors
///
/// Returns [`Error::MissingFile`](crate::Error::MissingFile) if the
/// path does not exist, [`Error::Io`](crate::Error::Io) if it cannot
/// be read, and [`Error::Parse`](crate::Error::Parse) if it is not
/// valid TOML.
pub fn read_toml(path: &Path) -> Result<toml::Table, crate::Error> {
    if !path.is_file() {
        return Err(crate::Error::MissingFile(path.to_owned()));
    }
    let text = std::fs::read_to_string(path).map_err(crate::Error::Io)?;
    text.parse()
        .map_err(|e: toml::de::Error| crate::Error::Parse {
            path: path.to_owned(),
            reason: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_root;

    #[test]
    fn fast_checks_pass_on_this_tree() {
        let root = workspace_root();
        for (name, findings) in [
            ("dep_direction", dep_direction::run(&root).unwrap()),
            ("pins", pins::run(&root).unwrap()),
            ("arch", arch::run(&root).unwrap()),
            ("drift", drift::run(&root).unwrap()),
            ("vault_surface", vault_surface::run(&root).unwrap()),
            ("space_surface", space_surface::run(&root).unwrap()),
        ] {
            assert!(
                findings.is_empty(),
                "check {name} failed: {:?}",
                findings
                    .iter()
                    .map(|f| f.message.as_str())
                    .collect::<Vec<_>>()
            );
        }
    }
}
