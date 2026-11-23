use std::os::unix::fs::PermissionsExt as _;

use super::*;

/// A fresh directory unique to this test process and test.
fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hatchling-author-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A skill with a playbook, a nested script and a dotfile, and its draft
/// manifest; and a key to sign it with.
fn package(name: &str) -> (PathBuf, PathBuf, PublicKey) {
    let root = dir(name);
    let skill = root.join("skill");
    std::fs::create_dir_all(skill.join("scripts")).unwrap();
    std::fs::write(skill.join("SKILL.md"), b"# Steps\n").unwrap();
    std::fs::write(skill.join("scripts").join("outline.py"), b"print(1)\n").unwrap();
    std::fs::write(skill.join(".notes"), b"kept\n").unwrap();
    std::fs::write(
        skill.join(MANIFEST_FILE),
        br#"{"name":"write-a-report","version":"1","description":"Use when asked for a report.","instructions":"SKILL.md"}"#,
    )
    .unwrap();
    let key = root.join("signing.key");
    let public = new_key(&key).unwrap();
    (skill, key, public)
}

fn run(args: &[&str]) -> (u8, String, String) {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = cli(&args, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn a_new_key_is_readable_by_its_owner_alone_and_never_overwrites() {
    let root = dir("key");
    let key = root.join("k");
    let public = new_key(&key).unwrap();
    let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(BASE64.decode(&public.base64).unwrap().len(), 32);
    assert_eq!(public.fingerprint.len(), 64);
    // It reads back as the key whose public half was given.
    assert_eq!(PublicKey::of(&read_key(&key).unwrap()), public);
    // A second key at the same path is refused, and the first is kept.
    let before = std::fs::read(&key).unwrap();
    assert!(matches!(new_key(&key), Err(Error::Author(_))));
    assert_eq!(std::fs::read(&key).unwrap(), before);
    // Two keys differ.
    assert_ne!(new_key(&root.join("k2")).unwrap(), public);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_key_others_can_read_a_link_or_a_malformed_file_is_refused() {
    let root = dir("bad-keys");
    let key = root.join("k");
    let _ = new_key(&key).unwrap();
    for mode in [0o640, 0o604, 0o620, 0o602] {
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(mode)).unwrap();
        let err = read_key(&key).unwrap_err().to_string();
        assert!(
            err.contains("others can read or write it"),
            "{mode:o}: {err}"
        );
    }
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(read_key(&key).is_ok());
    let link = root.join("link");
    std::os::unix::fs::symlink(&key, &link).unwrap();
    assert!(
        read_key(&link)
            .unwrap_err()
            .to_string()
            .contains("links are refused")
    );
    let write = |name: &str, text: &str| {
        let path = root.join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        read_key(&path).unwrap_err().to_string()
    };
    let good = BASE64.encode([7u8; 32]);
    let short = BASE64.encode([7u8; 31]);
    let cases = [
        ("no-header", format!("{good}\n"), "first line"),
        ("not-base64", format!("{KEY_HEADER}\n!!\n"), "isn't base64"),
        (
            "short",
            format!("{KEY_HEADER}\n{short}\n"),
            "isn't 32 bytes",
        ),
        (
            "more",
            format!("{KEY_HEADER}\n{good}\nmore\n"),
            "more than a key",
        ),
    ];
    for (name, text, why) in cases {
        let said = write(name, &text);
        assert!(said.contains(why), "{name}: {said}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn signing_lists_every_file_and_checks_as_the_loader_will() {
    let (skill, key, public) = package("sign");
    let signed = sign(&skill, &key).unwrap();
    let paths: Vec<&str> = signed
        .manifest
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    // Sorted, nested, the dotfile included, the manifest never.
    assert_eq!(paths, [".notes", "SKILL.md", "scripts/outline.py"]);
    assert_eq!(
        signed.manifest.file("SKILL.md"),
        Some(&FileEntry::of("SKILL.md", b"# Steps\n"))
    );
    assert_eq!(signed.manifest.format, FORMAT);
    // What was written parses as format 2 and verifies under the key.
    let written =
        SignedManifest::from_json(&std::fs::read(skill.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(written.manifest, signed.manifest);
    let checked = check(&skill, &public.base64).unwrap();
    assert_eq!(
        checked,
        Checked {
            id: String::from("write-a-report@1"),
            signer: public.fingerprint,
            files: 3,
            unlisted: Vec::new(),
        }
    );
    // Signing again, a file added, lists it.
    std::fs::write(skill.join("templates.md"), b"t\n").unwrap();
    assert_eq!(sign(&skill, &key).unwrap().manifest.files.len(), 4);
    let _ = std::fs::remove_dir_all(skill.parent().unwrap());
}

#[test]
fn check_refuses_another_key_a_changed_file_and_names_an_unlisted_one() {
    let (skill, key, public) = package("check");
    let _ = sign(&skill, &key).unwrap();
    let other = new_key(&skill.parent().unwrap().join("other.key")).unwrap();
    assert!(matches!(
        check(&skill, &other.base64),
        Err(Error::BadSignature { .. })
    ));
    assert!(matches!(
        check(&skill, "not a key"),
        Err(Error::InvalidTrustRoot(_))
    ));
    // A file added after signing is named, not read.
    std::fs::write(skill.join("added.sh"), b"rm -rf ~\n").unwrap();
    assert_eq!(
        check(&skill, &public.base64).unwrap().unlisted,
        vec![String::from("added.sh")]
    );
    // A listed file changed, or gone, is refused.
    std::fs::write(skill.join("SKILL.md"), b"# Other steps\n").unwrap();
    assert!(matches!(
        check(&skill, &public.base64),
        Err(Error::FileMismatch { path, .. }) if path == "SKILL.md"
    ));
    std::fs::remove_file(skill.join("SKILL.md")).unwrap();
    assert!(matches!(
        check(&skill, &public.base64),
        Err(Error::FileMismatch { path, .. }) if path == "SKILL.md"
    ));
    let _ = std::fs::remove_dir_all(skill.parent().unwrap());
}

#[test]
fn a_link_in_the_skill_or_a_manifest_that_doesnt_validate_signs_nothing() {
    let (skill, key, public) = package("refuse");
    std::os::unix::fs::symlink("/etc/passwd", skill.join("passwd")).unwrap();
    let err = sign(&skill, &key).unwrap_err().to_string();
    assert!(err.contains("passwd is a link or a special file"), "{err}");
    std::fs::remove_file(skill.join("passwd")).unwrap();
    // A link to a listed file is refused at check, as the loader would.
    let _ = sign(&skill, &key).unwrap();
    std::fs::remove_file(skill.join(".notes")).unwrap();
    std::os::unix::fs::symlink(skill.join("SKILL.md"), skill.join(".notes")).unwrap();
    assert!(matches!(
        check(&skill, &public.base64),
        Err(Error::FileMismatch { path, .. }) if path == ".notes"
    ));
    std::fs::remove_file(skill.join(".notes")).unwrap();
    // Neither a playbook nor a program: refused, and the manifest unchanged.
    let draft = br#"{"name":"empty","version":"1","description":"Nothing."}"#;
    std::fs::write(skill.join(MANIFEST_FILE), draft).unwrap();
    assert!(matches!(sign(&skill, &key), Err(Error::InvalidManifest(_))));
    assert_eq!(std::fs::read(skill.join(MANIFEST_FILE)).unwrap(), draft);
    // A field the format doesn't have is refused.
    std::fs::write(
        skill.join(MANIFEST_FILE),
        br#"{"name":"s","version":"1","description":"d","instructions":"SKILL.md","hooks":"x"}"#,
    )
    .unwrap();
    assert!(matches!(sign(&skill, &key), Err(Error::InvalidManifest(_))));
    let _ = std::fs::remove_dir_all(skill.parent().unwrap());
}

#[test]
fn the_command_says_what_it_did_and_never_the_private_key() {
    let root = dir("cli");
    let key = root.join("k");
    let key_arg = key.to_str().unwrap();
    let (status, out, err) = run(&["key", "new", key_arg]);
    assert_eq!(status, 0, "{err}");
    let secret = std::fs::read_to_string(&key).unwrap();
    let secret = secret.lines().nth(1).unwrap();
    assert!(!out.contains(secret) && !err.contains(secret));
    let public = PublicKey::of(&read_key(&key).unwrap());
    assert!(
        out.contains(&public.base64) && out.contains(&public.fingerprint),
        "{out}"
    );

    let skill = root.join("skill");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), b"# Steps\n").unwrap();
    std::fs::write(
        skill.join(MANIFEST_FILE),
        br#"{"name":"s","version":"1","description":"d","instructions":"SKILL.md"}"#,
    )
    .unwrap();
    let skill_arg = skill.to_str().unwrap();
    let (status, out, err) = run(&["sign", skill_arg, "--key", key_arg]);
    assert_eq!(status, 0, "{err}");
    assert!(out.contains("Signed s@1: 1 files listed"), "{out}");
    assert!(!out.contains(secret) && !err.contains(secret));
    let (status, out, _) = run(&["check", skill_arg, "--root", &public.base64]);
    assert_eq!(status, 0);
    assert!(
        out.contains("s@1 verifies") && out.contains(&public.fingerprint),
        "{out}"
    );
    std::fs::write(skill.join("extra"), b"x").unwrap();
    let (_, out, _) = run(&["check", skill_arg, "--root", &public.base64]);
    assert!(out.contains("Not listed, so never read: extra"), "{out}");

    // A refusal is said on stderr with status 1; a misuse prints usage, 64.
    let (status, _, err) = run(&["key", "new", key_arg]);
    assert_eq!(status, 1);
    assert!(err.starts_with("hatchling skill: "), "{err}");
    let misuses: [&[&str]; 4] = [
        &[],
        &["sign", skill_arg],
        &["key", "old", key_arg],
        &["sign", skill_arg, "--secret", "x"],
    ];
    for misuse in misuses {
        let (status, out, err) = run(misuse);
        assert_eq!(status, 64, "{misuse:?}");
        assert!(
            out.is_empty() && err.contains("hatchling skill key new"),
            "{misuse:?}"
        );
    }
    let (status, out, _) = run(&["--help"]);
    assert_eq!(status, 0);
    assert!(out.contains("hatchling skill sign"));
    let _ = std::fs::remove_dir_all(&root);
}
