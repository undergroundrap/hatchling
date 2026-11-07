//! The approved-tools list (WO-019): the programs a harness's CLI cell may
//! run, each with the SHA-256 it was approved with, and when.
//!
//! The list names what a CLI cell runs, with its vendor's network. Anyone
//! who could change it could approve a program of their own for the
//! agent's coding CLI to run. So it's `approved.json` in a directory
//! beside the vault's, `<vault dir>.tools/`, both the user's the broker
//! starts as, and writable by no one else: one that isn't is refused, at
//! the broker's start and by every command. Run as root, that user is
//! root, not the broker's own: a broker that has dropped to
//! `hatchling-broker` can't change what it will run after a restart.
//!
//! The broker makes the list at its first start with a networked harness,
//! of the tools it has: `HATCHLING_HARNESS_TOOLS`'s, or the defaults this
//! host has ([`crate::harness::Harnesses::pin_tools`]). The operator
//! changes it with `tools add <path>` and `tools remove <path>`, and reads
//! it with `tools list`; a change takes effect at the broker's next start,
//! when its tools are pinned and copied.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::harness::{Seen, Tool, parse_tools, read_tools};
use crate::{Config, Error};

/// The list's file, in its directory.
pub(crate) const FILE: &str = "approved.json";

/// The file a new list is written to before it replaces the list.
const NEXT: &str = ".approved.json.next";

/// The list's format.
const VERSION: u32 = 1;

/// The most the list's file may hold.
const MAX_LIST_BYTES: u64 = 1024 * 1024;

/// The most tools the list may name.
const MAX_TOOLS: usize = 4096;

/// How `tools` is used.
pub const USAGE: &str = "usage: hatchling-broker tools <list | add <path> | remove <path>>";

/// The setting naming the tools before there's a list.
const TOOLS_KEY: &str = crate::environment::HATCHLING_HARNESS_TOOLS.name;

/// The list's directory: `<vault dir>.tools`.
#[must_use]
pub(crate) fn dir(config: &Config) -> PathBuf {
    let mut sibling = OsString::from(config.vault.root.as_os_str());
    sibling.push(".tools");
    PathBuf::from(sibling)
}

/// Seconds since the epoch: when a tool is approved.
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// One approved tool.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    tool: Tool,
    sha256: String,
    approved: u64,
}

/// The list as it's stored.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    tools: Vec<StoredEntry>,
}

/// One tool as it's stored.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEntry {
    path: String,
    sha256: String,
    approved: u64,
}

/// The approved tools, in the order they were approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Approved {
    entries: Vec<Entry>,
}

