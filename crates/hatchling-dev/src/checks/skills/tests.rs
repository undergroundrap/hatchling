use std::path::{Path, PathBuf};

use super::*;

/// A workspace of the test's own, empty.
fn scratch(name: &str) -> Result<PathBuf, String> {
    let root = std::env::temp_dir().join(format!(
        "hatchling-dev-skills-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(SKILLS_DIR)).map_err(|err| err.to_string())?;
    Ok(root)
}

/// A signing key in `root`, and its public half.
fn key(root: &Path, name: &str) -> Result<(PathBuf, String), String> {
    let file = root.join(name);
    let public = hatchling_skill::author::new_key(&file).map_err(|err| err.to_string())?;
    Ok((file, public.base64))
}

/// The playbook `name` in `root`'s shipped skills, signed with `key`.
fn signed(root: &Path, name: &str, key: &Path) -> Result<PathBuf, String> {
    let dir = root.join(SKILLS_DIR).join(name);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    std::fs::write(dir.join("SKILL.md"), "# Steps\n").map_err(|err| err.to_string())?;
    std::fs::write(
        dir.join("manifest.json"),
        format!(
            "{{\"name\":\"{name}\",\"version\":\"1\",\"description\":\"Use when asked.\",\"instructions\":\"SKILL.md\"}}"
        ),
    )
    .map_err(|err| err.to_string())?;
    hatchling_skill::author::sign(&dir, key).map_err(|err| err.to_string())?;
    Ok(dir)
}

fn trust(root: &Path, text: &str) -> Result<(), String> {
    std::fs::write(root.join(TRUST_ROOTS), text).map_err(|err| err.to_string())
}

fn messages(root: &Path) -> Vec<String> {
    run(root)
        .unwrap_or_default()
        .into_iter()
        .map(|finding| finding.message)
        .collect()
}

#[test]
fn a_shipped_skill_signed_by_a_trusted_key_passes_and_none_shipped_passes() {
    let root = scratch("passes").unwrap();
    let (file, public) = key(&root, "key").unwrap();
    signed(&root, "notes", &file).unwrap();
    trust(&root, &format!("{HEADER}\nprimary {public}\nend\n")).unwrap();
    assert_eq!(run(&root).unwrap(), Vec::new());

    // Nothing shipped: nothing to check, and no trust roots needed.
    let bare = scratch("bare").unwrap();
    std::fs::remove_dir(bare.join(SKILLS_DIR)).unwrap();
    assert_eq!(run(&bare).unwrap(), Vec::new());
}

#[test]
fn a_skill_changed_after_signing_or_holding_more_or_misnamed_is_found() {
    let root = scratch("changed").unwrap();
    let (file, public) = key(&root, "key").unwrap();
    trust(&root, &format!("{HEADER}\nprimary {public}\nend\n")).unwrap();

    let changed = signed(&root, "changed", &file).unwrap();
    std::fs::write(changed.join("SKILL.md"), "# Other steps\n").unwrap();
    let extra = signed(&root, "extra", &file).unwrap();
    std::fs::write(extra.join("notes.txt"), "unlisted").unwrap();
    let misnamed = signed(&root, "first", &file).unwrap();
    std::fs::rename(&misnamed, root.join(SKILLS_DIR).join("second")).unwrap();
    std::fs::write(root.join(SKILLS_DIR).join("loose.md"), "x").unwrap();

    let found = messages(&root);
    assert_eq!(found.len(), 4, "{found:#?}");
    let [changed, extra, loose, second] = found.as_slice() else {
        unreachable!("{found:#?}")
    };
    assert!(
        changed.starts_with("packaging/skills/changed doesn't verify")
            && changed.contains("sign it again"),
        "{changed}"
    );
    assert_eq!(
        extra,
        "packaging/skills/extra/notes.txt isn't in its signed manifest: sign the skill again, or remove the file"
    );
    assert_eq!(
        loose,
        "packaging/skills/loose.md isn't a directory: a skill is a directory"
    );
    assert_eq!(
        second,
        "packaging/skills/second is signed as first@1: name it for its directory"
    );
}

#[test]
fn a_skill_signed_by_a_key_not_trusted_is_found_and_the_previous_key_is_trusted() {
    let root = scratch("keys").unwrap();
    let (old, old_public) = key(&root, "old").unwrap();
    let (_, new_public) = key(&root, "new").unwrap();
    signed(&root, "notes", &old).unwrap();
    trust(&root, &format!("{HEADER}\nprimary {new_public}\nend\n")).unwrap();
    let found = messages(&root);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found.iter().all(|message| message.starts_with(
            "packaging/skills/notes doesn't verify under packaging/skill-trust-roots"
        )),
        "{found:#?}"
    );
    // During a rotation, the key it was signed with is the previous one.
    trust(
        &root,
        &format!("{HEADER}\nprimary {new_public}\nprevious {old_public}\nend\n"),
    )
    .unwrap();
    assert_eq!(messages(&root), Vec::<String>::new());
}

#[test]
fn a_trust_roots_file_that_is_missing_or_malformed_is_found() {
    let root = scratch("roots").unwrap();
    let (file, public) = key(&root, "key").unwrap();
    signed(&root, "notes", &file).unwrap();
    let missing = messages(&root);
    assert!(
        matches!(missing.as_slice(), [one] if one.starts_with("packaging/skill-trust-roots can't be read, and packaging/skills needs it")),
        "{missing:#?}"
    );
    for (text, why) in [
        (
            format!("other\nprimary {public}\nend\n"),
            "its first line isn't",
        ),
        (
            format!("{HEADER}\nprimary {public}\n"),
            "it has no `end` line",
        ),
        (
            format!("{HEADER}\nprimary {public}\nend\nprimary {public}\n"),
            "it goes on after `end`",
        ),
        (
            format!("{HEADER}\nprimary {public}\nprimary {public}\nend\n"),
            "a key is given twice",
        ),
        (
            format!("{HEADER}\nspare {public}\nend\n"),
            "isn't a key line",
        ),
        (format!("{HEADER}\nend\n"), "it trusts nobody"),
    ] {
        trust(&root, &text).unwrap();
        let found = messages(&root);
        assert!(
            matches!(found.as_slice(), [one] if one.starts_with("packaging/skill-trust-roots: ") && one.contains(why)),
            "{why}: {found:#?}"
        );
    }
}
