use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use super::*;

const DIGEST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A volume of this test's own: a root as the room disk's is, but this
/// user's, not root's.
struct Volume {
    root: PathBuf,
    pinned: OwnedFd,
}

impl Volume {
    fn new(test: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "hatchling-room-skills-{test}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let pinned = rustix::fs::open(
            &root,
            OFlags::PATH.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC),
            Mode::empty(),
        )
        .unwrap();
        Self { root, pinned }
    }

    fn skills(&self) -> PathBuf {
        self.root.join("skills")
    }

    fn put(&self, name: &str, digest: &str, files: &[SkillFile<'_>]) -> Result<(), PutError> {
        put_as(self.pinned.as_fd(), name, digest, files, crate::euid())
    }

    fn held(&self) -> Vec<HeldSkill> {
        held_as(self.pinned.as_fd(), crate::euid()).unwrap()
    }

    fn remove(&self, name: &str) -> Result<bool, PutError> {
        remove_as(self.pinned.as_fd(), name, crate::euid())
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.skills())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const fn file<'a>(path: &'a str, bytes: &'a [u8]) -> SkillFile<'a> {
    SkillFile { path, bytes }
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

fn refusal(put: Result<(), PutError>) -> &'static str {
    match put {
        Err(PutError::Refused(why)) => why,
        other => unreachable!("not refused: {other:?}"),
    }
}

#[test]
fn the_bounds_are_what_the_docs_say() {
    assert_eq!((SKILLS, SKILL_FILES, SKILL_BYTES), (64, 64, 1 << 20));
}

#[test]
fn a_skill_exactly_at_its_bounds_is_put() {
    let volume = Volume::new("edges");
    let full = vec![b'x'; SKILL_BYTES];
    volume.put("full", DIGEST_A, &[file("big", &full)]).unwrap();
    assert_eq!(
        std::fs::metadata(volume.skills().join("full/big"))
            .unwrap()
            .len(),
        1 << 20
    );
    let names: Vec<String> = (0..SKILL_FILES).map(|n| format!("f{n}")).collect();
    let many: Vec<SkillFile<'_>> = names.iter().map(|path| file(path, b"x")).collect();
    volume.put("many", DIGEST_A, &many).unwrap();
    assert_eq!(
        std::fs::read_dir(volume.skills().join("many"))
            .unwrap()
            .count(),
        65
    );
    let deep = "a/b/c/d/e/f/g/h";
    volume.put("deep", DIGEST_A, &[file(deep, b"x")]).unwrap();
    assert!(
        volume.remove("deep").unwrap(),
        "the deepest a skill can be is taken out"
    );
}

#[test]
fn whats_left_deeper_than_a_skill_can_be_is_refused_and_at_the_bound_taken_out() {
    let volume = Volume::new("depth");
    volume.held();
    // `.old-x` itself, then eight directories under it: the deepest a skill is.
    let at = volume.skills().join(".old-at/1/2/3/4/5/6/7/8");
    std::fs::create_dir_all(&at).unwrap();
    volume.held();
    assert!(!volume.skills().join(".old-at").exists(), "taken out");
    let past = volume.skills().join(".old-past/1/2/3/4/5/6/7/8/9");
    std::fs::create_dir_all(&past).unwrap();
    assert!(held_as(volume.pinned.as_fd(), crate::euid()).is_err());
    std::fs::remove_dir_all(volume.skills().join(".old-past")).unwrap();
}

#[test]
fn names_and_paths_keep_to_their_grammar() {
    for name in ["notes", "write-a-report", "a.b_c-1", &"n".repeat(128)] {
        assert!(is_name(name), "{name}");
    }
    for name in [
        "",
        ".hidden",
        "..",
        "a/b",
        "a b",
        "naïve",
        "a\0",
        &"n".repeat(129),
    ] {
        assert!(!is_name(name), "{name:?}");
    }
    assert!(is_path("SKILL.md"));
    assert!(is_path("scripts/outline.py"));
    assert!(is_path("a/b/c/d/e/f/g/h"));
    for path in [
        "",
        "/abs",
        "a//b",
        "a/../b",
        "a/./b",
        "a/",
        "a/b/c/d/e/f/g/h/i",
        "a/.git",
    ] {
        assert!(!is_path(path), "{path:?}");
    }
    assert!(is_digest(DIGEST_A));
    for digest in [
        "",
        "A".repeat(64).as_str(),
        &"a".repeat(63),
        &"a".repeat(65),
        &format!("{}g", "a".repeat(63)),
    ] {
        assert!(!is_digest(digest), "{digest:?}");
    }
}

#[test]
fn a_skill_is_put_whole_read_only_and_listed_with_its_digest() {
    let volume = Volume::new("put");
    assert!(volume.held().is_empty(), "made empty");
    assert_eq!(mode(&volume.skills()), 0o755);
    volume
        .put(
            "notes",
            DIGEST_A,
            &[
                file("SKILL.md", b"# Steps\n"),
                file("scripts/outline.py", b"print(1)\n"),
            ],
        )
        .unwrap();
    let skill = volume.skills().join("notes");
    assert_eq!(std::fs::read(skill.join("SKILL.md")).unwrap(), b"# Steps\n");
    assert_eq!(
        std::fs::read(skill.join("scripts/outline.py")).unwrap(),
        b"print(1)\n"
    );
    assert_eq!(
        std::fs::read_to_string(skill.join(DIGEST)).unwrap(),
        DIGEST_A
    );
    for read_only in ["SKILL.md", "scripts/outline.py", DIGEST] {
        assert_eq!(mode(&skill.join(read_only)), 0o444, "{read_only}");
    }
    assert_eq!(mode(&skill), 0o755);
    assert_eq!(mode(&skill.join("scripts")), 0o755);
    assert_eq!(
        volume.held(),
        [HeldSkill {
            name: String::from("notes"),
            digest: String::from(DIGEST_A),
        }]
    );
    // Put again at another digest: in its place, the old files gone.
    volume
        .put("notes", DIGEST_B, &[file("SKILL.md", b"# Other\n")])
        .unwrap();
    assert_eq!(std::fs::read(skill.join("SKILL.md")).unwrap(), b"# Other\n");
    assert!(!skill.join("scripts").exists());
    assert_eq!(volume.held().first().unwrap().digest, DIGEST_B);
    assert_eq!(volume.entries(), ["notes"], "nothing left beside it");
}

#[test]
fn a_skill_is_taken_out_and_what_an_interrupted_one_left_is_tidied() {
    let volume = Volume::new("remove");
    volume
        .put("notes", DIGEST_A, &[file("a/b.md", b"x")])
        .unwrap();
    assert!(volume.remove("notes").unwrap());
    assert!(!volume.remove("notes").unwrap(), "already gone");
    assert!(volume.entries().is_empty());
    // A put or removal that didn't finish, and an entry not a skill's.
    std::fs::create_dir_all(volume.skills().join(".new-half/sub")).unwrap();
    std::fs::write(volume.skills().join(".new-half/sub/f"), b"x").unwrap();
    std::fs::create_dir(volume.skills().join(".old-gone")).unwrap();
    std::fs::write(volume.skills().join("stray.txt"), b"x").unwrap();
    std::fs::create_dir(volume.skills().join("nodigest")).unwrap();
    assert_eq!(
        volume.held(),
        [HeldSkill {
            name: String::from("nodigest"),
            digest: String::new(),
        }],
        "half-done ones tidied, a file isn't a skill, an unreadable digest is empty"
    );
    assert_eq!(volume.entries(), ["nodigest", "stray.txt"]);
}

#[test]
fn what_the_room_doesnt_take_is_refused_and_nothing_is_written() {
    let volume = Volume::new("refused");
    let big = vec![0u8; SKILL_BYTES + 1];
    let many: Vec<String> = (0..=SKILL_FILES).map(|n| format!("f{n}")).collect();
    let many: Vec<SkillFile<'_>> = many.iter().map(|path| file(path, b"x")).collect();
    for (name, digest, files, why) in [
        (
            ".x",
            DIGEST_A,
            vec![file("a", b"x")],
            "that isn't a name a skill has in the Room",
        ),
        (
            "x",
            "nope",
            vec![file("a", b"x")],
            "that isn't a skill's digest",
        ),
        (
            "x",
            DIGEST_A,
            vec![file("../a", b"x")],
            "that isn't a path a file has in a skill in the Room",
        ),
        (
            "x",
            DIGEST_A,
            vec![file(DIGEST, b"x")],
            "that isn't a path a file has in a skill in the Room",
        ),
        (
            "x",
            DIGEST_A,
            vec![file("a", b"x"), file("a", b"y")],
            "a skill names a file twice",
        ),
        (
            "x",
            DIGEST_A,
            vec![file("a", &big)],
            "a skill holds at most 1 MiB in the Room",
        ),
        (
            "x",
            DIGEST_A,
            many.clone(),
            "a skill holds at most 64 files in the Room",
        ),
    ] {
        assert_eq!(
            refusal(volume.put(name, digest, &files)),
            why,
            "{name} {digest}"
        );
    }
    assert!(volume.held().is_empty());
    assert!(matches!(volume.remove(".x"), Err(PutError::Refused(_))));
}

#[test]
fn the_room_holds_at_most_its_bound_of_skills_and_a_skill_may_be_put_again_there() {
    let volume = Volume::new("bound");
    for n in 0..SKILLS {
        volume
            .put(&format!("s{n}"), DIGEST_A, &[file("SKILL.md", b"x")])
            .unwrap();
    }
    assert_eq!(
        refusal(volume.put("one-more", DIGEST_A, &[file("SKILL.md", b"x")])),
        "the Room holds 64 skills already"
    );
    volume
        .put("s0", DIGEST_B, &[file("SKILL.md", b"y")])
        .unwrap();
    assert_eq!(volume.held().len(), SKILLS);
}

#[test]
fn a_skills_directory_others_could_write_or_reached_by_a_link_is_refused() {
    let volume = Volume::new("trust");
    std::fs::create_dir(volume.skills()).unwrap();
    std::fs::set_permissions(volume.skills(), std::fs::Permissions::from_mode(0o775)).unwrap();
    assert!(matches!(
        volume.put("x", DIGEST_A, &[file("a", b"x")]),
        Err(PutError::Failed(_))
    ));
    std::fs::remove_dir(volume.skills()).unwrap();
    let elsewhere = volume.root.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, volume.skills()).unwrap();
    assert!(matches!(
        volume.put("x", DIGEST_A, &[file("a", b"x")]),
        Err(PutError::Failed(_))
    ));
    assert!(
        std::fs::read_dir(&elsewhere).unwrap().next().is_none(),
        "nothing written through it"
    );
}