impl Approved {
    /// `tools`, each approved at `at` with the SHA-256 [`read_tools`]
    /// found, in `found`.
    ///
    /// # Errors
    ///
    /// A sentence for the operator: a tool can't be read.
    pub(crate) fn of(
        tools: &[Tool],
        found: &[Result<(String, Seen), String>],
        at: u64,
    ) -> Result<Self, String> {
        let entries = tools
            .iter()
            .zip(found)
            .map(|(tool, found)| {
                let (sha256, _) = found.as_ref().map_err(Clone::clone)?;
                Ok(Entry {
                    tool: tool.clone(),
                    sha256: sha256.clone(),
                    approved: at,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { entries })
    }

    /// The tools, in order.
    pub(crate) fn tools(&self) -> Vec<Tool> {
        self.entries
            .iter()
            .map(|entry| entry.tool.clone())
            .collect()
    }

    /// Each tool, and the SHA-256 it was approved with.
    pub(crate) fn pins(&self) -> Vec<(Tool, String)> {
        self.entries
            .iter()
            .map(|entry| (entry.tool.clone(), entry.sha256.clone()))
            .collect()
    }

    /// Whether `tools` are the same tools, in any order.
    pub(crate) fn names_the_same(&self, tools: &[Tool]) -> bool {
        names(self.entries.iter().map(|entry| &entry.tool)) == names(tools.iter())
    }

    /// The list `stored` holds.
    ///
    /// # Errors
    ///
    /// Why it isn't a list: another version, too many tools, a path that
    /// can't name a tool or is named twice, or a SHA-256 that isn't one.
    fn parse(stored: Stored) -> Result<Self, String> {
        if stored.version != VERSION {
            return Err(format!("version {} is not {VERSION}", stored.version));
        }
        if stored.tools.len() > MAX_TOOLS {
            return Err(format!("it names more than {MAX_TOOLS} tools"));
        }
        let mut entries: Vec<Entry> = Vec::new();
        for stored in stored.tools {
            let tool = Tool::at(&stored.path)?;
            if entries.iter().any(|entry| entry.tool == tool) {
                return Err(format!("{} is named twice", stored.path));
            }
            let hex = stored.sha256.len() == 64
                && stored
                    .sha256
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
            if !hex {
                return Err(format!("{}'s SHA-256 isn't one", stored.path));
            }
            entries.push(Entry {
                tool,
                sha256: stored.sha256,
                approved: stored.approved,
            });
        }
        Ok(Self { entries })
    }

    /// The list as it's stored.
    fn stored(&self) -> Stored {
        Stored {
            version: VERSION,
            tools: self
                .entries
                .iter()
                .map(|entry| StoredEntry {
                    path: format!("/{}", entry.tool.name()),
                    sha256: entry.sha256.clone(),
                    approved: entry.approved,
                })
                .collect(),
        }
    }

    /// The entry for `tool`.
    fn find(&mut self, tool: &Tool) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|entry| entry.tool == *tool)
    }
}

/// The names `tools` give their programs, in a cell.
fn names<'t>(tools: impl Iterator<Item = &'t Tool>) -> std::collections::BTreeSet<&'t str> {
    tools.map(Tool::name).collect()
}

const fn refused(reason: String) -> Error {
    Error::ApprovedTools { reason }
}

/// Whether a file or directory with owner `uid` and mode `mode` is
/// `euid`'s, and writable by no one else.
const fn only_theirs(uid: u32, mode: u32, euid: u32) -> bool {
    uid == euid && mode & 0o022 == 0
}

/// Checks `meta`, what `path` is, is `euid`'s and writable by no one
/// else.
fn check_owner(path: &Path, meta: &std::fs::Metadata, euid: u32) -> Result<(), Error> {
    if only_theirs(meta.uid(), meta.mode(), euid) {
        Ok(())
    } else {
        Err(refused(format!(
            "{} must be uid {euid}'s, and writable by no one else: a list another user can \
             change could approve their program",
            path.display()
        )))
    }
}

/// Whether there's a directory at `dir`, checked: a directory itself,
/// this process's user's, and writable by no one else.
fn checked_dir(dir: &Path) -> Result<bool, Error> {
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(refused(format!("{} is unreadable: {error}", dir.display()))),
    };
    if !meta.file_type().is_dir() {
        return Err(refused(format!("{} is not a directory", dir.display())));
    }
    check_owner(dir, &meta, hatchling_sys::euid())?;
    Ok(true)
}

