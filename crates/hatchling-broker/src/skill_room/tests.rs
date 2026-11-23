use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt as _;

use ed25519_dalek::{Signer as _, SigningKey};
use hatchling_skill::{FileEntry, MANIFEST_FILE, Manifest, SignedManifest};

use super::*;
use crate::SkillConfig;
use crate::skills::Enabled;
use crate::trust_roots::TRUST_ROOTS_HEADER;

const SECRET: [u8; 32] = [0x3d; 32];

/// A Room of the test's: what it holds, and every call, in order.
#[derive(Default)]
struct Fake {
    held: RefCell<Vec<HeldSkillEntry>>,
    calls: RefCell<Vec<String>>,
    fail: Option<&'static str>,
}

impl RoomSkills for Fake {
    fn held(&self) -> Result<Vec<HeldSkillEntry>, String> {
        if self.fail == Some("held") {
            return Err(String::from("unreadable"));
        }
        Ok(self.held.borrow().clone())
    }

    fn put(&self, name: &str, digest: &str, files: &[(String, Vec<u8>)]) -> Result<(), String> {
        if self.fail == Some("put") {
            return Err(String::from("full"));
        }
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        self.calls
            .borrow_mut()
            .push(format!("put {name} {digest} {}", paths.join(",")));
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        if self.fail == Some("remove") {
            return Err(String::from("busy"));
        }
        self.calls.borrow_mut().push(format!("remove {name}"));
        Ok(())
    }
}

fn held(name: &str, digest: &str) -> HeldSkillEntry {
    HeldSkillEntry {
        name: name.to_owned(),
        digest: digest.to_owned(),
    }
}

fn want(name: &str, digest: &str) -> Wanted {
    Wanted {
        name: name.to_owned(),
        digest: digest.to_owned(),
        files: vec![(String::from("SKILL.md"), b"x".to_vec())],
    }
}

#[test]
fn a_refresh_takes_out_what_isnt_wanted_as_it_is_and_puts_in_what_is_missing() {
    let room = Fake::default();
    *room.held.borrow_mut() = vec![held("a", "d1"), held("b", "d2"), held("c", "dx")];
    let done = refresh(
        &room,
        &[want("a", "d1"), want("b", "d3"), want("e", "d4")],
        2,
    )
    .unwrap();
    assert_eq!(
        done,
        Refreshed {
            put: 2,
            removed: 2,
            left_out: 2
        }
    );
    assert_eq!(
        *room.calls.borrow(),
        [
            "remove b",
            "remove c",
            "put b d3 SKILL.md",
            "put e d4 SKILL.md"
        ],
        "out first, then in; the one held as wanted is left alone"
    );
    // In line already: nothing done.
    let room = Fake::default();
    *room.held.borrow_mut() = vec![held("a", "d1")];
    assert_eq!(
        refresh(&room, &[want("a", "d1")], 0).unwrap(),
        Refreshed::default()
    );
    assert!(room.calls.borrow().is_empty());
}

#[test]
fn a_refresh_stops_at_the_first_failure_and_says_it() {
    for (fail, said) in [("held", "unreadable"), ("remove", "busy"), ("put", "full")] {
        let room = Fake {
            fail: Some(fail),
            ..Fake::default()
        };
        *room.held.borrow_mut() = vec![held("old", "d")];
        assert_eq!(
            refresh(&room, &[want("new", "d")], 0),
            Err(String::from(said))
        );
    }
}

/// `name`, signed, its `files` listed, `program` the one that runs.
fn package(dir: &std::path::Path, name: &str, files: &[(&str, &[u8])], program: Option<&str>) {
    let mut manifest =
        Manifest::for_program(name, "1", "Use when asked.", "main", b"x", Vec::new());
    manifest.files.clear();
    manifest.program = None;
    for (path, bytes) in files {
        manifest.files.push(FileEntry::of(path, bytes));
        let at = dir.join(name).join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, bytes).unwrap();
    }
    manifest.instructions = Some(String::from("SKILL.md"));
    if let Some(program) = program {
        let mut with =
            Manifest::for_program(name, "1", "Use when asked.", program, b"bin", Vec::new());
        manifest.program = with.program.take();
        std::fs::write(dir.join(name).join(program), b"bin").unwrap();
        manifest.files.push(FileEntry::of(program, b"bin"));
    }
    let signature = SigningKey::from_bytes(&SECRET).sign(&manifest.canonical_bytes().unwrap());
    let signed = SignedManifest {
        manifest,
        signature: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_bytes(),
        )),
    };
    std::fs::write(
        dir.join(name).join(MANIFEST_FILE),
        signed.to_json().unwrap(),
    )
    .unwrap();
    for directory in [dir.join(name), dir.join(name).join("scripts")] {
        if directory.is_dir() {
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

#[test]
fn what_the_room_is_given_is_each_enabled_skills_verified_files_but_its_program() {
    let scratch = crate::api_auth::tests::scratch("skill-room");
    let dir = scratch.0.join("skills");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    package(
        &dir,
        "notes",
        &[
            ("SKILL.md", b"# Steps\n"),
            ("scripts/outline.py", b"print(1)\n"),
        ],
        Some("main"),
    );
    package(&dir, "bad name", &[("SKILL.md", b"# Steps\n")], None);
    package(&dir, "changed", &[("SKILL.md", b"# Steps\n")], None);
    package(&dir, "waiting", &[("SKILL.md", b"# Steps\n")], None);
    let public = SigningKey::from_bytes(&SECRET).verifying_key().to_bytes();
    let public = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, public);
    let roots = scratch.0.join("roots");
    std::fs::write(
        &roots,
        format!("{TRUST_ROOTS_HEADER}\nprimary {public}\nend\n"),
    )
    .unwrap();
    std::fs::set_permissions(&roots, std::fs::Permissions::from_mode(0o600)).unwrap();
    let skills = Skills::load(&SkillConfig {
        skills_dir: dir.clone(),
        trust_roots_file: roots,
        enablements: scratch.0.join("enabled"),
    })
    .unwrap();
    let enabled: Enabled = skills
        .snapshot()
        .listing()
        .iter()
        .filter(|listing| listing.name != "waiting")
        .map(|listing| (listing.name.clone(), listing.digest.clone()))
        .collect();
    skills.set_enabled(enabled);
    // Changed since it was loaded: no longer as signed.
    std::fs::write(dir.join("changed").join("SKILL.md"), b"# Other\n").unwrap();
    let (wanted, left_out) = wanted(&skills.snapshot());
    assert_eq!(left_out, 2, "the bad name and the changed file");
    let [notes] = wanted.as_slice() else {
        unreachable!("{wanted:?}")
    };
    assert_eq!(notes.name, "notes");
    let listed = skills
        .snapshot()
        .listing()
        .iter()
        .find(|listing| listing.name == "notes")
        .unwrap()
        .digest
        .clone();
    assert_eq!(notes.digest, listed);
    assert_eq!(
        notes.files,
        [
            (String::from("SKILL.md"), b"# Steps\n".to_vec()),
            (String::from("scripts/outline.py"), b"print(1)\n".to_vec()),
        ],
        "its program isn't copied"
    );
    // And brought in line through a Room.
    let room = Fake::default();
    refresh_from(&room, &skills);
    assert_eq!(room.calls.borrow().len(), 1);
    assert!(
        room.calls
            .borrow()
            .first()
            .unwrap()
            .starts_with("put notes ")
    );
}
