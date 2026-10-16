//! Architecture-conformance checks: the conventions from `AGENTS.md`
//! that a compiler cannot enforce on its own.
//!
//! - `main.rs` stays thin (the thin-binary-edge rule);
//! - the strict lint set is still intact (weakening it must be a
//!   deliberate, reviewed act, not drift);
//! - every dependency was explicitly reviewed
//!   (`docs/dependency-review.md`);
//! - prompt text lives only in `hatchling-prompt` (the pack stays in
//!   one auditable place);
//! - the frontend host, `hatchling-host`, stays a thin transport
//!   adapter: no dependencies beyond the wire contract and no
//!   authorization logic. The broker owns every authorization decision;
//! - every crate has a row in the module map
//!   (`docs/arch/module-map.md`), and the map names no crate that
//!   doesn't exist;
//! - the crate that runs in a cell, `hatchling-agent`, never depends on
//!   a crate that holds keys, the network or the cell machinery, not
//!   even through another crate.

use std::path::Path;

use super::{Finding, members, read_toml};
use crate::config::MAIN_RS_BUDGET;

/// Restriction lints that must stay at `deny` in
/// `[workspace.lints.clippy]`. (The `rust` table is checked separately.)
const REQUIRED_DENY_LINTS: [&str; 13] = [
    "unwrap_used",
    "expect_used",
    "panic",
    "todo",
    "unimplemented",
    "dbg_macro",
    "print_stdout",
    "print_stderr",
    "indexing_slicing",
    "as_conversions",
    "wildcard_enum_match_arm",
    "undocumented_unsafe_blocks",
    "exit",
];

/// Run all architecture-conformance checks.
///
/// # Errors
///
/// Returns [`Error`](crate::Error) if a manifest cannot be read or
/// parsed, or if the review document cannot be read.
pub fn run(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let mut findings = Vec::new();
    findings.extend(check_main_thin(root)?);
    findings.extend(check_lint_set(root)?);
    findings.extend(check_dep_review(root)?);
    findings.extend(check_prompt_placement(root)?);
    findings.extend(check_adapter_thin(root)?);
    findings.extend(check_api_adapter(root)?);
    findings.extend(check_module_map(root)?);
    findings.extend(check_cell_side(root)?);
    Ok(findings)
}

/// The architecture map: every workspace crate has a row in it.
pub const MODULE_MAP: &str = "docs/arch/module-map.md";

/// The crate that runs inside a cell.
pub const CELL_CRATE: &str = "hatchling-agent";

/// Crates that hold keys, the network or the cell machinery. The cell
/// crate must never depend on one, directly or through another crate.
pub const TRUSTED_SIDE: [&str; 6] = [
    "hatchling-broker",
    "hatchling-vault",
    "hatchling-egress",
    "hatchling-isolation",
    "hatchling-fido2",
    "hatchling-runtime",
];

/// Every member crate has a row in the module map, and every row names
/// a member crate: a new crate can't go undocumented, and a removed
/// one can't linger in the map.
fn check_module_map(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let path = root.join(MODULE_MAP);
    if !path.is_file() {
        return Ok(vec![Finding::new(
            "arch",
            format!("{MODULE_MAP} is missing"),
        )]);
    }
    let map = std::fs::read_to_string(&path).map_err(crate::Error::Io)?;
    let rows = map_rows(&map);
    let members = members(root)?;
    let mut findings = Vec::new();
    for member in &members {
        if !rows.contains(&member.name.as_str()) {
            findings.push(Finding::new(
                "arch",
                format!("crate `{}` has no row in {MODULE_MAP}", member.name),
            ));
        }
    }
    for row in rows {
        if !members.iter().any(|member| member.name == row) {
            findings.push(Finding::new(
                "arch",
                format!("{MODULE_MAP} has a row for `{row}`, which is not a workspace crate"),
            ));
        }
    }
    Ok(findings)
}

/// The crate names the module map's table rows start with: lines of the
/// form ``| `name` | role |``.
fn map_rows(map: &str) -> Vec<&str> {
    map.lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once('`'))
        .map(|(name, _)| name)
        .collect()
}