/// The list in `dir`; none when there's none yet.
///
/// # Errors
///
/// [`Error::ApprovedTools`] when the directory or the file isn't this
/// process's user's alone, isn't what it should be, or can't be read, or
/// the file isn't a list.
pub(crate) fn read(dir: &Path) -> Result<Option<Approved>, Error> {
    use std::io::Read as _;
    if !checked_dir(dir)? {
        return Ok(None);
    }
    let path = dir.join(FILE);
    let unreadable =
        |error: std::io::Error| refused(format!("{} is unreadable: {error}", path.display()));
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(error)),
    };
    if !meta.file_type().is_file() {
        return Err(refused(format!("{} is not a regular file", path.display())));
    }
    check_owner(&path, &meta, hatchling_sys::euid())?;
    let file = std::fs::File::open(&path).map_err(unreadable)?;
    let opened = file.metadata().map_err(unreadable)?;
    if (opened.dev(), opened.ino()) != (meta.dev(), meta.ino()) {
        return Err(refused(format!(
            "{} changed as it was read",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_LIST_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() > usize::try_from(MAX_LIST_BYTES).unwrap_or(usize::MAX) {
        return Err(refused(format!(
            "{} is more than {MAX_LIST_BYTES} bytes",
            path.display()
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| error.to_string())
        .and_then(Approved::parse)
        .map(Some)
        .map_err(|reason| refused(format!("{} is not a list: {reason}", path.display())))
}

/// Writes `approved` as the list in `dir`, made `0700` if absent: whole,
/// or not at all.
///
/// # Errors
///
/// [`Error::ApprovedTools`] when the directory isn't this process's user's
/// alone, or the list can't be written.
pub(crate) fn write(dir: &Path, approved: &Approved) -> Result<(), Error> {
    if !checked_dir(dir)? {
        hatchling_sys::create_private_dir(dir)
            .map_err(|error| refused(format!("cannot make {}: {error}", dir.display())))?;
    }
    let path = dir.join(FILE);
    let next = dir.join(NEXT);
    let failed =
        |error: std::io::Error| refused(format!("cannot write {}: {error}", path.display()));
    let mut text = serde_json::to_string_pretty(&approved.stored())
        .map_err(|error| refused(format!("cannot write {}: {error}", path.display())))?;
    text.push('\n');
    // One a failed write left is this user's: the directory is.
    hatchling_sys::remove_file_of(
        &next,
        hatchling_sys::FileKind::Regular,
        &[hatchling_sys::euid()],
    )
    .map_err(|error| failed(std::io::Error::other(error)))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&next)
        .map_err(failed)?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(failed)?;
    std::fs::rename(&next, &path).map_err(failed)?;
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(failed)
}

/// Runs `tools <action>`, printing to stdout.
///
/// # Errors
///
/// As [`command`].
pub fn run_tools(config: &Config, args: impl IntoIterator<Item = String>) -> Result<(), Error> {
    let args: Vec<String> = args.into_iter().collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let named = std::env::var(TOOLS_KEY).ok();
    let text = command(&dir(config), named.as_deref(), &args, now())?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{text}").map_err(|e| refused(format!("cannot write the report: {e}")))
}

/// `tools` with `args`, on the list in `dir`, at `at`: the report.
/// `named` is `HATCHLING_HARNESS_TOOLS`, which names the tools a list made
/// now starts with, as the broker's start would make it.
///
/// # Errors
///
/// [`Error::Usage`] for an unknown action, [`Error::ApprovedTools`] for a
/// list that can't be read or written, a path that isn't a program to run,
/// or one to remove that isn't approved, and [`Error::InvalidConfig`] when
/// a list made now can't be.
pub(crate) fn command(
    dir: &Path,
    named: Option<&str>,
    args: &[&str],
    at: u64,
) -> Result<String, Error> {
    match args {
        ["list"] => list(dir),
        ["add", path] => add(dir, named, path, at),
        ["remove", path] => remove(dir, named, path, at),
        _ => Err(Error::Usage {
            reason: String::from(USAGE),
        }),
    }
}

/// The list in `dir`, or one made now as the broker's start would make
/// it, and what the report says about making it.
fn current(dir: &Path, named: Option<&str>, at: u64) -> Result<(Approved, Option<String>), Error> {
    if let Some(approved) = read(dir)? {
        return Ok((approved, None));
    }
    let tools = parse_tools(named, true)?;
    let approved =
        Approved::of(&tools, &read_tools(&tools), at).map_err(|reason| Error::InvalidConfig {
            key: TOOLS_KEY,
            reason,
        })?;
    let from = if named.is_some_and(|raw| !raw.trim().is_empty()) {
        TOOLS_KEY
    } else {
        "the defaults this host has"
    };
    let made = format!(
        "made the approved-tools list in {}, of {} tools from {from}",
        dir.display(),
        tools.len()
    );
    Ok((approved, Some(made)))
}

/// What the report says when `named`, `HATCHLING_HARNESS_TOOLS`, names
/// other tools than `approved`: the broker won't start while it does.
fn named_note(named: Option<&str>, approved: &Approved) -> Option<String> {
    let named = named.filter(|raw| !raw.trim().is_empty())?;
    let tools: Vec<Tool> = named
        .split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .filter_map(|path| Tool::at(path).ok())
        .collect();
    (!approved.names_the_same(&tools)).then(|| {
        format!(
            "{TOOLS_KEY} names other tools than the list: unset it, or the broker refuses to start"
        )
    })
}

fn add(dir: &Path, named: Option<&str>, path: &str, at: u64) -> Result<String, Error> {
    let not_a_program = |reason: String| {
        refused(format!(
            "{} is not a program to run: {reason}",
            path.escape_debug()
        ))
    };
    let tool = Tool::at(path).map_err(not_a_program)?;
    let (sha256, _) = read_tools(std::slice::from_ref(&tool))
        .pop()
        .unwrap_or_else(|| Err(String::from("it wasn't read")))
        .map_err(not_a_program)?;
    let (mut approved, made) = current(dir, named, at)?;
    let full = approved.entries.len() >= MAX_TOOLS;
    let said = match approved.find(&tool) {
        Some(entry) if entry.sha256 == sha256 => {
            format!("{path} is approved already, as this program (sha256 {sha256})")
        }
        Some(entry) => {
            let was = std::mem::replace(&mut entry.sha256, sha256.clone());
            entry.approved = at;
            format!(
                "approved {path} again: sha256 {sha256}, was {was}; CLI cells get it from the \
                 broker's next start"
            )
        }
        None if full => {
            return Err(refused(format!("the list names {MAX_TOOLS} tools already")));
        }
        None => {
            approved.entries.push(Entry {
                tool,
                sha256: sha256.clone(),
                approved: at,
            });
            format!(
                "approved {path}: sha256 {sha256}; CLI cells get it from the broker's next start"
            )
        }
    };
    write(dir, &approved)?;
    Ok(made
        .into_iter()
        .chain([said])
        .chain(named_note(named, &approved))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn remove(dir: &Path, named: Option<&str>, path: &str, at: u64) -> Result<String, Error> {
    let tool = Tool::at(path).map_err(refused)?;
    let (mut approved, made) = current(dir, named, at)?;
    let before = approved.entries.len();
    approved.entries.retain(|entry| entry.tool != tool);
    if approved.entries.len() == before {
        return Err(refused(format!("{path} isn't approved")));
    }
    write(dir, &approved)?;
    let said = format!("removed {path}; CLI cells stop getting it at the broker's next start");
    Ok(made
        .into_iter()
        .chain([said])
        .chain(named_note(named, &approved))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Each approved tool, its SHA-256 and when it was approved, and whether
/// it's still that program.
fn list(dir: &Path) -> Result<String, Error> {
    let Some(approved) = read(dir)? else {
        return Ok(format!(
            "no approved-tools list in {} yet: the broker makes it at its next start with a \
             harness's API set, of the tools {TOOLS_KEY} names or the defaults this host has",
            dir.display()
        ));
    };
    if approved.entries.is_empty() {
        return Ok(format!(
            "no approved tools in {}: CLI cells get none",
            dir.display()
        ));
    }
    let found = read_tools(&approved.tools());
    let mut lines = vec![format!(
        "{} approved tools in {} (a change takes effect at the broker's next start):",
        approved.entries.len(),
        dir.display()
    )];
    lines.extend(approved.entries.iter().zip(found).map(|(entry, found)| {
        let now = match found {
            Ok((actual, _)) if actual == entry.sha256 => String::new(),
            Ok((actual, _)) => format!("  CHANGED: sha256 now {actual}"),
            Err(reason) => format!("  UNREADABLE: {reason}"),
        };
        format!(
            "/{}  sha256 {}  approved {}{now}",
            entry.tool.name(),
            entry.sha256,
            when(entry.approved)
        )
    }));
    Ok(lines.join("\n"))
}

/// A time, as a UTC timestamp.
fn when(at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(|at| hatchling_scheduler::Timestamp::from_second(at).ok())
        .map_or_else(|| at.to_string(), |at| at.to_string())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use hatchling_skill::CodeHash;

    use super::*;
    use crate::api_auth::tests::{Scratch, scratch};

    /// A program at `name` in `scratch`, holding `bytes`.
    fn program(scratch: &Scratch, name: &str, bytes: &[u8]) -> String {
        let path = scratch.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path.display().to_string()
    }

    fn sha(bytes: &[u8]) -> String {
        CodeHash::of(bytes).to_string()
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn chmod(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Why `result` was refused.
    fn refusal(result: Result<impl std::fmt::Debug, Error>) -> String {
        let error = result.unwrap_err();
        if let Error::ApprovedTools { reason } = error {
            reason
        } else {
            format!("not an approved-tools refusal: {error}")
        }
    }

    /// Only the user's own, and writable by no one else.
    #[test]
    fn only_its_user_may_write_the_list() {
        assert!(only_theirs(1000, 0o600, 1000));
        assert!(only_theirs(1000, 0o755, 1000));
        assert!(only_theirs(0, 0o700, 0));
        assert!(!only_theirs(1000, 0o620, 1000), "group");
        assert!(!only_theirs(1000, 0o602, 1000), "others");
        assert!(!only_theirs(1001, 0o600, 1000), "another user's");
        assert!(!only_theirs(0, 0o600, 1000), "root's, to another user");
    }

    /// The list is written whole, owner-only in an owner-only directory,
    /// and read back as it was.
    #[test]
    fn the_list_is_written_whole_and_read_back() {
        let scratch = scratch("tools-written");
        let dir = scratch.0.join("vault.tools");
        assert_eq!(read(&dir).unwrap(), None, "no directory");
        let first = program(&scratch, "first", b"one");
        let tools = [Tool::at(&first).unwrap()];
        let approved = Approved::of(&tools, &read_tools(&tools), 7).unwrap();
        write(&dir, &approved).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(FILE)), 0o600);
        assert!(!dir.join(NEXT).exists());
        assert_eq!(read(&dir).unwrap(), Some(approved.clone()));
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            serde_json::json!({
                "version": 1,
                "tools": [{"path": first, "sha256": sha(b"one"), "approved": 7}],
            })
        );
        // A write a crash left half-done is replaced.
        std::fs::write(dir.join(NEXT), b"half").unwrap();
        write(&dir, &approved).unwrap();
        assert_eq!(read(&dir).unwrap(), Some(approved));
        // A directory with no list holds none.
        std::fs::remove_file(dir.join(FILE)).unwrap();
        assert_eq!(read(&dir).unwrap(), None);
        // A tool that can't be read can't be approved.
        let gone = [Tool::at(&scratch.0.join("gone").display().to_string()).unwrap()];
        assert!(
            Approved::of(&gone, &read_tools(&gone), 7)
                .unwrap_err()
                .contains("is unreadable")
        );
    }

    /// WO-019: a list, or its directory, that another user could write,
    /// or that isn't what it should be, is refused; so is one that isn't a
    /// list.
    #[test]
    fn a_list_open_to_another_user_or_not_a_list_is_refused() {
        let scratch = scratch("tools-refused");
        let dir = scratch.0.join("vault.tools");
        let tool = program(&scratch, "tool", b"tool");
        let tools = [Tool::at(&tool).unwrap()];
        write(&dir, &Approved::of(&tools, &read_tools(&tools), 1).unwrap()).unwrap();
        let file = dir.join(FILE);
        for (path, open) in [(&file, 0o620), (&file, 0o606), (&dir, 0o770), (&dir, 0o707)] {
            let was = mode(path);
            chmod(path, open);
            assert!(
                refusal(read(&dir)).starts_with(&format!(
                    "{} must be uid {}'s, and writable by no one else",
                    path.display(),
                    hatchling_sys::euid()
                )),
                "{} at {open:o}",
                path.display()
            );
            if path == &dir {
                let none = Approved {
                    entries: Vec::new(),
                };
                assert!(refusal(write(&dir, &none)).contains("writable by no one else"));
            }
            chmod(path, was);
        }
        assert!(read(&dir).unwrap().is_some());
        // Never through a link: the file's, or the directory's.
        let elsewhere = scratch.0.join("elsewhere.json");
        std::fs::rename(&file, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &file).unwrap();
        assert!(refusal(read(&dir)).ends_with("is not a regular file"));
        std::fs::remove_file(&file).unwrap();
        std::fs::rename(&elsewhere, &file).unwrap();
        let linked = scratch.0.join("linked.tools");
        std::os::unix::fs::symlink(&dir, &linked).unwrap();
        assert!(refusal(read(&linked)).ends_with("is not a directory"));
        // Another user's directory: `/` is root's.
        if hatchling_sys::euid() != 0 {
            assert!(refusal(read(Path::new("/"))).contains("must be uid"));
        }
        // Not a list.
        let entry = |path: &str, sha256: &str| serde_json::json!({"path": path, "sha256": sha256, "approved": 1});
        let good = sha(b"tool");
        let list =
            |tools: Vec<serde_json::Value>| serde_json::json!({"version": 1, "tools": tools});
        for (list, why) in [
            (
                serde_json::json!({"version": 2, "tools": []}),
                "version 2 is not 1",
            ),
            (
                list(vec![entry("relative", &good)]),
                "relative is not an absolute path",
            ),
            (
                list(vec![entry(&tool, &good), entry(&tool, &good)]),
                "is named twice",
            ),
            (
                list(vec![entry(&tool, &good.to_uppercase())]),
                "SHA-256 isn't one",
            ),
            (list(vec![entry(&tool, "abc")]), "SHA-256 isn't one"),
            (
                serde_json::json!({"version": 1, "tools": [], "more": true}),
                "unknown field",
            ),
        ] {
            std::fs::write(&file, list.to_string()).unwrap();
            let reason = refusal(read(&dir));
            assert!(
                reason.starts_with(&format!("{} is not a list: ", file.display()))
                    && reason.contains(why),
                "{why}: {reason}"
            );
        }
        let many = (0..=MAX_TOOLS)
            .map(|n| entry(&format!("/bin/t{n}"), &good))
            .collect();
        std::fs::write(&file, list(many).to_string()).unwrap();
        assert!(refusal(read(&dir)).contains(&format!("more than {MAX_TOOLS} tools")));
        std::fs::write(&file, vec![b' '; 1024 * 1024 + 1]).unwrap();
        assert!(refusal(read(&dir)).ends_with(&format!("is more than {MAX_LIST_BYTES} bytes")));
    }

    /// WO-019: `tools add` records a tool and its hash, `list` shows each
    /// and when it was approved, and whether it's still that program, and
    /// `remove` takes it away. The first command makes the list as the
    /// broker's start would, of the tools `HATCHLING_HARNESS_TOOLS` names.
    #[test]
    fn the_operator_adds_lists_and_removes_tools() {
        let scratch = scratch("tools-command");
        let dir = scratch.0.join("vault.tools");
        let named = program(&scratch, "named", b"named");
        let added = program(&scratch, "added", b"added");
        let run = |args: &[&str], at: u64| command(&dir, Some(&named), args, at);
        let note = format!(
            "{TOOLS_KEY} names other tools than the list: unset it, or the broker refuses to start"
        );
        assert!(
            run(&["list"], 1)
                .unwrap()
                .starts_with(&format!("no approved-tools list in {} yet", dir.display()))
        );
        assert_eq!(
            run(&["add", &added], 1_800_000_000).unwrap(),
            format!(
                "made the approved-tools list in {}, of 1 tools from {TOOLS_KEY}\n\
                 approved {added}: sha256 {}; CLI cells get it from the broker's next start\n\
                 {note}",
                dir.display(),
                sha(b"added")
            )
        );
        let pins = |dir: &Path| read(dir).unwrap().unwrap().pins();
        assert_eq!(
            pins(&dir),
            [
                (Tool::at(&named).unwrap(), sha(b"named")),
                (Tool::at(&added).unwrap(), sha(b"added"))
            ]
        );
        assert_eq!(
            run(&["add", &added], 2).unwrap(),
            format!(
                "{added} is approved already, as this program (sha256 {})\n{note}",
                sha(b"added")
            )
        );
        std::fs::write(&added, b"upgraded").unwrap();
        assert_eq!(
            run(&["list"], 3).unwrap(),
            format!(
                "2 approved tools in {} (a change takes effect at the broker's next start):\n\
                 {named}  sha256 {}  approved 2027-01-15T08:00:00Z\n\
                 {added}  sha256 {}  approved 2027-01-15T08:00:00Z  CHANGED: sha256 now {}",
                dir.display(),
                sha(b"named"),
                sha(b"added"),
                sha(b"upgraded")
            )
        );
        assert_eq!(
            command(&dir, None, &["add", &added], 4).unwrap(),
            format!(
                "approved {added} again: sha256 {}, was {}; CLI cells get it from the broker's \
                 next start",
                sha(b"upgraded"),
                sha(b"added")
            )
        );
        std::fs::remove_file(&named).unwrap();
        let listed = run(&["list"], 5).unwrap();
        assert!(
            listed.contains(&format!(
                "{added}  sha256 {}  approved 1970-01-01T00:00:04Z\n",
                sha(b"upgraded")
            )) || listed.ends_with(&format!(
                "{added}  sha256 {}  approved 1970-01-01T00:00:04Z",
                sha(b"upgraded")
            )),
            "{listed}"
        );
        assert!(
            listed.contains(&format!(
                "{named}  sha256 {}  approved 2027-01-15T08:00:00Z  UNREADABLE: {named} is unreadable",
                sha(b"named")
            )),
            "{listed}"
        );
        // Removed: the list is the tools the setting names again.
        assert_eq!(
            run(&["remove", &added], 6).unwrap(),
            format!("removed {added}; CLI cells stop getting it at the broker's next start")
        );
        assert_eq!(pins(&dir), [(Tool::at(&named).unwrap(), sha(b"named"))]);
        assert_eq!(
            refusal(run(&["remove", &added], 7)),
            format!("{added} isn't approved")
        );
        assert_eq!(
            command(&dir, None, &["remove", &named], 8).unwrap(),
            format!("removed {named}; CLI cells stop getting it at the broker's next start")
        );
        assert_eq!(
            run(&["list"], 9).unwrap(),
            format!("no approved tools in {}: CLI cells get none", dir.display())
        );
        assert!(read(&dir).unwrap().unwrap().tools().is_empty());
    }

    /// What isn't a program to run is never approved, and a command that
    /// isn't one is a usage error.
    #[test]
    fn what_isnt_a_program_is_never_approved() {
        let scratch = scratch("tools-not-programs");
        let dir = scratch.0.join("vault.tools");
        let added = program(&scratch, "added", b"added");
        let run = |args: &[&str]| command(&dir, None, args, 1);
        for (path, why) in [
            (String::from("relative"), "relative is not an absolute path"),
            (scratch.0.display().to_string(), "is not a regular file"),
            (format!("{added}\u{1b}]0;x"), "holds a control character"),
        ] {
            let reason = refusal(run(&["add", &path]));
            assert!(
                reason.starts_with(&format!(
                    "{} is not a program to run: ",
                    path.escape_debug()
                )) && reason.contains(why),
                "{reason}"
            );
        }
        assert_eq!(read(&dir).unwrap(), None, "nothing made");
        assert!(matches!(
            run(&["drop", &added]).unwrap_err(),
            Error::Usage { reason } if reason == USAGE
        ));
    }

    /// With no list, the first command makes it of the defaults this host
    /// has, as the broker's start would.
    #[test]
    fn a_first_command_approves_the_default_tools() {
        let scratch = scratch("tools-first");
        let dir = scratch.0.join("vault.tools");
        let tool = program(&scratch, "tool", b"tool");
        let said = command(&dir, None, &["add", &tool], 1).unwrap();
        let defaults = parse_tools(None, true).unwrap();
        assert!(
            said.starts_with(&format!(
                "made the approved-tools list in {}, of {} tools from the defaults this host has\n",
                dir.display(),
                defaults.len()
            )),
            "{said}"
        );
        let mut expected = defaults;
        expected.push(Tool::at(&tool).unwrap());
        assert_eq!(read(&dir).unwrap().unwrap().tools(), expected);
    }

    /// Each part of the list, on its own: the tools it names, in any
    /// order; its stored form, read back; the checks on its file and
    /// directory; and the note when the setting names other tools.
    #[test]
    fn each_part_of_the_list_answers_for_itself() {
        let scratch = scratch("tools-parts");
        let one = Tool::at("/bin/one").unwrap();
        let two = Tool::at("/bin/two").unwrap();
        let approved = Approved {
            entries: vec![
                Entry {
                    tool: one.clone(),
                    sha256: sha(b"one"),
                    approved: 1,
                },
                Entry {
                    tool: two.clone(),
                    sha256: sha(b"two"),
                    approved: 2,
                },
            ],
        };
        assert!(approved.names_the_same(&[two.clone(), one.clone()]));
        assert!(!approved.names_the_same(std::slice::from_ref(&one)));
        assert!(!approved.names_the_same(&[one, two, Tool::at("/bin/three").unwrap()]));
        assert_eq!(Approved::parse(approved.stored()), Ok(approved.clone()));
        let mut stored = approved.stored();
        stored.version = 0;
        assert_eq!(
            Approved::parse(stored),
            Err(String::from("version 0 is not 1"))
        );
        // The setting's note: only when it's set and names other tools.
        assert_eq!(named_note(None, &approved), None);
        assert_eq!(named_note(Some(" "), &approved), None);
        assert_eq!(named_note(Some("/bin/two, /bin/one"), &approved), None);
        assert_eq!(
            named_note(Some("/bin/one"), &approved),
            Some(format!(
                "{TOOLS_KEY} names other tools than the list: unset it, or the broker refuses to start"
            ))
        );
        // The checks on what's at a path.
        let euid = hatchling_sys::euid();
        let file = scratch.0.join("file");
        std::fs::write(&file, b"").unwrap();
        chmod(&file, 0o600);
        let meta = |path: &Path| std::fs::symlink_metadata(path).unwrap();
        assert!(check_owner(&file, &meta(&file), euid).is_ok());
        assert!(
            refusal(check_owner(&file, &meta(&file), euid.wrapping_add(1))).contains("must be uid")
        );
        chmod(&file, 0o660);
        assert!(
            refusal(check_owner(&file, &meta(&file), euid)).contains("writable by no one else")
        );
        assert!(!checked_dir(&scratch.0.join("missing")).unwrap());
        assert!(checked_dir(&scratch.0).unwrap());
        assert!(refusal(checked_dir(&file)).ends_with("is not a directory"));
    }

    #[test]
    fn a_time_is_a_utc_timestamp() {
        assert_eq!(when(0), "1970-01-01T00:00:00Z");
        assert_eq!(when(u64::MAX), u64::MAX.to_string());
    }
}
