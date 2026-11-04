//! A bounded log the broker keeps for the owner.
//!
//! One JSON record per line in one file, `0600`, newest last. Past
//! [`MAX_LINES`] lines it's cut back to the newest [`MAX_KEPT`], through a
//! sibling `.next` file renamed over it. The owner's decisions
//! ([`crate::decisions`]) and their Feed page's posts
//! ([`crate::feed_page`]) are kept this way.

use std::fs::OpenOptions;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// How many records are kept, and listed: the newest.
pub const MAX_KEPT: usize = 200;

/// How many lines a file may grow to before it's cut back to
/// [`MAX_KEPT`].
pub const MAX_LINES: usize = 2 * MAX_KEPT;

/// The largest file read.
pub const MAX_FILE_BYTES: u64 = 8 << 20;

/// Serializes the broker's writers.
static WRITES: Mutex<()> = Mutex::new(());

/// Appends `record` to the log at `path`, cutting it back if it's grown
/// past [`MAX_LINES`].
///
/// # Errors
///
/// Returns the error if the record can't be written, or the log can't be
/// read back or cut.
pub fn append<T: Serialize>(path: &Path, record: &T) -> std::io::Result<()> {
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?
        .write_all(line.as_bytes())?;
    let lines = lines(path)?;
    if lines.len() > MAX_LINES {
        let kept = lines.get(lines.len() - MAX_KEPT..).unwrap_or_default();
        let next = next_path(path);
        let mut text = kept.join("\n");
        text.push('\n');
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&next)?
            .write_all(text.as_bytes())?;
        std::fs::rename(&next, path)?;
    }
    Ok(())
}

/// Where a cut-back log is written before it's renamed over `path`.
#[must_use]
pub fn next_path(path: &Path) -> PathBuf {
    let extension = path
        .extension()
        .map_or_else(String::new, |extension| {
            format!("{}.", extension.to_string_lossy())
        });
    path.with_extension(format!("{extension}next"))
}

/// The file's lines, if it's small enough to read.
fn lines(path: &Path) -> std::io::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_FILE_BYTES {
        return Err(std::io::Error::other("the log is too large to read"));
    }
    BufReader::new(file).lines().collect()
}

/// The records kept at `path`, the newest first; none when there's no
/// file or it can't be read. A line that doesn't read is skipped.
#[must_use]
pub fn list<T: DeserializeOwned>(path: &Path) -> Vec<T> {
    let Ok(lines) = lines(path) else {
        return Vec::new();
    };
    lines
        .iter()
        .rev()
        .filter_map(|line| serde_json::from_str(line).ok())
        .take(MAX_KEPT)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-kept-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn records_are_kept_and_listed_newest_first() {
        let dir = scratch("kept");
        let path = dir.join("schedules.feed");
        assert!(list::<u32>(&path).is_empty(), "no file: none");
        append(&path, &1_u32).unwrap();
        append(&path, &2_u32).unwrap();
        assert_eq!(list::<u32>(&path), [2, 1]);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // A line that doesn't read is skipped.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("not a number\n");
        std::fs::write(&path, text).unwrap();
        assert_eq!(list::<u32>(&path), [2, 1]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_log_is_bounded_to_the_newest() {
        let dir = scratch("bounded");
        let path = dir.join("schedules.feed");
        for n in 0..=MAX_LINES {
            append(&path, &n).unwrap();
        }
        let listed = list::<usize>(&path);
        assert_eq!(listed.len(), MAX_KEPT);
        assert_eq!(listed.first(), Some(&MAX_LINES));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            MAX_KEPT,
            "cut back past {MAX_LINES} lines"
        );
        assert!(!next_path(&path).exists(), "nothing left over");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "the cut-back file is the owner's alone too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_next_file_sits_beside_the_log() {
        assert_eq!(
            next_path(Path::new("/var/lib/hatchling/schedules.decisions")),
            Path::new("/var/lib/hatchling/schedules.decisions.next")
        );
        assert_eq!(
            next_path(Path::new("/var/lib/hatchling/feed")),
            Path::new("/var/lib/hatchling/feed.next")
        );
    }

    #[test]
    fn a_log_that_cant_be_written_or_read_keeps_nothing() {
        let dir = scratch("broken");
        let path = dir.join("missing").join("schedules.feed");
        assert!(append(&path, &1_u32).is_err());
        assert!(list::<u32>(&path).is_empty());
        let big = dir.join("big.feed");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(list::<u32>(&big).is_empty());
        assert!(append(&big, &1_u32).is_err(), "too large to read back");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