/// The cell crate's dependencies, followed through every member crate,
/// never reach the trusted side. Dev-dependencies don't count: a test may
/// stand up a broker.
fn check_cell_side(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let mut graph = std::collections::BTreeMap::new();
    for member in members(root)? {
        let table = read_toml(&member.dir.join("Cargo.toml"))?;
        let deps: Vec<String> = ["dependencies", "build-dependencies"]
            .iter()
            .filter_map(|section| table.get(*section).and_then(|deps| deps.as_table()))
            .flat_map(|deps| deps.keys().cloned())
            .collect();
        graph.insert(member.name, deps);
    }
    let mut findings = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    // Each entry: a crate, and the chain of dependencies that reached it.
    let mut stack = vec![vec![CELL_CRATE.to_owned()]];
    while let Some(chain) = stack.pop() {
        let Some(name) = chain.last() else {
            continue;
        };
        if !seen.insert(name.clone()) {
            continue;
        }
        if TRUSTED_SIDE.contains(&name.as_str()) {
            findings.push(Finding::new(
                "arch",
                format!(
                    "`{CELL_CRATE}` depends on `{name}`, which holds keys, the network or the cell machinery: {}",
                    chain.join(" -> ")
                ),
            ));
            continue;
        }
        for dep in graph.get(name).into_iter().flatten() {
            let mut next = chain.clone();
            next.push(dep.clone());
            stack.push(next);
        }
    }
    Ok(findings)
}

/// The frontend host crate: the thin transport adapter between
/// the UI and the broker's UI socket. This check holds it thin.
pub const ADAPTER_CRATE: &str = "hatchling-host";

/// The local API's adapter: a module of the broker, held to the
/// frontend host's rule. The broker's `api_auth` names callers and
/// `api_ops` decides their actions; the adapter only transports.
pub const API_ADAPTER: &str = "crates/hatchling-broker/src/api.rs";

/// What the frontend host may depend on: the wire contract and its
/// serialization, errors and logs, and the two pieces of the WebSocket
/// opening handshake's accept key (`sha1`, `base64`; WO-021). Anything
/// more is a reviewed change to this list, not drift.
const ADAPTER_DEPENDENCIES: [&str; 7] = [
    "hatchling-core",
    "serde",
    "serde_json",
    "thiserror",
    "tracing",
    "sha1",
    "base64",
];

/// Words that name authorization. In the frontend host, code that uses
/// one is deciding instead of transporting.
const AUTHORIZATION_WORDS: [&str; 14] = [
    "authoriz",
    "authoris",
    "authentic",
    "permission",
    "privilege",
    "credential",
    "allowlist",
    "allow_list",
    "denylist",
    "deny_list",
    "access_control",
    "accesscontrol",
    "is_allowed",
    "can_access",
];

/// Whether `word`, one identifier, names authorization.
fn names_authorization(word: &str) -> bool {
    let word = word.to_ascii_lowercase();
    AUTHORIZATION_WORDS.iter().any(|stem| word.contains(stem))
        || word == "acl"
        || word == "acls"
        || word.starts_with("acl_")
        || word.ends_with("_acl")
}

/// The frontend host stays a thin transport adapter: only the allowed
/// dependencies, and no authorization vocabulary in its code (comment
/// lines aside).
fn check_adapter_thin(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let mut findings = Vec::new();
    for member in members(root)? {
        if member.name != ADAPTER_CRATE {
            continue;
        }
        let table = read_toml(&member.dir.join("Cargo.toml"))?;
        for section in ["dependencies", "build-dependencies"] {
            let Some(deps) = table.get(section).and_then(|s| s.as_table()) else {
                continue;
            };
            for dep in deps
                .keys()
                .filter(|dep| !ADAPTER_DEPENDENCIES.contains(&dep.as_str()))
            {
                findings.push(Finding::new(
                    "arch",
                    format!(
                        "`{ADAPTER_CRATE}` [{section}] has `{dep}`: the frontend host is a thin transport adapter and depends only on {ADAPTER_DEPENDENCIES:?}"
                    ),
                ));
            }
        }
        let mut files = Vec::new();
        collect_rs(&member.dir.join("src"), &mut files)?;
        files.sort();
        for file in files {
            findings.extend(authorization_lines(
                root,
                &file,
                &format!("`{ADAPTER_CRATE}`"),
                "authorization belongs to the broker; the frontend host only transports ",
            )?);
        }
    }
    Ok(findings)
}

