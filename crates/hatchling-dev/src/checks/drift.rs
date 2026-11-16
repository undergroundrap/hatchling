//! The docs and the work-order queue, held to what the code and the
//! queue's own files say (WO-042).
//!
//! - **Versions:** the session protocol's and the local API's, as
//!   `docs/arch/protocol.md` and `docs/session-protocol.md` state them,
//!   against `PROTOCOL_VERSION` and `UI_SCHEMA_VERSION`.
//! - **System users:** the count `docs/install.md` gives, against the
//!   users `packaging/sysusers.d/hatchling.conf` makes.
//! - **The queue:** every work-order file has a row in
//!   `docs/workorders/README.md` with the same status, and every row a
//!   file; numbers run from 001 without a gap; an open work order names
//!   its issue; a closed one names its issue or says `none`, and has no
//!   unticked acceptance item.
//!
//! What the issues themselves say (open or closed, on a milestone) is
//! checked against GitHub by `scripts/tracking.py`, in CI.

use std::collections::BTreeMap;
use std::path::Path;

use super::Finding;

const CHECK: &str = "drift";

/// The session protocol's version, where the code keeps it.
pub const FRAMING: &str = "crates/hatchling-core/src/framing.rs";
/// The local API's version, where the code keeps it.
pub const UI: &str = "crates/hatchling-core/src/ui.rs";
/// The architecture overview of both protocols.
pub const PROTOCOL_DOC: &str = "docs/arch/protocol.md";
/// The session protocol's reference.
pub const SESSION_DOC: &str = "docs/session-protocol.md";
/// The users the install makes.
pub const SYSUSERS: &str = "packaging/sysusers.d/hatchling.conf";
/// The install guide.
pub const INSTALL_DOC: &str = "docs/install.md";
/// The work orders' directory.
pub const WORKORDERS: &str = "docs/workorders";

/// The issue link an open work order must carry.
const ISSUE_LINK: &str = "(https://github.com/undergroundrap/hatchling/issues/";

/// Run every drift check.
///
/// # Errors
///
/// Returns [`Error::Io`](crate::Error::Io) if a file that exists can't
/// be read. A file that's missing is a finding, not an error.
pub fn run(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let mut findings = Vec::new();
    findings.extend(versions(root)?);
    findings.extend(users(root)?);
    findings.extend(queue(root)?);
    Ok(findings)
}

/// `path` under `root`, or `None` when it isn't there.
fn read(root: &Path, path: &str) -> Result<Option<String>, crate::Error> {
    match std::fs::read_to_string(root.join(path)) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(crate::Error::Io(error)),
    }
}

/// The finding for a file a check reads that isn't there.
fn missing(path: &str) -> Finding {
    Finding::new(CHECK, format!("{path} is missing"))
}

/// The number that starts `text`, if it starts with a digit.
fn leading_number(text: &str) -> Option<u32> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The number right after the first `marker` in `text`.
fn after(text: &str, marker: &str) -> Option<u32> {
    text.split_once(marker)
        .and_then(|(_, rest)| leading_number(rest))
}

/// The value of `pub const <name>: u16 = N;` in `source`.
fn constant(source: &str, name: &str) -> Option<u32> {
    after(source, &format!("pub const {name}: u16 = "))
}

/// The versions the docs state, against the code's.
fn versions(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let mut findings = Vec::new();
    let mut code = BTreeMap::new();
    for (path, name) in [(FRAMING, "PROTOCOL_VERSION"), (UI, "UI_SCHEMA_VERSION")] {
        match read(root, path)?
            .as_deref()
            .and_then(|text| constant(text, name))
        {
            Some(version) => {
                code.insert(name, version);
            }
            None => findings.push(Finding::new(
                CHECK,
                format!("{path} has no `pub const {name}: u16`"),
            )),
        }
    }
    let stated = [
        (
            PROTOCOL_DOC,
            "**Session protocol** (currently **v",
            "PROTOCOL_VERSION",
        ),
        (
            PROTOCOL_DOC,
            "**Local API** (currently **v",
            "UI_SCHEMA_VERSION",
        ),
        (SESSION_DOC, "**Status:** v", "PROTOCOL_VERSION"),
    ];
    for (path, marker, name) in stated {
        let Some(text) = read(root, path)? else {
            findings.push(missing(path));
            continue;
        };
        let Some(&actual) = code.get(name) else {
            continue;
        };
        match after(&text, marker) {
            Some(said) if said == actual => {}
            Some(said) => findings.push(Finding::new(
                CHECK,
                format!("{path} says v{said} where {name} is {actual}"),
            )),
            None => findings.push(Finding::new(
                CHECK,
                format!("{path} doesn't state the version `{marker}N`"),
            )),
        }
    }
    Ok(findings)
}

