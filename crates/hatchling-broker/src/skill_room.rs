//! The skills the owner enabled, copied into the agent's computer
//! (WO-046): read-only at `/workspace/skills/<name>/`, so a playbook's
//! helpers run there under the Room's rules.
//!
//! Before a chat run with a Room, the broker brings the Room's copy in
//! line with what the owner enabled: a skill no longer enabled, or enabled
//! at another digest, is taken out, and one the Room lacks is put in.
//! What is put is the broker's own verified copy, each file read and
//! checked against its signed hash as it is read. A skill's program isn't
//! copied: it runs in a cell of its own, through `skill_invoke`. A skill
//! whose name or a path the Room can't hold is left out, and logged.
//!
//! A refresh that fails never stops the run: the Room keeps what it had,
//! and the next run tries again.

use hatchling_skill::SkillId;
use hatchling_sys::room_skills::{SKILL_BYTES, is_name, is_path};

use crate::room_disk::HeldSkillEntry;
use crate::skills::{SkillSet, Skills, read_skill_file};

/// What puts the skills on the room disk: the spawner, or a test's.
pub(crate) trait RoomSkills {
    /// The skills the Room holds, with their digests.
    ///
    /// # Errors
    ///
    /// Why they can't be read.
    fn held(&self) -> Result<Vec<HeldSkillEntry>, String>;
    /// Puts `name` at `digest`, holding `files`.
    ///
    /// # Errors
    ///
    /// Why it wasn't put.
    fn put(&self, name: &str, digest: &str, files: &[(String, Vec<u8>)]) -> Result<(), String>;
    /// Takes `name` out.
    ///
    /// # Errors
    ///
    /// Why it wasn't taken out.
    fn remove(&self, name: &str) -> Result<(), String>;
}

impl RoomSkills for crate::spawner::Spawner {
    fn held(&self) -> Result<Vec<HeldSkillEntry>, String> {
        self.room_skills_held()
    }

    fn put(&self, name: &str, digest: &str, files: &[(String, Vec<u8>)]) -> Result<(), String> {
        self.room_skill_put(name, digest, files)
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        self.room_skill_remove(name)
    }
}

/// A skill as the Room is given it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Wanted {
    /// Its name.
    pub name: String,
    /// The digest the owner enabled.
    pub digest: String,
    /// Its files but its program: each one's path, and its verified bytes.
    pub files: Vec<(String, Vec<u8>)>,
}

/// What a refresh did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Refreshed {
    /// Skills put in.
    pub put: usize,
    /// Skills taken out.
    pub removed: usize,
    /// Enabled skills left out: a name or a path the Room can't hold, or
    /// a file that no longer matches its signed hash.
    pub left_out: usize,
}

/// The skills the owner enabled that `set` runs, as the Room is given
/// them, and how many were left out.
pub(crate) fn wanted(set: &SkillSet) -> (Vec<Wanted>, usize) {
    let mut wanted = Vec::new();
    let mut left_out = 0usize;
    for listing in set.listing() {
        let id = SkillId::new(&listing.name, &listing.version);
        let (Some(registered), Some(dir)) = (set.registry().get(&id), set.package(&id)) else {
            continue;
        };
        match copy(listing.name.as_str(), &listing.digest, registered, dir) {
            Some(skill) => wanted.push(skill),
            None => left_out = left_out.saturating_add(1),
        }
    }
    (wanted, left_out)
}

/// One enabled skill as the Room is given it: `None` when the Room can't
/// hold it.
fn copy(
    name: &str,
    digest: &str,
    registered: &hatchling_skill::RegisteredSkill,
    dir: &std::path::Path,
) -> Option<Wanted> {
    if !is_name(name) {
        tracing::warn!(skill = ?name, "a skill's name is one the Room can't hold: left out of it");
        return None;
    }
    let manifest = registered.admission().manifest();
    let program = manifest.entry_point();
    let mut files = Vec::new();
    let mut bytes = 0usize;
    for entry in &manifest.files {
        if Some(entry.path.as_str()) == program {
            continue;
        }
        if !is_path(&entry.path) {
            tracing::warn!(skill = %name, path = ?entry.path, "a skill's path is one the Room can't hold: left out of it");
            return None;
        }
        let read = read_skill_file(dir, &entry.path, name, "listed file")
            .ok()
            .filter(|read| entry.check(name, read).is_ok());
        let Some(read) = read else {
            tracing::error!(skill = %name, path = %entry.path, "a skill's file isn't as signed: left out of the Room");
            return None;
        };
        bytes = bytes.saturating_add(read.len());
        files.push((entry.path.clone(), read));
    }
    if bytes > SKILL_BYTES {
        tracing::warn!(skill = %name, bytes, "a skill is larger than the Room holds: left out of it");
        return None;
    }
    Some(Wanted {
        name: name.to_owned(),
        digest: digest.to_owned(),
        files,
    })
}

/// Brings the Room's skills in line with `wanted`, through `room`.
///
/// # Errors
///
/// The first thing that failed: what the Room held, a removal, or a put.
pub(crate) fn refresh(
    room: &dyn RoomSkills,
    wanted: &[Wanted],
    left_out: usize,
) -> Result<Refreshed, String> {
    let held = room.held()?;
    let mut done = Refreshed {
        left_out,
        ..Refreshed::default()
    };
    for skill in &held {
        let kept = wanted
            .iter()
            .any(|want| want.name == skill.name && want.digest == skill.digest);
        if !kept {
            room.remove(&skill.name)?;
            done.removed = done.removed.saturating_add(1);
        }
    }
    for want in wanted {
        let has = held
            .iter()
            .any(|skill| skill.name == want.name && skill.digest == want.digest);
        if !has {
            room.put(&want.name, &want.digest, &want.files)?;
            done.put = done.put.saturating_add(1);
        }
    }
    Ok(done)
}

/// At a chat run's start: when it has a Room, brings the Room's skills in
/// line with what the owner enabled. Never stops the run.
pub(crate) fn before_chat_run() {
    if !crate::room_ops::offered(true, crate::room_ops::open) {
        return;
    }
    let (Some(spawner), Some(skills)) = (crate::spawner::spawner(), crate::skills::live()) else {
        return;
    };
    refresh_from(spawner, skills);
}

/// Brings `room`'s skills in line with what `skills` runs now, and says
/// what it did in the log.
pub(crate) fn refresh_from(room: &dyn RoomSkills, skills: &Skills) {
    let (wanted, left_out) = wanted(&skills.snapshot());
    match refresh(room, &wanted, left_out) {
        Ok(done) => tracing::info!(
            put = done.put,
            removed = done.removed,
            left_out = done.left_out,
            "the Room's skills are in line with what the owner enabled"
        ),
        Err(reason) => {
            tracing::warn!(%reason, "the Room's skills weren't brought in line: it keeps what it had");
        }
    }
}

#[cfg(test)]
#[path = "skill_room/tests.rs"]
mod tests;