/// The local API's adapter stays a transport: no authorization
/// vocabulary in its code (comment lines aside), wherever it is used.
/// A tree without the adapter has nothing to check.
fn check_api_adapter(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let file = root.join(API_ADAPTER);
    if !file.is_file() {
        return Ok(Vec::new());
    }
    authorization_lines(
        root,
        &file,
        "the local API's adapter",
        "authorization belongs to the broker's `api_auth` and `api_ops`; the API's adapter only transports ",
    )
}

/// Each line of `file` whose code names authorization (comment lines
/// aside): a finding saying the word is in `owner`, and `why` it does
/// not belong there.
fn authorization_lines(
    root: &Path,
    file: &Path,
    owner: &str,
    why: &str,
) -> Result<Vec<Finding>, crate::Error> {
    let text = std::fs::read_to_string(file).map_err(crate::Error::Io)?;
    let mut findings = Vec::new();
    for (number, line) in (1..).zip(text.lines()) {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let mut words = line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
        if let Some(word) = words.find(|word| names_authorization(word)) {
            findings.push(Finding::new(
                "arch",
                format!(
                    "{}:{number}: `{word}` in {owner}: {why}",
                    file.strip_prefix(root).unwrap_or(file).display(),
                ),
            ));
        }
    }
    Ok(findings)
}

/// Every binary entry point stays under the line budget.
fn check_main_thin(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let mut findings = Vec::new();
    for member in members(root)? {
        let main = member.dir.join("src").join("main.rs");
        if !main.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&main).map_err(crate::Error::Io)?;
        let lines = text.lines().count();
        if lines > MAIN_RS_BUDGET {
            findings.push(Finding::new(
                "arch",
                format!(
                    "crate `{}`: src/main.rs is {lines} lines (budget {MAIN_RS_BUDGET}); move logic into the library",
                    member.name,
                ),
            ));
        }
    }
    Ok(findings)
}

/// The strict lint set has not been weakened.
fn check_lint_set(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let mut findings = Vec::new();
    let manifest = read_toml(&root.join("Cargo.toml"))?;
    let lints = manifest
        .get("workspace")
        .and_then(|w| w.get("lints"))
        .and_then(|l| l.as_table());

    let Some(lints) = lints else {
        findings.push(Finding::new(
            "arch",
            String::from("workspace Cargo.toml has no [workspace.lints] table"),
        ));
        return Ok(findings);
    };

    let level_of = |table: &str, lint: &str| -> Option<String> {
        let entry = lints.get(table)?.get(lint)?;
        if let Some(level) = entry.as_str() {
            return Some(level.to_owned());
        }
        entry
            .get("level")
            .and_then(|l| l.as_str())
            .map(str::to_owned)
    };

    if level_of("rust", "unsafe_code").as_deref() != Some("deny") {
        findings.push(Finding::new(
            "arch",
            String::from("[workspace.lints.rust] unsafe_code is not \"deny\""),
        ));
    }
    for lint in REQUIRED_DENY_LINTS {
        if level_of("clippy", lint).as_deref() != Some("deny") {
            findings.push(Finding::new(
                "arch",
                format!("[workspace.lints.clippy] {lint} is not \"deny\""),
            ));
        }
    }
    Ok(findings)
}

/// Every dependency in every member manifest has a review entry.
fn check_dep_review(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let mut findings = Vec::new();
    let review_path = root.join("docs").join("dependency-review.md");
    let review = if review_path.is_file() {
        std::fs::read_to_string(&review_path).map_err(crate::Error::Io)?
    } else {
        findings.push(Finding::new(
            "arch",
            String::from("docs/dependency-review.md is missing"),
        ));
        return Ok(findings);
    };

    for member in members(root)? {
        let manifest = member.dir.join("Cargo.toml");
        let table = read_toml(&manifest)?;
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let Some(deps) = table.get(section).and_then(|s| s.as_table()) else {
                continue;
            };
            for dep in deps.keys() {
                let heading = format!("## {dep}");
                if !review.lines().any(|line| line.trim() == heading) {
                    findings.push(Finding::new(
                        "arch",
                        format!(
                            "dependency `{dep}` (crate `{}`, [{section}]) has no `## {dep}` entry in docs/dependency-review.md",
                            member.name,
                        ),
                    ));
                }
            }
        }
    }
    Ok(findings)
}