/// A number from one to twelve, in words, as the docs write a count.
fn number_word(word: &str) -> Option<usize> {
    let words = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];
    let lower = word.to_ascii_lowercase();
    words
        .iter()
        .position(|w| *w == lower)
        .map(|at| at.saturating_add(1))
}

/// The system users the install guide counts, against those the
/// install makes.
fn users(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let Some(sysusers) = read(root, SYSUSERS)? else {
        return Ok(vec![missing(SYSUSERS)]);
    };
    let Some(install) = read(root, INSTALL_DOC)? else {
        return Ok(vec![missing(INSTALL_DOC)]);
    };
    let made = sysusers
        .lines()
        .filter(|line| line.trim_start().starts_with("u "))
        .count();
    let Some(row) = install
        .lines()
        .find(|line| line.contains("system users") && line.contains("sysusers.d/hatchling.conf"))
    else {
        return Ok(vec![Finding::new(
            CHECK,
            format!("{INSTALL_DOC} has no row for the system users {SYSUSERS} makes"),
        )]);
    };
    let first = row
        .trim_start_matches(['|', ' '])
        .split_whitespace()
        .next()
        .unwrap_or_default();
    Ok(match number_word(first) {
        Some(said) if said == made => Vec::new(),
        Some(said) => vec![Finding::new(
            CHECK,
            format!("{INSTALL_DOC} counts {said} system users where {SYSUSERS} makes {made}"),
        )],
        None => vec![Finding::new(
            CHECK,
            format!("{INSTALL_DOC}'s system users row doesn't start with their count in words"),
        )],
    })
}

/// A row of the queue: its number, the file it links and its status.
#[derive(Debug, PartialEq, Eq)]
struct Row {
    number: u32,
    file: String,
    status: String,
}

/// The queue's rows: `| [NNN](file) | ... | STATUS |`.
fn rows(readme: &str) -> Vec<Row> {
    readme
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("| [")?;
            let (number, rest) = rest.split_once("](")?;
            let (file, _) = rest.split_once(')')?;
            let status = line
                .trim_end()
                .strip_suffix('|')?
                .rsplit('|')
                .next()?
                .trim()
                .to_owned();
            Some(Row {
                number: number.parse().ok()?,
                file: file.to_owned(),
                status,
            })
        })
        .collect()
}

/// Closed is closed, whichever word says it.
fn closed(status: &str) -> bool {
    matches!(status, "CLOSED" | "DONE" | "DROPPED")
}

/// A work order's status, its first word after `**Status:**`.
fn status_of(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("**Status:**"))
        .and_then(|rest| rest.split_whitespace().next())
        .map(|word| {
            // "DONE." and "DONE (2026-11-01)" are both DONE.
            word.chars()
                .take_while(|c| c.is_ascii_uppercase() || *c == '-')
                .collect()
        })
}

/// The acceptance section's items not ticked.
fn unticked(text: &str) -> usize {
    text.split_once("## Acceptance").map_or(0, |(_, rest)| {
        rest.split("\n## ")
            .next()
            .unwrap_or_default()
            .lines()
            .filter(|line| line.trim_start().starts_with("- [ ]"))
            .count()
    })
}

