//! The skills an install ships (WO-046), checked as the broker loads them.
//!
//! Each one in `packaging/skills` verifies under a key in
//! `packaging/skill-trust-roots`, every file it holds listed and as listed,
//! and is named for its directory.
//!
//! A playbook edited after its maintainer signed it would install as a
//! skill no broker loads: this check finds that in the gate, before a
//! release ships it.

use std::path::Path;

use super::Finding;

const CHECK: &str = "skills";
/// Where the shipped skills are, from the workspace root.
pub const SKILLS_DIR: &str = "packaging/skills";
/// Who signs them, from the workspace root.
pub const TRUST_ROOTS: &str = "packaging/skill-trust-roots";
const HEADER: &str = "hatchling-trust-roots v1";

/// The findings for the shipped skills under `root`: none when there is no
/// `packaging/skills` at all.
///
/// # Errors
///
/// [`crate::Error::Io`] when the skills directory can't be listed.
pub fn run(root: &Path) -> Result<Vec<Finding>, crate::Error> {
    let dir = root.join(SKILLS_DIR);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut findings = Vec::new();
    let keys = match std::fs::read_to_string(root.join(TRUST_ROOTS)) {
        Ok(text) => match keys_in(&text) {
            Ok(keys) => keys,
            Err(why) => {
                findings.push(Finding::new(CHECK, format!("{TRUST_ROOTS}: {why}")));
                return Ok(findings);
            }
        },
        Err(err) => {
            findings.push(Finding::new(
                CHECK,
                format!("{TRUST_ROOTS} can't be read, and {SKILLS_DIR} needs it: {err}"),
            ));
            return Ok(findings);
        }
    };
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    for name in names {
        findings.extend(check_one(&dir.join(&name), &name, &keys));
    }
    Ok(findings)
}

/// What's wrong with the skill at `skill`, named `name` by its directory.
fn check_one(skill: &Path, name: &str, keys: &[String]) -> Vec<Finding> {
    let shown = format!("{SKILLS_DIR}/{name}");
    if !skill.is_dir() {
        return vec![Finding::new(
            CHECK,
            format!("{shown} isn't a directory: a skill is a directory"),
        )];
    }
    let mut refused = Vec::new();
    for key in keys {
        match hatchling_skill::author::check(skill, key) {
            Ok(checked) => {
                let mut findings = Vec::new();
                if checked.id.split('@').next() != Some(name) {
                    findings.push(Finding::new(
                        CHECK,
                        format!(
                            "{shown} is signed as {}: name it for its directory",
                            checked.id
                        ),
                    ));
                }
                for path in checked.unlisted {
                    findings.push(Finding::new(
                        CHECK,
                        format!(
                            "{shown}/{path} isn't in its signed manifest: sign the skill again, or remove the file"
                        ),
                    ));
                }
                return findings;
            }
            Err(err) => refused.push(err.to_string()),
        }
    }
    vec![Finding::new(
        CHECK,
        format!(
            "{shown} doesn't verify under {TRUST_ROOTS} (sign it again after any change): {}",
            refused.join("; ")
        ),
    )]
}

/// The keys a trust-roots file names, `primary` first: its header, at
/// most a `primary` and a `previous` line, then `end`.
fn keys_in(text: &str) -> Result<Vec<String>, String> {
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err(format!("its first line isn't {HEADER:?}"));
    }
    let mut primary = None;
    let mut previous = None;
    let mut ended = false;
    for line in lines {
        if ended {
            return Err(String::from("it goes on after `end`"));
        }
        if line == "end" {
            ended = true;
            continue;
        }
        let slot = match line.split_once(' ') {
            Some(("primary", key)) => (&mut primary, key),
            Some(("previous", key)) => (&mut previous, key),
            _ => return Err(format!("{line:?} isn't a key line")),
        };
        if slot.0.replace(slot.1.to_owned()).is_some() {
            return Err(String::from("a key is given twice"));
        }
    }
    if !ended {
        return Err(String::from("it has no `end` line"));
    }
    let keys: Vec<String> = primary.into_iter().chain(previous).collect();
    if keys.is_empty() {
        return Err(String::from("it trusts nobody, and skills ship signed"));
    }
    Ok(keys)
}

#[cfg(test)]
mod tests;