/// Prompt prose lives only in `hatchling-prompt`.
///
/// Distinctive section markers must not appear in any other member's
/// sources: the pack stays in one auditable place, and the
/// change-detector test in `hatchling-prompt` remains the single
/// gate for prompt edits.
fn check_prompt_placement(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    // Markers are full phrases from the pack's prose — long enough
    // that test doubles (which use different wording) don't trip the
    // check on the dev crate's own test code. They are assembled with
    // `concat!` from fragments so this file itself never contains the
    // full phrases: the check scans every member's `src/`, including
    // this one.
    const MARKERS: [&str; 2] = [
        concat!("You are Hatchling, a capable ", "personal agent"),
        concat!("Instruction precedence, ", "highest to lowest"),
    ];
    let mut findings = Vec::new();
    for member in members(root)? {
        if member.name == "hatchling-prompt" {
            continue;
        }
        let src = member.dir.join("src");
        let mut files = Vec::new();
        collect_rs(&src, &mut files)?;
        for file in files {
            let text = std::fs::read_to_string(&file).map_err(crate::Error::Io)?;
            for marker in MARKERS {
                if text.contains(marker) {
                    findings.push(Finding::new(
                        "arch",
                        format!(
                            "crate `{}`: prompt prose belongs in `hatchling-prompt` (found {marker:?} in {})",
                            member.name,
                            file.strip_prefix(root).unwrap_or(&file).display(),
                        ),
                    ));
                }
            }
        }
    }
    Ok(findings)
}