/// The queue, against the work orders' files.
fn queue(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let readme_path = format!("{WORKORDERS}/README.md");
    let Some(readme) = read(root, &readme_path)? else {
        return Ok(vec![missing(&readme_path)]);
    };
    let mut findings = Vec::new();
    let rows = rows(&readme);
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(root.join(WORKORDERS)).map_err(crate::Error::Io)? {
        let name = entry
            .map_err(crate::Error::Io)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if let Some(number) = name
            .strip_prefix("workorder-")
            .filter(|_| {
                Path::new(&name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            })
            .and_then(leading_number)
        {
            files.insert(number, name);
        }
    }
    for row in &rows {
        if !files.values().any(|file| *file == row.file) {
            findings.push(Finding::new(
                CHECK,
                format!(
                    "the queue's row {:03} links {}, which isn't there",
                    row.number, row.file
                ),
            ));
        }
    }
    let highest = files
        .keys()
        .chain(rows.iter().map(|row| &row.number))
        .max()
        .copied();
    for number in 1..=highest.unwrap_or(0) {
        if !files.contains_key(&number) && !rows.iter().any(|row| row.number == number) {
            findings.push(Finding::new(
                CHECK,
                format!("WO-{number:03} is missing: numbers run without a gap"),
            ));
        }
    }
    for (number, name) in &files {
        let Some(text) = read(root, &format!("{WORKORDERS}/{name}"))? else {
            continue;
        };
        findings.extend(work_order(*number, name, &text, &rows));
    }
    Ok(findings)
}

/// One work order's file, against its row.
fn work_order(number: u32, name: &str, text: &str, rows: &[Row]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let status = status_of(text).unwrap_or_default();
    match rows.iter().find(|row| row.file == name) {
        None => findings.push(Finding::new(
            CHECK,
            format!("WO-{number:03} ({name}) has no row in the queue"),
        )),
        Some(row)
            if closed(&row.status) != closed(&status)
                || (!closed(&status) && row.status != status) =>
        {
            findings.push(Finding::new(
                CHECK,
                format!(
                    "WO-{number:03} says {status} but its queue row says {}",
                    row.status
                ),
            ));
        }
        Some(_) => {}
    }
    let issue = text
        .lines()
        .find_map(|line| line.strip_prefix("**Issue:**"))
        .map(str::trim)
        .unwrap_or_default();
    let linked = issue.contains(ISSUE_LINK);
    if closed(&status) {
        if !linked && issue != "none" {
            findings.push(Finding::new(
                CHECK,
                format!("WO-{number:03} is closed and names no issue: link it, or say `none`"),
            ));
        }
        let open = unticked(text);
        if status != "DROPPED" && open > 0 {
            findings.push(Finding::new(
                CHECK,
                format!("WO-{number:03} is closed with {open} acceptance item(s) unticked"),
            ));
        }
    } else if !linked {
        findings.push(Finding::new(
            CHECK,
            format!("WO-{number:03} is {status} and names no tracking issue"),
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Root(std::path::PathBuf);

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Root {
        /// A tree that passes every check: one users row, both versions
        /// stated, and a queue of two work orders.
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("hatchling-dev-drift-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let me = Self(root);
            me.write(FRAMING, "pub const PROTOCOL_VERSION: u16 = 31;\n");
            me.write(UI, "pub const UI_SCHEMA_VERSION: u16 = 4;\n");
            me.write(
                PROTOCOL_DOC,
                "- **Session protocol** (currently **v31**): x\n- **Local API** (currently **v4**): y\n",
            );
            me.write(SESSION_DOC, "# S\n\n**Status:** v31 (current).\n");
            me.write(SYSUSERS, "# users\nu a - \"A\"\nu b - \"B\"\n");
            me.write(
                INSTALL_DOC,
                "| What | Where |\n| Two system users: a and b | `/etc/sysusers.d/hatchling.conf` |\n",
            );
            me.write(
                "docs/workorders/README.md",
                "| WO | Title | TB | Status |\n| --- | --- | --- | --- |\n\
                 | [001](workorder-001-one.md) | One | yes | DONE |\n\
                 | [002](workorder-002-two.md) | Two |  | QUEUED |\n",
            );
            me.write(
                "docs/workorders/workorder-001-one.md",
                "# WO-001\n\n**Status:** CLOSED\n**Issue:** none\n\n## Acceptance criteria\n\n- [x] it\n\n## Gates\n\n- [ ] the template's\n",
            );
            me.write(
                "docs/workorders/workorder-002-two.md",
                "# WO-002\n\n**Status:** QUEUED\n**Issue:** [#9](https://github.com/undergroundrap/hatchling/issues/9)\n\n## Acceptance criteria\n\n- [ ] it\n",
            );
            me
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        fn findings(&self) -> Vec<String> {
            run(&self.0)
                .unwrap()
                .into_iter()
                .map(|finding| finding.message)
                .collect()
        }
    }

    #[test]
    fn a_tree_that_agrees_with_itself_passes() {
        assert_eq!(Root::new("agrees").findings(), Vec::<String>::new());
    }

    #[test]
    fn a_doc_whose_version_is_behind_the_code_is_named() {
        let root = Root::new("versions");
        root.write(FRAMING, "pub const PROTOCOL_VERSION: u16 = 32;\n");
        root.write(UI, "pub const UI_SCHEMA_VERSION: u16 = 5;\n");
        assert_eq!(
            root.findings(),
            [
                "docs/arch/protocol.md says v31 where PROTOCOL_VERSION is 32",
                "docs/arch/protocol.md says v4 where UI_SCHEMA_VERSION is 5",
                "docs/session-protocol.md says v31 where PROTOCOL_VERSION is 32",
            ]
        );
        root.write(SESSION_DOC, "# S\n\nNo status.\n");
        assert!(root.findings().contains(&String::from(
            "docs/session-protocol.md doesn't state the version `**Status:** vN`"
        )),);
        root.write(UI, "nothing\n");
        assert!(root.findings().contains(&String::from(
            "crates/hatchling-core/src/ui.rs has no `pub const UI_SCHEMA_VERSION: u16`"
        )),);
    }

    #[test]
    fn the_install_guides_count_of_users_is_held_to_the_install() {
        let root = Root::new("users");
        root.write(SYSUSERS, "u a - \"A\"\nu b - \"B\"\nu c - \"C\"\n");
        assert_eq!(
            root.findings(),
            [
                "docs/install.md counts 2 system users where packaging/sysusers.d/hatchling.conf makes 3"
            ]
        );
        root.write(
            INSTALL_DOC,
            "| 3 system users | `/etc/sysusers.d/hatchling.conf` |\n",
        );
        assert_eq!(
            root.findings(),
            ["docs/install.md's system users row doesn't start with their count in words"]
        );
        root.write(INSTALL_DOC, "nothing\n");
        assert_eq!(
            root.findings(),
            [
                "docs/install.md has no row for the system users packaging/sysusers.d/hatchling.conf makes"
            ]
        );
    }

    #[test]
    fn the_queue_and_its_files_agree() {
        let root = Root::new("queue");
        // A file with no row, and a gap before it.
        root.write(
            "docs/workorders/workorder-004-four.md",
            "**Status:** QUEUED\n**Issue:** [#1](https://github.com/undergroundrap/hatchling/issues/1)\n",
        );
        assert_eq!(
            root.findings(),
            [
                "WO-003 is missing: numbers run without a gap",
                "WO-004 (workorder-004-four.md) has no row in the queue",
            ]
        );
        let _ = std::fs::remove_file(root.0.join("docs/workorders/workorder-004-four.md"));
        // A row whose file isn't there.
        root.write(
            "docs/workorders/README.md",
            "| [001](workorder-001-one.md) | One | yes | DONE |\n\
             | [002](workorder-002-two.md) | Two |  | QUEUED |\n\
             | [003](workorder-003-gone.md) | Gone |  | QUEUED |\n",
        );
        assert_eq!(
            root.findings(),
            ["the queue's row 003 links workorder-003-gone.md, which isn't there"]
        );
    }

    #[test]
    fn a_work_order_says_its_status_and_issue_and_closes_ticked() {
        let root = Root::new("work-order");
        root.write(
            "docs/workorders/README.md",
            "| [001](workorder-001-one.md) | One | yes | QUEUED |\n\
             | [002](workorder-002-two.md) | Two |  | IN-PROGRESS |\n",
        );
        assert_eq!(
            root.findings(),
            [
                "WO-001 says CLOSED but its queue row says QUEUED",
                "WO-002 says QUEUED but its queue row says IN-PROGRESS",
            ]
        );
        root.write(
            "docs/workorders/README.md",
            "| [001](workorder-001-one.md) | One | yes | DONE |\n\
             | [002](workorder-002-two.md) | Two |  | QUEUED |\n",
        );
        root.write(
            "docs/workorders/workorder-001-one.md",
            "**Status:** DONE\n**Issue:** none yet\n\n## Acceptance criteria\n\n- [x] it\n- [ ] not it\n",
        );
        root.write(
            "docs/workorders/workorder-002-two.md",
            "**Status:** QUEUED\n**Issue:** to be opened\n",
        );
        assert_eq!(
            root.findings(),
            [
                "WO-001 is closed and names no issue: link it, or say `none`",
                "WO-001 is closed with 1 acceptance item(s) unticked",
                "WO-002 is QUEUED and names no tracking issue",
            ]
        );
        // A dropped one keeps its unticked items, with its reason.
        root.write(
            "docs/workorders/workorder-001-one.md",
            "**Status:** DROPPED\n**Issue:** none\n\n## Acceptance criteria\n\n- [ ] not it\n",
        );
        root.write(
            "docs/workorders/README.md",
            "| [001](workorder-001-one.md) | One | yes | DROPPED |\n\
             | [002](workorder-002-two.md) | Two |  | QUEUED |\n",
        );
        root.write(
            "docs/workorders/workorder-002-two.md",
            "**Status:** QUEUED\n**Issue:** [#9](https://github.com/undergroundrap/hatchling/issues/9)\n",
        );
        assert_eq!(root.findings(), Vec::<String>::new());
    }

    #[test]
    fn the_users_row_is_the_table_row_not_a_line_that_mentions_users() {
        let root = Root::new("users-row");
        root.write(
            INSTALL_DOC,
            "Setup makes three system users, and more.\n\
             See /etc/sysusers.d/hatchling.conf for them.\n\
             | Two system users: a and b | `/etc/sysusers.d/hatchling.conf` |\n",
        );
        assert_eq!(root.findings(), Vec::<String>::new());
    }

    #[test]
    fn a_doc_that_cant_be_read_is_an_error_not_a_missing_file() {
        let root = Root::new("unreadable");
        let path = root.0.join(PROTOCOL_DOC);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        assert!(run(&root.0).is_err(), "a directory where the doc should be");
    }

    #[test]
    fn a_missing_doc_is_a_finding() {
        let root = Root::new("missing");
        let _ = std::fs::remove_file(root.0.join(PROTOCOL_DOC));
        let _ = std::fs::remove_file(root.0.join(SYSUSERS));
        assert_eq!(
            root.findings(),
            [
                "docs/arch/protocol.md is missing",
                "docs/arch/protocol.md is missing",
                "packaging/sysusers.d/hatchling.conf is missing",
            ]
        );
    }

    #[test]
    fn the_queues_rows_are_read_from_its_table() {
        let readme = "Text with [001](x) in it.\n| [012](workorder-012-a.md) | A, with | in it | yes | DONE |\n";
        assert_eq!(
            rows(readme),
            [Row {
                number: 12,
                file: String::from("workorder-012-a.md"),
                status: String::from("DONE"),
            }]
        );
        assert_eq!(number_word("Five"), Some(5));
        assert_eq!(number_word("5"), None);
        assert_eq!(leading_number("31 rest"), Some(31));
        assert_eq!(leading_number("v31"), None);
        assert_eq!(status_of("**Status:** DONE.\n").as_deref(), Some("DONE"));
        assert_eq!(
            status_of("**Status:** DONE (2026-11-01)\n").as_deref(),
            Some("DONE")
        );
        assert_eq!(
            status_of("**Status:** IN-PROGRESS\n").as_deref(),
            Some("IN-PROGRESS")
        );
    }
}
