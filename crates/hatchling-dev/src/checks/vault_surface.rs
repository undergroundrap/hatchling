//! Vault-surface ratchet: the credential store has no bulk-read API.
//!
//! The peer-authorized-but-unmediated read class: a read surface
//! where list/get/mint operations skipped the `SO_PEERCRED` gate.
//! Hatchling's answer is structural: the vault
//! protocol speaks only single-secret ops, the backend exposes only
//! single-secret methods, and the agent can only reach the store through
//! the broker's grant-checked session. This check fails the gate if any
//! of those invariants is weakened:
//!
//! - `VaultOp` / `VaultAction` gain a variant outside
//!   {Store, Fetch, Remove} (the protocol surface);
//! - `hatchling_vault::Vault` gains a `pub fn` outside
//!   {open, store, fetch, remove, exists} (the backend surface);
//! - `hatchling-agent` names `hatchling-vault` in `[dependencies]` or
//!   `[build-dependencies]` (unmediated store access from the cell).
//!
//! The scan is syntactic (brace-depth enum parsing), not textual
//! substring matching: tests that pin the *rejection* of unknown ops
//! and docs that describe the *absence* of a list API must not trip it.
//!
//! 's `seal.rs` (encryption at rest) is acknowledged and
//! deliberately *outside* this scan: the ratchet keeps scanning
//! `vault.rs` only, whose public surface is untouched by sealing —
//! `SealedVault` composes `Vault` without adding bulk-read methods to
//! it. Migration (`migrate`), the store check (`check`) and rekey (`rekey`) enumerate
//! the store directory, but they are one-shot operator actions that
//! transform files in place and never return secret material to any
//! caller; they are not a runtime read surface, so they do not widen
//! what this ratchet guards.

use std::path::Path;

use super::{Finding, read_toml};

/// The only vault protocol variants. A `List` (or any other bulk op)
/// would reintroduce the unmediated-read class.
const VAULT_OP_VARIANTS: [&str; 3] = ["Store", "Fetch", "Remove"];

/// The only public methods on the vault backend. Single-secret
/// primitives only; the broker decides which of them an agent may call.
const VAULT_METHODS: [&str; 5] = ["open", "store", "fetch", "remove", "exists"];

/// Run the vault-surface ratchet.
///
/// # Errors
///
/// Returns [`Error`](crate::Error) if a manifest or the protocol source
/// cannot be read or parsed.
pub fn run(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let mut findings = Vec::new();
    findings.extend(check_protocol_variants(root)?);
    findings.extend(check_backend_methods(root)?);
    findings.extend(check_agent_isolation(root)?);
    Ok(findings)
}

/// Extract the variant names of `enum <name>` from Rust source by
/// brace-depth tracking. Variant declarations are the `PascalCase`
/// identifiers at depth 1 inside the enum body.
pub(super) fn enum_variants(source: &str, name: &str) -> Vec<String> {
    let mut variants = Vec::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        if !line.contains(&format!("enum {name}")) {
            continue;
        }
        // The opening brace may be on the enum line or the next one.
        // `usize` with saturation: malformed input ends the scan
        // instead of underflowing.
        let mut depth: usize = usize::from(line.contains('{'));
        for line in lines.by_ref() {
            let opens = line.chars().filter(|c| *c == '{').count();
            let closes = line.chars().filter(|c| *c == '}').count();
            depth = depth.saturating_add(opens).saturating_sub(closes);
            if depth == 1 {
                let trimmed = line.trim_start();
                let ident: String = trimmed
                    .chars()
                    .take_while(|c| c.is_alphanumeric())
                    .collect();
                if ident
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_uppercase() && c.is_ascii_alphabetic())
                {
                    variants.push(ident);
                }
            }
            if depth == 0 {
                break;
            }
        }
        break;
    }
    variants
}

/// The protocol enums keep exactly the allowlisted variants.
fn check_protocol_variants(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let path = root
        .join("crates")
        .join("hatchling-core")
        .join("src")
        .join("messages.rs");
    let source = std::fs::read_to_string(&path).map_err(crate::Error::Io)?;
    let mut findings = Vec::new();
    for enum_name in ["VaultOp", "VaultAction"] {
        for variant in enum_variants(&source, enum_name) {
            if !VAULT_OP_VARIANTS.contains(&variant.as_str()) {
                findings.push(Finding::new(
                    "vault_surface",
                    format!(
                        "`{enum_name}` has variant `{variant}` outside the single-secret allowlist {VAULT_OP_VARIANTS:?}; \
                         the vault protocol must not grow a bulk-read op",
                    ),
                ));
            }
        }
    }
    Ok(findings)
}

/// The vault backend keeps exactly the allowlisted public methods.
fn check_backend_methods(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let path = root
        .join("crates")
        .join("hatchling-vault")
        .join("src")
        .join("vault.rs");
    let source = std::fs::read_to_string(&path).map_err(crate::Error::Io)?;
    let mut findings = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("pub fn ") else {
            continue;
        };
        // Identifiers continue through underscores: `dump_all` is one
        // name, not `dump`.
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !VAULT_METHODS.contains(&name.as_str()) {
            findings.push(Finding::new(
                "vault_surface",
                format!(
                    "`hatchling_vault::Vault` has public method `{name}` outside the single-secret allowlist {VAULT_METHODS:?}; \
                     the backend must not grow a bulk-read method",
                ),
            ));
        }
    }
    Ok(findings)
}