/// Collect every `.rs` file under `dir`, recursively.
fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), crate::Error> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(crate::Error::Io)?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("hatchling-dev-arch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/foo/src")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        // A lint set matching the real workspace: everything the check
        // requires, so only the injected violation fires.
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\n[workspace.lints.rust]\nunsafe_code = \"deny\"\n\
             [workspace.lints.clippy]\nunwrap_used = \"deny\"\nexpect_used = \"deny\"\n\
             panic = \"deny\"\ntodo = \"deny\"\nunimplemented = \"deny\"\n\
             dbg_macro = \"deny\"\nprint_stdout = \"deny\"\nprint_stderr = \"deny\"\n\
             indexing_slicing = \"deny\"\nas_conversions = \"deny\"\n\
             wildcard_enum_match_arm = \"deny\"\nundocumented_unsafe_blocks = \"deny\"\n\
             exit = \"deny\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("docs/dependency-review.md"),
            "# Dependency review\n\n## serde\n\nReviewed.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             [dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("docs/arch")).unwrap();
        std::fs::write(
            root.join(MODULE_MAP),
            "# Module map\n\n| Crate | Role |\n| --- | --- |\n| `foo` | The only crate. |\n",
        )
        .unwrap();
        root
    }

    /// Adds a member crate with these `[dependencies]`,
    /// `[build-dependencies]` and `[dev-dependencies]` lines.
    fn add_crate(root: &Path, name: &str, deps: &str, build: &str, dev: &str) {
        let dir = root.join("crates").join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [dependencies]\n{deps}[build-dependencies]\n{build}[dev-dependencies]\n{dev}"
            ),
        )
        .unwrap();
    }

    fn messages(findings: &[Finding]) -> Vec<String> {
        findings.iter().map(|f| f.message.clone()).collect()
    }

    #[test]
    fn map_rows_reads_the_first_cell_of_each_table_row() {
        let map = "# Map\n\nText naming `not-a-row`.\n| Crate | Role |\n| --- | --- |\n\
                   | `alpha` | First. |\n| `beta-gamma` | Second, see `alpha`. |\n";
        assert_eq!(map_rows(map), ["alpha", "beta-gamma"]);
    }

    #[test]
    fn the_module_map_lists_every_crate_and_only_real_ones() {
        let root = synthetic_root("module-map");
        assert!(check_module_map(&root).unwrap().is_empty());

        add_crate(&root, "bar", "", "", "");
        let found = messages(&check_module_map(&root).unwrap());
        assert_eq!(found, [format!("crate `bar` has no row in {MODULE_MAP}")]);

        std::fs::write(
            root.join(MODULE_MAP),
            "| `foo` | One. |\n| `bar` | Two. |\n| `ghost` | Gone. |\n",
        )
        .unwrap();
        let found = messages(&check_module_map(&root).unwrap());
        assert_eq!(
            found,
            [format!(
                "{MODULE_MAP} has a row for `ghost`, which is not a workspace crate"
            )]
        );

        std::fs::remove_file(root.join(MODULE_MAP)).unwrap();
        let found = messages(&check_module_map(&root).unwrap());
        assert_eq!(found, [format!("{MODULE_MAP} is missing")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cell_crate_reaching_the_trusted_side_is_caught_with_its_path() {
        let root = synthetic_root("cell-side-indirect");
        add_crate(&root, CELL_CRATE, "middle = \"1\"\n", "", "");
        add_crate(&root, "middle", "hatchling-vault = \"1\"\n", "", "");
        add_crate(&root, "hatchling-vault", "", "", "");
        let found = messages(&check_cell_side(&root).unwrap());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found
                .iter()
                .all(|m| m.ends_with("hatchling-agent -> middle -> hatchling-vault")),
            "{found:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_trusted_crate_is_caught_directly_and_through_build_dependencies() {
        for side_crate in TRUSTED_SIDE {
            let root = synthetic_root(&format!("cell-side-{side_crate}"));
            add_crate(
                &root,
                CELL_CRATE,
                &format!("{side_crate} = \"1\"\n"),
                "",
                "",
            );
            let found = messages(&check_cell_side(&root).unwrap());
            assert_eq!(found.len(), 1, "{side_crate}: {found:?}");
            let _ = std::fs::remove_dir_all(&root);

            let root = synthetic_root(&format!("cell-side-build-{side_crate}"));
            add_crate(
                &root,
                CELL_CRATE,
                "",
                &format!("{side_crate} = \"1\"\n"),
                "",
            );
            let found = messages(&check_cell_side(&root).unwrap());
            assert_eq!(found.len(), 1, "build-dependency {side_crate}: {found:?}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn dev_dependencies_and_unrelated_crates_are_allowed() {
        let root = synthetic_root("cell-side-allowed");
        add_crate(
            &root,
            CELL_CRATE,
            "middle = \"1\"\nserde = \"1\"\n",
            "",
            "hatchling-broker = \"1\"\n",
        );
        add_crate(&root, "middle", "hatchling-core = \"1\"\n", "", "");
        add_crate(&root, "hatchling-core", "", "", "");
        // The broker may depend on anything; only the cell crate is held.
        add_crate(
            &root,
            "hatchling-broker",
            "hatchling-vault = \"1\"\n",
            "",
            "",
        );
        assert!(check_cell_side(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_shared_dependency_is_reported_once() {
        // Two paths to the same trusted crate: one finding, for the
        // first path the walk takes.
        let root = synthetic_root("cell-side-once");
        add_crate(&root, CELL_CRATE, "left = \"1\"\nright = \"1\"\n", "", "");
        add_crate(&root, "left", "hatchling-egress = \"1\"\n", "", "");
        add_crate(&root, "right", "hatchling-egress = \"1\"\n", "", "");
        let found = messages(&check_cell_side(&root).unwrap());
        assert_eq!(found.len(), 1, "{found:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn main_rs_at_exactly_the_budget_passes() {
        // Boundary: MAIN_RS_BUDGET lines is allowed; the check is `>`,
        // not `>=`. A `>=` mutant would flag this file.
        let root = synthetic_root("budget-main");
        let at_budget = (0..MAIN_RS_BUDGET)
            .map(|i| format!("// line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(root.join("crates/foo/src/main.rs"), at_budget).unwrap();

        let findings = run(&root).unwrap();
        assert!(
            !findings.iter().any(|f| f.message.contains("main.rs")),
            "a main.rs at exactly the budget must pass, got {findings:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn catches_fat_main_rs() {
        let root = synthetic_root("fat-main");
        let long_main = (0..=MAIN_RS_BUDGET)
            .map(|i| format!("// line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(root.join("crates/foo/src/main.rs"), long_main).unwrap();

        let findings = run(&root).unwrap();
        assert!(
            findings.iter().any(|f| f.message.contains("main.rs")),
            "expected a main.rs finding, got {findings:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn catches_unreviewed_dependency() {
        let root = synthetic_root("unreviewed-dep");
        std::fs::write(
            root.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             [dependencies]\nleftpad = \"1\"\n",
        )
        .unwrap();

        let findings = run(&root).unwrap();
        assert!(
            findings.iter().any(|f| f.message.contains("leftpad")),
            "expected an unreviewed-dep finding, got {findings:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn catches_weakened_lint() {
        // check_lint_set must report a required deny lint that was
        // weakened. A `check_lint_set -> Ok(vec![])` mutant reports
        // nothing and fails here.
        let root = synthetic_root("weak-lint");
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let weakened = manifest.replace("unwrap_used = \"deny\"", "unwrap_used = \"warn\"");
        assert_ne!(manifest, weakened, "test setup: lint not replaced");
        std::fs::write(root.join("Cargo.toml"), weakened).unwrap();

        let findings = run(&root).unwrap();
        assert!(
            findings.iter().any(|f| f.message.contains("unwrap_used")),
            "expected an unwrap_used finding, got {findings:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_synthetic_tree_passes() {
        let root = synthetic_root("clean");
        assert!(run(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn catches_prompt_prose_outside_the_prompt_crate() {
        let root = synthetic_root("prompt-leak");
        // The leaked prose is assembled with `concat!` so this test
        // file itself never contains the full marker phrase (the
        // check scans this file too).
        std::fs::write(
            root.join("crates/foo/src/lib.rs"),
            concat!(
                "pub fn system() -> &'static str { \"",
                "You are Hatchling, a capable ",
                "personal agent (leaked).\" }\n",
            ),
        )
        .unwrap();

        let findings = run(&root).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("belongs in `hatchling-prompt`")),
            "expected a prompt-placement finding, got {findings:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn host(root: &Path, manifest_deps: &str, source: &str) {
        std::fs::create_dir_all(root.join("crates/hatchling-host/src")).unwrap();
        std::fs::write(
            root.join("crates/hatchling-host/Cargo.toml"),
            format!(
                "[package]\nname = \"hatchling-host\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [dependencies]\n{manifest_deps}"
            ),
        )
        .unwrap();
        std::fs::write(root.join("crates/hatchling-host/src/lib.rs"), source).unwrap();
    }

    fn adapter_findings(root: &Path) -> Vec<String> {
        check_adapter_thin(root)
            .unwrap()
            .into_iter()
            .map(|finding| finding.message)
            .collect()
    }

    #[test]
    fn a_thin_frontend_host_passes() {
        let root = synthetic_root("host-thin");
        host(
            &root,
            "hatchling-core = \"0.1\"\nserde = \"1\"\nserde_json = \"1\"\nthiserror = \"2\"\ntracing = \"0.1\"\nsha1 = \"0.10\"\nbase64 = \"0.22\"\n",
            "// No authorization here: the broker decides.\npub fn relay(frame: &[u8]) -> Vec<u8> { frame.to_vec() }\npub const ORACLE: u8 = 1;\n",
        );
        assert!(adapter_findings(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_frontend_host_with_its_own_dependencies_or_authorization_fails() {
        let root = synthetic_root("host-fat");
        host(
            &root,
            "hatchling-core = \"0.1\"\nhatchling-vault = \"0.1\"\n",
            "pub fn relay() {}\npub fn authorize_frame() -> bool { true }\nconst PERMISSIONS: u8 = 1;\nfn check_acl() {}\nfn acl_for() {}\nstatic ACLS: u8 = 0;\n",
        );
        let findings = adapter_findings(&root);
        assert_eq!(findings.len(), 6, "{findings:?}");
        assert!(
            findings
                .first()
                .is_some_and(|f| f.contains("[dependencies] has `hatchling-vault`")),
            "{findings:?}"
        );
        for (finding, (line, word)) in findings.iter().skip(1).zip([
            (2, "authorize_frame"),
            (3, "PERMISSIONS"),
            (4, "check_acl"),
            (5, "acl_for"),
            (6, "ACLS"),
        ]) {
            let expected = format!("lib.rs:{line}: `{word}` in `hatchling-host`");
            assert!(finding.contains(&expected), "{finding} vs {expected}");
        }
        // Wired into the arch run.
        let all = run(&root).unwrap();
        assert!(all.iter().any(|f| f.message.contains("hatchling-host")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_local_apis_adapter_only_transports() {
        let root = synthetic_root("api-adapter");
        assert!(
            check_api_adapter(&root).unwrap().is_empty(),
            "no adapter, nothing to check"
        );
        let adapter = root.join(API_ADAPTER);
        std::fs::create_dir_all(adapter.parent().unwrap()).unwrap();
        std::fs::write(
            &adapter,
            "//! Who may call is the broker's: authorization lives in api_auth.\npub fn relay(frame: &[u8]) -> Vec<u8> { frame.to_vec() }\nfn admit_caller() {}\n",
        )
        .unwrap();
        assert!(
            check_api_adapter(&root).unwrap().is_empty(),
            "comments may name it; code may transport"
        );
        std::fs::write(
            &adapter,
            "pub fn relay() {}\nfn peer_credentials() {}\nlet authorized = true;\n",
        )
        .unwrap();
        let findings: Vec<String> = check_api_adapter(&root)
            .unwrap()
            .into_iter()
            .map(|finding| finding.message)
            .collect();
        assert_eq!(
            findings,
            [
                "crates/hatchling-broker/src/api.rs:2: `peer_credentials` in the local API's adapter: authorization belongs to the broker's `api_auth` and `api_ops`; the API's adapter only transports ",
                "crates/hatchling-broker/src/api.rs:3: `authorized` in the local API's adapter: authorization belongs to the broker's `api_auth` and `api_ops`; the API's adapter only transports ",
            ]
        );
        // Wired into the arch run.
        let all = run(&root).unwrap();
        assert!(
            all.iter()
                .any(|f| f.message.contains("the local API's adapter"))
        );
        // The words are the frontend host's, with its own message.
        let host_file = root.join("lib.rs");
        std::fs::write(&host_file, "fn x() {}\nfn privileged() {}\n").unwrap();
        let host = authorization_lines(&root, &host_file, "`hatchling-host`", "why").unwrap();
        assert_eq!(host.len(), 1);
        assert!(
            host.first()
                .is_some_and(|f| f.message == "lib.rs:2: `privileged` in `hatchling-host`: why")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_frontend_host_build_dependency_counts_too() {
        let root = synthetic_root("host-build-dep");
        host(
            &root,
            "serde = \"1\"\n\n[build-dependencies]\ncc = \"1\"\n",
            "",
        );
        let findings = adapter_findings(&root);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings
                .first()
                .is_some_and(|f| f.contains("[build-dependencies] has `cc`")),
            "{findings:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn authorization_words_are_matched_as_whole_identifiers() {
        for word in [
            "authorize",
            "Authorised",
            "authenticated",
            "permissions",
            "privilege",
            "Credentials",
            "allowlist",
            "deny_list",
            "AccessControl",
            "is_allowed",
            "can_access",
            "acl",
            "ACLS",
            "acl_entry",
            "user_acl",
        ] {
            assert!(names_authorization(word), "{word}");
        }
        for word in [
            "oracle", "miracle", "relay", "decide", "aclx", "xacl", "grant", "frame",
        ] {
            assert!(!names_authorization(word), "{word}");
        }
    }

    #[test]
    fn prompt_crate_itself_is_exempt() {
        let root = synthetic_root("prompt-home");
        std::fs::create_dir_all(root.join("crates/hatchling-prompt/src")).unwrap();
        std::fs::write(
            root.join("crates/hatchling-prompt/Cargo.toml"),
            "[package]\nname = \"hatchling-prompt\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/hatchling-prompt/src/lib.rs"),
            concat!(
                "pub const S: &str = \"",
                "You are Hatchling, a capable ",
                "personal agent (home).\";\n",
            ),
        )
        .unwrap();

        let findings = run(&root).unwrap();
        assert!(
            !findings
                .iter()
                .any(|f| f.message.contains("belongs in `hatchling-prompt`")),
            "the prompt crate itself must pass, got {findings:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