/// The agent reaches the vault only through the broker's session: it
/// must not depend on `hatchling-vault` in a runtime dependency
/// section. (Dev-dependencies are test-only and stay allowed.)
fn check_agent_isolation(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let manifest = root
        .join("crates")
        .join("hatchling-agent")
        .join("Cargo.toml");
    let table = read_toml(&manifest)?;
    let mut findings = Vec::new();
    for section in ["dependencies", "build-dependencies"] {
        if table
            .get(section)
            .and_then(|s| s.as_table())
            .is_some_and(|deps| deps.contains_key("hatchling-vault"))
        {
            findings.push(Finding::new(
                "vault_surface",
                format!(
                    "crate `hatchling-agent` lists `hatchling-vault` under [{section}]; \
                     the agent must reach secrets only through the broker's grant-checked session",
                ),
            ));
        }
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tree(root: &Path, messages: &str, vault: &str, agent_deps: &str) {
        let core_src = root.join("crates/hatchling-core/src");
        let vault_src = root.join("crates/hatchling-vault/src");
        let agent_dir = root.join("crates/hatchling-agent");
        std::fs::create_dir_all(&core_src).unwrap();
        std::fs::create_dir_all(&vault_src).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(core_src.join("messages.rs"), messages).unwrap();
        std::fs::write(vault_src.join("vault.rs"), vault).unwrap();
        std::fs::write(
            agent_dir.join("Cargo.toml"),
            format!("[package]\nname = \"hatchling-agent\"\n{agent_deps}"),
        )
        .unwrap();
    }

    const CLEAN_MESSAGES: &str = r"
pub enum VaultOp {
    Store { name: String, secret_b64: String },
    Fetch { name: String },
    Remove { name: String },
}
pub enum VaultAction {
    Store,
    Fetch,
    Remove,
}
";

    const CLEAN_VAULT: &str = r"
impl Vault {
    pub fn open() {}
    pub fn store() {}
    pub fn fetch() {}
    pub fn remove() {}
    pub fn exists() {}
}
";

    const CLEAN_AGENT: &str = "[dependencies]\nhatchling-core = \"0.1.0\"\n";

    fn synthetic_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-dev-vaultsurf-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_tree(&root, CLEAN_MESSAGES, CLEAN_VAULT, CLEAN_AGENT);
        root
    }

    #[test]
    fn clean_tree_passes() {
        let root = synthetic_root("clean");
        assert!(run(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn enum_variants_ignores_non_enum_blocks() {
        // Pins the `!` on the enum-line scan guard: deleting it starts
        // the scan at the first non-enum line, so a PascalCase ident in
        // a preceding struct leaks in as a "variant".
        let source = "pub struct Cfg {\n    Path: String,\n}\npub enum VaultOp {\n    Store,\n}\n";
        assert_eq!(enum_variants(source, "VaultOp"), vec!["Store"]);
    }

    #[test]
    fn enum_variants_rejects_lowercase_idents() {
        // Pins `&&` on the ident filter: `||` admits any ASCII
        // alphabetic ident, so a lowercase field at depth 1 leaks in
        // as a "variant".
        let source = "pub enum VaultOp {\n    Store,\n    store_helper: u8,\n}\n";
        assert_eq!(enum_variants(source, "VaultOp"), vec!["Store"]);
    }

    #[test]
    fn catches_list_variant() {
        // A bulk op in the protocol is exactly what the ratchet exists
        // to forbid. The rejection test in messages.rs (which mentions
        // "list" in a string literal) must NOT trip this — only a real
        // variant does.
        let root = synthetic_root("list-variant");
        let with_list = CLEAN_MESSAGES.replace(
            "    Remove { name: String },\n}",
            "    Remove { name: String },\n    List,\n}",
        );
        write_tree(&root, &with_list, CLEAN_VAULT, CLEAN_AGENT);
        let findings = run(&root).unwrap();
        assert!(
            findings.iter().any(|f| f.message.contains("`List`")),
            "expected a List finding, got {findings:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn catches_bulk_backend_method() {
        let root = synthetic_root("bulk-method");
        let with_dump = CLEAN_VAULT.replace(
            "    pub fn exists() {}",
            "    pub fn exists() {}\n    pub fn dump_all() {}",
        );
        write_tree(&root, CLEAN_MESSAGES, &with_dump, CLEAN_AGENT);
        let findings = run(&root).unwrap();
        assert!(
            findings.iter().any(|f| f.message.contains("`dump_all`")),
            "expected a dump_all finding, got {findings:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn catches_agent_vault_dependency() {
        let root = synthetic_root("agent-dep");
        let agent_deps = "[dependencies]\nhatchling-vault = \"0.1.0\"\n";
        write_tree(&root, CLEAN_MESSAGES, CLEAN_VAULT, agent_deps);
        let findings = run(&root).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("hatchling-agent")),
            "expected an agent-isolation finding, got {findings:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejection_tests_do_not_trip_the_ratchet() {
        // The real messages.rs contains string literals like
        // `{"op":"list"}` in tests pinning the rejection of unknown
        // ops. The ratchet parses enum variants, not text, so those
        // are invisible to it. Prove it with a literal in the tree.
        let root = synthetic_root("rejection-text");
        let with_test = format!(
            "{CLEAN_MESSAGES}\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn rejects_list() {{\n        let json = br#\"{{\"op\":\"list\",\"name\":\"x\"}}\"#;\n        assert!(json.len() > 0);\n    }}\n}}\n"
        );
        write_tree(&root, &with_test, CLEAN_VAULT, CLEAN_AGENT);
        assert!(run(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
